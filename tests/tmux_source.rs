use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use tmux_rescue::{
    CaptureSource, LinuxProcessInspector, PaneProcessObservation, SnapshotSource, TmuxAdapter,
    TmuxSelector, ValidatedSnapshot,
};

static SOURCE_COMMAND_TEST: Mutex<()> = Mutex::new(());

const APPROVED_CODEX_FOOTER: &str = "  gpt-5.6-sol ultra · ~/projects/tmux-rescue · main · Context 78% used · 258K window · Fast on · Approve for me · 2.55M used · Main…";

struct ProcessContextGuard {
    directory: PathBuf,
    path: Option<OsString>,
    tmux: Option<OsString>,
    log: Option<OsString>,
    fail: Option<OsString>,
    scenario: Option<OsString>,
    state: Option<OsString>,
}

impl Drop for ProcessContextGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.directory).unwrap();
        for (key, value) in [
            ("PATH", &self.path),
            ("TMUX", &self.tmux),
            ("FAKE_TMUX_LOG", &self.log),
            ("FAKE_TMUX_FAIL", &self.fail),
            ("FAKE_TMUX_SCENARIO", &self.scenario),
            ("FAKE_TMUX_STATE", &self.state),
        ] {
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

fn install_fake_tmux(temp: &Path, directory: &Path, fail: bool) -> (PathBuf, ProcessContextGuard) {
    let bin = temp.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let tmux = bin.join("tmux");
    std::fs::write(
        &tmux,
        b"#!/bin/sh\n{ printf 'BEGIN\\nPWD=%s\\nTMUX=%s\\n' \"$PWD\" \"${TMUX-unset}\"; for arg do printf 'ARG=%s\\n' \"$arg\"; done; } >> \"$FAKE_TMUX_LOG\"\n[ \"${FAKE_TMUX_FAIL-}\" = 1 ] && { printf 'selection failed\\n' >&2; exit 1; }\ncase \" $* \" in\n  *' #{n:socket_path}:#{socket_path} '*) printf '21:/reported/source.sock\\n' ;;\n  *' list-panes '*) printf '4:work1:01:04:/tmp6:editor4:/tmp1:19:/dev/null0:7:/bin/sh3:%%15\\n' ;;\n  *) printf 'unexpected fake tmux command\\n' >&2; exit 2 ;;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o700)).unwrap();
    let log = temp.join("tmux.log");
    let old_path = std::env::var_os("PATH");
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        old_path.as_deref().unwrap_or(OsStr::new("")),
    )))
    .unwrap();
    let guard = ProcessContextGuard {
        directory: std::env::current_dir().unwrap(),
        path: old_path,
        tmux: std::env::var_os("TMUX"),
        log: std::env::var_os("FAKE_TMUX_LOG"),
        fail: std::env::var_os("FAKE_TMUX_FAIL"),
        scenario: std::env::var_os("FAKE_TMUX_SCENARIO"),
        state: std::env::var_os("FAKE_TMUX_STATE"),
    };
    std::env::set_current_dir(directory).unwrap();
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("TMUX", "ambient.sock,1,0");
        std::env::set_var("FAKE_TMUX_LOG", &log);
        if fail {
            std::env::set_var("FAKE_TMUX_FAIL", "1")
        } else {
            std::env::remove_var("FAKE_TMUX_FAIL")
        }
        std::env::remove_var("FAKE_TMUX_SCENARIO");
        std::env::remove_var("FAKE_TMUX_STATE");
    }
    (log, guard)
}

