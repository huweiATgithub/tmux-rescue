use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use tmux_rescue::{
    CaptureSource, LinuxProcessInspector, PaneProcessObservation, SnapshotSource, TmuxAdapter,
    TmuxSelector,
};

static SOURCE_COMMAND_TEST: Mutex<()> = Mutex::new(());

struct ProcessContextGuard {
    directory: PathBuf,
    path: Option<OsString>,
    tmux: Option<OsString>,
    log: Option<OsString>,
    fail: Option<OsString>,
}

impl Drop for ProcessContextGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.directory).unwrap();
        for (key, value) in [
            ("PATH", &self.path),
            ("TMUX", &self.tmux),
            ("FAKE_TMUX_LOG", &self.log),
            ("FAKE_TMUX_FAIL", &self.fail),
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
        b"#!/bin/sh\n{ printf 'BEGIN\\nPWD=%s\\nTMUX=%s\\n' \"$PWD\" \"${TMUX-unset}\"; for arg do printf 'ARG=%s\\n' \"$arg\"; done; } >> \"$FAKE_TMUX_LOG\"\n[ \"${FAKE_TMUX_FAIL-}\" = 1 ] && { printf 'selection failed\\n' >&2; exit 1; }\ncase \" $* \" in\n  *' #{n:socket_path}:#{socket_path} '*) printf '21:/reported/source.sock\\n' ;;\n  *' list-panes '*) printf '4:work1:01:04:/tmp6:editor4:/tmp1:19:/dev/null0:7:/bin/sh\\n' ;;\n  *) printf 'unexpected fake tmux command\\n' >&2; exit 2 ;;\nesac\n",
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
    }
    (log, guard)
}

struct TemporaryTmuxServer {
    socket: PathBuf,
}

impl TemporaryTmuxServer {
    fn start(socket: &Path, session_cwd: &Path) -> Self {
        let output = Command::new("tmux")
            .args(["-u", "-S"])
            .arg(socket)
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
            socket: socket.to_owned(),
        }
    }

    fn run(&self, arguments: &[&str]) {
        let output = Command::new("tmux")
            .args(["-u", "-N", "-S"])
            .arg(&self.socket)
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
        let output = Command::new("tmux")
            .args(["-u", "-S"])
            .arg(socket)
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
            socket: socket.to_owned(),
        }
    }
}

impl Drop for TemporaryTmuxServer {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-u", "-N", "-S"])
            .arg(&self.socket)
            .arg("kill-server")
            .status();
    }
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