fn install_visible_grid_fake_tmux(
    temp: &Path,
    directory: &Path,
    scenario: &str,
) -> (PathBuf, ProcessContextGuard) {
    let bin = temp.join("bin");
    fs::create_dir(&bin).unwrap();
    let tmux = bin.join("tmux");
    fs::write(
        &tmux,
        br#"#!/bin/sh
{ printf 'BEGIN\nPWD=%s\nTMUX=%s\n' "$PWD" "${TMUX-unset}"; for arg do printf 'ARG=%s\n' "$arg"; done; } >> "$FAKE_TMUX_LOG"
case " $* " in
  *' list-panes '*) printf '4:work1:01:04:/tmp6:editor4:/tmp1:19:/dev/null0:7:/bin/sh3:%%15\n' ;;
  *' display-message -p -t %15 '*)
    if [ "$FAKE_TMUX_SCENARIO" = wrong_mode ]; then
      printf '3:%%152:801:41:81:14:true\n'
    elif [ -e "$FAKE_TMUX_STATE" ]; then
      if [ "$FAKE_TMUX_SCENARIO" = changed ]; then
        printf '3:%%152:801:41:91:11:0\n'
      else
        printf '3:%%152:801:41:81:11:0\n'
      fi
    else
      : > "$FAKE_TMUX_STATE"
      printf '3:%%152:801:41:81:11:0\n'
    fi ;;
  *' capture-pane -p -e -t %15 '*)
    if [ "$FAKE_TMUX_SCENARIO" = wrong_rows ]; then
      printf '\302\273 private source row\n  second row\n  95%% context left\n'
    else
      printf '\302\273 \033[2mdraft\033[0m\n  second\n\n  95%% context left\n'
    fi ;;
  *) printf 'unexpected fake tmux command\n' >&2; exit 2 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
    let log = temp.join("tmux.log");
    let state = temp.join("metadata-state");
    let old_path = std::env::var_os("PATH");
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        old_path.as_deref().unwrap_or(OsStr::new("")),
    )))
    .unwrap();
    let guard = ProcessContextGuard {
        directory: std::env::current_dir().unwrap(),
        path: old_path,
        tmux: std::env::var_os("TMUX"),
        log: std::env::var_os("FAKE_TMUX_LOG"),
        fail: std::env::var_os("FAKE_TMUX_FAIL"),
        scenario: std::env::var_os("FAKE_TMUX_SCENARIO"),
        state: std::env::var_os("FAKE_TMUX_STATE"),
    };
    std::env::set_current_dir(directory).unwrap();
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("TMUX", "ambient.sock,1,0");
        std::env::set_var("FAKE_TMUX_LOG", &log);
        std::env::remove_var("FAKE_TMUX_FAIL");
        std::env::set_var("FAKE_TMUX_SCENARIO", scenario);
        std::env::set_var("FAKE_TMUX_STATE", state);
    }
    (log, guard)
}

struct TemporaryTmuxServer {
    selector: TmuxSelector,
    tmux_tmpdir: Option<PathBuf>,
}

impl TemporaryTmuxServer {
    fn start(socket: &Path, session_cwd: &Path) -> Self {
        Self::start_selected(
            TmuxSelector::SocketPath(socket.as_os_str().to_owned()),
            None,
            session_cwd,
        )
    }

    fn start_selected(
        selector: TmuxSelector,
        tmux_tmpdir: Option<&Path>,
        session_cwd: &Path,
    ) -> Self {
        let mut command = selected_tmux(&selector, tmux_tmpdir, false);
        let output = command
            .args(["-f", "/dev/null", "new-session", "-d", "-s", "work", "-n"])
            .arg("editor: main")
            .args(["-c"])
            .arg(session_cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "failed to start isolated tmux: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self {
            selector,
            tmux_tmpdir: tmux_tmpdir.map(Path::to_owned),
        }
    }

    fn run(&self, arguments: &[&str]) {
        let output = selected_tmux(&self.selector, self.tmux_tmpdir.as_deref(), true)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated tmux command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn start_with_explicit_shell(socket: &Path, session_cwd: &Path) -> Self {
        let selector = TmuxSelector::SocketPath(socket.as_os_str().to_owned());
        let output = selected_tmux(&selector, None, false)
            .args(["-f", "/dev/null", "new-session", "-d", "-s", "work", "-c"])
            .arg(session_cwd)
            .args(["/bin/sh", "-i"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "failed to start isolated tmux with explicit shell: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Self {
            selector,
            tmux_tmpdir: None,
        }
    }

    fn reported_socket_path(&self) -> PathBuf {
        let output = selected_tmux(&self.selector, self.tmux_tmpdir.as_deref(), true)
            .args(["display-message", "-p", "#{socket_path}"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "failed to read selected socket path: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
    }
}

impl Drop for TemporaryTmuxServer {
    fn drop(&mut self) {
        let _ = selected_tmux(&self.selector, self.tmux_tmpdir.as_deref(), true)
            .arg("kill-server")
            .status();
    }
}

fn selected_tmux(selector: &TmuxSelector, tmux_tmpdir: Option<&Path>, no_start: bool) -> Command {
    let mut command = Command::new("tmux");
    command.arg("-u");
    if no_start {
        command.arg("-N");
    }
    command.arg(selector.flag()).arg(selector.value());
    command.env_remove("TMUX").env_remove("TMUX_PANE");
    if let Some(tmux_tmpdir) = tmux_tmpdir {
        command.env("TMUX_TMPDIR", tmux_tmpdir);
    } else {
        command.env_remove("TMUX_TMPDIR");
    }
    command
}

fn capture_selected_source(
    selector: &TmuxSelector,
    tmux_tmpdir: Option<&Path>,
    state_home: &Path,
) -> PathBuf {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tmux-rescue"));
    command
        .arg(selector.flag())
        .arg(selector.value())
        .arg("snapshot")
        .env("XDG_STATE_HOME", state_home)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    if let Some(tmux_tmpdir) = tmux_tmpdir {
        command.env("TMUX_TMPDIR", tmux_tmpdir);
    } else {
        command.env_remove("TMUX_TMPDIR");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "snapshot failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("snapshot: ").map(PathBuf::from))
        .expect("snapshot output omitted its immutable path")
}

#[test]
fn reads_an_isolated_source_server_into_a_validated_topology() {
    let _serial = SOURCE_COMMAND_TEST.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let session_cwd = temp.path().join("session");
    let second_pane_cwd = temp.path().join("second-pane");
    std::fs::create_dir(&session_cwd).unwrap();
    std::fs::create_dir(&second_pane_cwd).unwrap();
    let socket = temp.path().join("source.sock");
    let server = TemporaryTmuxServer::start(&socket, &session_cwd);
    server.run(&[
        "split-window",
        "-d",
        "-t",
        "work:0",
        "-c",
        second_pane_cwd.to_str().unwrap(),
    ]);

    let source =
        SnapshotSource::try_from_bytes(socket.as_os_str().as_encoded_bytes().to_vec()).unwrap();
    let mut adapter = TmuxAdapter::new(source, LinuxProcessInspector::new());
    let topology = adapter.read_topology().unwrap();

    assert_eq!(topology.sessions().len(), 1);
    let session = &topology.sessions()[0];
    assert_eq!(session.name(), "work");
    assert_eq!(
        session.working_directory().as_os_str(),
        session_cwd.as_os_str()
    );
    assert_eq!(session.windows().len(), 1);
    let window = &session.windows()[0];
    assert_eq!(window.source_index(), 0);
    assert_eq!(window.name(), "editor: main");
    assert_eq!(window.panes().len(), 2);
    assert_eq!(window.panes()[0].source_index(), 0);
    assert_eq!(window.panes()[1].source_index(), 1);
    let pane_cwds = window
        .panes()
        .iter()
        .map(|pane| pane.working_directory().as_os_str())
        .collect::<Vec<_>>();
    assert!(pane_cwds.contains(&session_cwd.as_os_str()));
    assert!(pane_cwds.contains(&second_pane_cwd.as_os_str()));
    for pane in window.panes() {
        assert!(pane.process_anchor().pane_pid() > 0);
        assert!(
            pane.process_anchor()
                .pane_tty()
                .as_bytes()
                .starts_with(b"/")
        );
    }
}

#[test]
fn classifies_a_tmux_explicit_interactive_shell_as_idle() {
    let _serial = SOURCE_COMMAND_TEST.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let session_cwd = temp.path().join("session");
    std::fs::create_dir(&session_cwd).unwrap();
    let socket = temp.path().join("explicit-shell.sock");
    let _server = TemporaryTmuxServer::start_with_explicit_shell(&socket, &session_cwd);
    let source =
        SnapshotSource::try_from_bytes(socket.as_os_str().as_encoded_bytes().to_vec()).unwrap();
    let mut adapter = TmuxAdapter::new(source, LinuxProcessInspector::new());
    let topology = adapter.read_topology().unwrap();
    let pane = &topology.sessions()[0].windows()[0].panes()[0];

    assert!(matches!(
        adapter.inspect_pane(pane),
        PaneProcessObservation::Idle
    ));
}

#[test]
fn named_and_path_sources_publish_to_one_global_stream() {
    let _serial = SOURCE_COMMAND_TEST.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let named_work = temp.path().join("named-work");
    let path_work = temp.path().join("path-work");
    let tmux_tmpdir = temp.path().join("tmux-tmp");
    let state_home = temp.path().join("state");
    fs::create_dir(&named_work).unwrap();
    fs::create_dir(&path_work).unwrap();
    fs::create_dir(&tmux_tmpdir).unwrap();
    let named_selector = TmuxSelector::SocketName(OsString::from("global-stream-name"));
    let path_selector =
        TmuxSelector::SocketPath(temp.path().join("global-stream.sock").into_os_string());
    let named_server = TemporaryTmuxServer::start_selected(
        named_selector.clone(),
        Some(&tmux_tmpdir),
        &named_work,
    );
    let path_server = TemporaryTmuxServer::start_selected(path_selector.clone(), None, &path_work);
    let named_reported_socket = named_server.reported_socket_path();
    let path_reported_socket = path_server.reported_socket_path();

    let named_snapshot = capture_selected_source(&named_selector, Some(&tmux_tmpdir), &state_home);
    let path_snapshot = capture_selected_source(&path_selector, None, &state_home);

    assert_eq!(named_snapshot.parent(), path_snapshot.parent());
    assert_eq!(
        named_snapshot.parent(),
        Some(state_home.join("tmux-rescue/snapshots").as_path())
    );
    assert_ne!(named_snapshot, path_snapshot);
    let expected_latest = [
        named_snapshot.file_name().unwrap(),
        path_snapshot.file_name().unwrap(),
    ]
    .into_iter()
    .max()
    .unwrap();
    assert_eq!(
        fs::read_link(state_home.join("tmux-rescue/latest")).unwrap(),
        Path::new("snapshots").join(expected_latest)
    );
    let named = ValidatedSnapshot::from_json(&fs::read(named_snapshot).unwrap()).unwrap();
    let path = ValidatedSnapshot::from_json(&fs::read(path_snapshot).unwrap()).unwrap();
    assert_eq!(
        named.source().path().as_os_str(),
        named_reported_socket.as_os_str()
    );
    assert_eq!(
        path.source().path().as_os_str(),
        path_reported_socket.as_os_str()
    );
}

#[test]
fn explicit_source_commands_preserve_selector_and_share_context() {
    let _serial = SOURCE_COMMAND_TEST.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let working = temp.path().join("working");
    std::fs::create_dir(&working).unwrap();
    let (log, _context) = install_fake_tmux(temp.path(), &working, false);
    let selector_bytes = vec![b'.', b'/', 0xff];
    for selector in [
        TmuxSelector::SocketName(OsString::from("exact-name")),
        TmuxSelector::SocketPath(OsString::from_vec(selector_bytes.clone())),
    ] {
        let mut adapter = TmuxAdapter::selected_source(Some(selector)).unwrap();
        assert_eq!(adapter.source().path().as_bytes(), b"/reported/source.sock");
        adapter.read_topology().unwrap();
    }

    let bytes = std::fs::read(log).unwrap();
    assert_eq!(bytes.matches(b"BEGIN\n").count(), 4);
    assert_eq!(
        bytes
            .matches(format!("PWD={}\n", working.display()).as_bytes())
            .count(),
        4
    );
    assert_eq!(bytes.matches(b"TMUX=unset\n").count(), 4);
    assert_eq!(bytes.matches(b"ARG=-L\nARG=exact-name\n").count(), 2);
    let mut exact_selector = b"ARG=-S\nARG=".to_vec();
    exact_selector.extend_from_slice(&selector_bytes);
    exact_selector.push(b'\n');
    assert_eq!(bytes.matches(&exact_selector).count(), 2);
    assert_eq!(bytes.matches(b"ARG=-N\n").count(), 4);
}

#[test]
fn ambient_discovery_transitions_once_to_the_reported_socket_path() {
    let _serial = SOURCE_COMMAND_TEST.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let working = temp.path().join("working");
    std::fs::create_dir(&working).unwrap();
    let (log, _context) = install_fake_tmux(temp.path(), &working, false);

    let mut adapter = TmuxAdapter::selected_source(None).unwrap();
    adapter.read_topology().unwrap();

    let bytes = std::fs::read(log).unwrap();
    assert_eq!(
        bytes
            .matches(format!("PWD={}\n", working.display()).as_bytes())
            .count(),
        2
    );
    let first = bytes.find(b"BEGIN\n").unwrap();
    let second = bytes[first + 1..].find(b"BEGIN\n").unwrap() + first + 1;
    let metadata = &bytes[first..second];
    let topology = &bytes[second..];
    assert!(
        metadata
            .windows(b"TMUX=ambient.sock,1,0\n".len())
            .any(|window| window == b"TMUX=ambient.sock,1,0\n")
    );
    assert!(
        !metadata
            .windows(b"ARG=-L\n".len())
            .any(|window| window == b"ARG=-L\n")
    );
    assert!(
        !metadata
            .windows(b"ARG=-S\n".len())
            .any(|window| window == b"ARG=-S\n")
    );
    assert!(
        topology
            .windows(b"TMUX=unset\n".len())
            .any(|window| window == b"TMUX=unset\n")
    );
    assert!(
        topology
            .windows(b"ARG=-S\nARG=/reported/source.sock\n".len())
            .any(|window| window == b"ARG=-S\nARG=/reported/source.sock\n")
    );
    assert_eq!(bytes.matches(b"ARG=-N\n").count(), 2);
}

#[test]
fn failed_source_selection_does_not_publish_a_snapshot() {
    let _serial = SOURCE_COMMAND_TEST.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let working = temp.path().join("working");
    std::fs::create_dir(&working).unwrap();
    let (_log, _context) = install_fake_tmux(temp.path(), &working, true);
    let state = temp.path().join("state");

    let output = Command::new(env!("CARGO_BIN_EXE_tmux-rescue"))
        .args(["-L", "abc", "snapshot"])
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(!state.join("tmux-rescue").exists());
}

#[test]
fn visible_grid_capture_uses_stable_metadata_and_never_joins_rows() {
    const METADATA_FORMAT: &str = concat!(
        "ARG=#{n:pane_id}:#{pane_id}",
        "#{n:pane_width}:#{pane_width}",
        "#{n:pane_height}:#{pane_height}",
        "#{n:cursor_x}:#{cursor_x}",
        "#{n:cursor_y}:#{cursor_y}",
        "#{n:pane_in_mode}:#{pane_in_mode}\n",
    );

    let _serial = SOURCE_COMMAND_TEST.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let working = temp.path().join("working");
    fs::create_dir(&working).unwrap();
    let (log, _context) = install_visible_grid_fake_tmux(temp.path(), &working, "stable");
    let source = SnapshotSource::try_from_bytes(b"/tmp/source.sock".to_vec()).unwrap();
    let mut adapter = TmuxAdapter::new(source, LinuxProcessInspector::new());
    let topology = adapter.read_topology().unwrap();
    let pane = &topology.sessions()[0].windows()[0].panes()[0];
    fs::write(&log, []).unwrap();

    let grid = adapter.read_visible_pane(pane).unwrap();

    assert_eq!(
        grid.rows()
            .iter()
            .map(|row| row.as_str())
            .collect::<Vec<_>>(),
        ["» draft", "  second", "", "  95% context left"]
    );
    let log = String::from_utf8(fs::read(log).unwrap()).unwrap();
    let commands = log
        .split("BEGIN\n")
        .filter(|command| !command.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(commands.len(), 3, "unexpected command log: {log}");
    assert!(commands[0].contains("ARG=display-message\nARG=-p\nARG=-t\nARG=%15\n"));
    assert!(commands[0].contains(METADATA_FORMAT));
    assert!(commands[1].contains("ARG=capture-pane\nARG=-p\nARG=-e\nARG=-t\nARG=%15\n"));
    assert!(commands[2].contains("ARG=display-message\nARG=-p\nARG=-t\nARG=%15\n"));
    assert!(commands[2].contains(METADATA_FORMAT));
    for command in &commands {
        let selector = command
            .find("ARG=-S\nARG=/tmp/source.sock\n")
            .expect("source socket selector");
        let subcommand = command
            .find("ARG=display-message\n")
            .or_else(|| command.find("ARG=capture-pane\n"))
            .expect("tmux subcommand");
        assert!(
            selector < subcommand,
            "selector moved after subcommand: {command}"
        );
    }
    let capture_arguments = commands[1].split_once("ARG=capture-pane\n").unwrap().1;
    for forbidden in [
        "ARG=-J\n",
        "ARG=-S\n",
        "ARG=-E\n",
        "ARG=set-buffer\n",
        "ARG=paste-buffer\n",
        "ARG=send-keys\n",
    ] {
        assert!(
            !capture_arguments.contains(forbidden),
            "source capture used forbidden argument {forbidden:?}: {log}"
        );
    }
}

#[test]
fn real_visible_grid_capture_preserves_the_approved_codex_suffix() {
    let _serial = SOURCE_COMMAND_TEST.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let working = temp.path().join("working");
    fs::create_dir(&working).unwrap();
    let socket = temp.path().join("visible-grid-source.sock");
    let server = TemporaryTmuxServer::start(&socket, &working);
    let renderer = temp.path().join("render-visible-grid");
    fs::write(
        &renderer,
        format!(
            "#!/bin/sh\nprintf '%s' '» The test prompt for recovering.\n\n  Line 1.\n\n  Line 2.\n\n{APPROVED_CODEX_FOOTER}'\nprintf '\\033[2A\\r\\033[9C'\nexec /bin/sleep 30\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&renderer, fs::Permissions::from_mode(0o700)).unwrap();
    let pane_title = format!(
        "codex-visible-grid-{}",
        temp.path().file_name().unwrap().to_string_lossy()
    );
    server.run(&["resize-window", "-t", "work:0", "-x", "132", "-y", "7"]);
    server.run(&["select-pane", "-t", "work:0.0", "-T", &pane_title]);
    server.run(&[
        "respawn-pane",
        "-k",
        "-t",
        "work:0.0",
        renderer.to_str().unwrap(),
    ]);

    let source =
        SnapshotSource::try_from_bytes(socket.as_os_str().as_encoded_bytes().to_vec()).unwrap();
    let mut adapter = TmuxAdapter::new(source, LinuxProcessInspector::new());
    let topology = adapter.read_topology().unwrap();
    let pane = &topology.sessions()[0].windows()[0].panes()[0];
    let expected = [
        "» The test prompt for recovering.",
        "",
        "  Line 1.",
        "",
        "  Line 2.",
        "",
        APPROVED_CODEX_FOOTER,
    ];
    let deadline = Instant::now() + Duration::from_secs(2);
    let grid = loop {
        let grid = adapter.read_visible_pane(pane).unwrap();
        if grid.rows().iter().map(|row| row.as_str()).eq(expected) {
            break grid;
        }
        assert!(
            Instant::now() < deadline,
            "isolated pane never rendered the approved suffix: {:?}",
            grid.rows()
        );
        thread::sleep(Duration::from_millis(10));
    };

    assert_eq!(grid.metadata().width().get(), 132);
    assert_eq!(grid.metadata().height().get(), 7);
    assert_eq!(grid.metadata().cursor().x(), 9);
    assert_eq!(grid.metadata().cursor().y(), 4);
    assert_eq!(
        grid.rows()
            .iter()
            .map(|row| row.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    let title = selected_tmux(&server.selector, None, true)
        .args([
            "display-message",
            "-p",
            "-t",
            pane.pane_id().as_str(),
            "#{pane_title}",
        ])
        .output()
        .unwrap();
    assert!(title.status.success());
    assert_eq!(String::from_utf8(title.stdout).unwrap().trim(), pane_title);
}

#[test]
fn changing_metadata_or_wrong_row_count_skips_prompt_capture() {
    let _serial = SOURCE_COMMAND_TEST.lock().unwrap();
    for (scenario, expected) in [
        ("changed", "pane metadata changed"),
        ("wrong_rows", "visible tmux pane output is invalid"),
        ("wrong_mode", "visible tmux pane could not be read"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let working = temp.path().join("working");
        fs::create_dir(&working).unwrap();
        let (_log, _context) = install_visible_grid_fake_tmux(temp.path(), &working, scenario);
        let source = SnapshotSource::try_from_bytes(b"/tmp/source.sock".to_vec()).unwrap();
        let mut adapter = TmuxAdapter::new(source, LinuxProcessInspector::new());
        let topology = adapter.read_topology().unwrap();
        let pane = &topology.sessions()[0].windows()[0].panes()[0];

        let failure = adapter.read_visible_pane(pane).unwrap_err();

        assert!(
            failure.message().contains(expected),
            "{scenario}: {}",
            failure.message()
        );
        assert!(!failure.message().contains("private source row"));
        assert!(!format!("{failure:?}").contains("private source row"));
    }
}

#[test]
fn visible_grid_capture_targets_the_ephemeral_pane_id() {
    let _serial = SOURCE_COMMAND_TEST.lock().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let working = temp.path().join("working");
    fs::create_dir(&working).unwrap();
    let (log, _context) = install_visible_grid_fake_tmux(temp.path(), &working, "stable");
    let source = SnapshotSource::try_from_bytes(b"/tmp/source.sock".to_vec()).unwrap();
    let mut adapter = TmuxAdapter::new(source, LinuxProcessInspector::new());
    let topology = adapter.read_topology().unwrap();
    let pane = &topology.sessions()[0].windows()[0].panes()[0];
    assert_eq!(pane.source_index(), 0);
    assert_eq!(pane.pane_id().as_str(), "%15");
    fs::write(&log, []).unwrap();

    adapter.read_visible_pane(pane).unwrap();

    let log = String::from_utf8(fs::read(log).unwrap()).unwrap();
    assert!(log.contains("ARG=capture-pane\nARG=-p\nARG=-e\nARG=-t\nARG=%15\n"));
    assert!(!log.contains("work:0.0"));
}

trait ByteSliceExt {
    fn matches<'a>(&'a self, needle: &'a [u8]) -> Box<dyn Iterator<Item = usize> + 'a>;
    fn find(&self, needle: &[u8]) -> Option<usize>;
}

impl ByteSliceExt for [u8] {
    fn matches<'a>(&'a self, needle: &'a [u8]) -> Box<dyn Iterator<Item = usize> + 'a> {
        Box::new(
            self.windows(needle.len())
                .enumerate()
                .filter_map(move |(index, window)| (window == needle).then_some(index)),
        )
    }

    fn find(&self, needle: &[u8]) -> Option<usize> {
        self.windows(needle.len())
            .position(|window| window == needle)
    }
}
