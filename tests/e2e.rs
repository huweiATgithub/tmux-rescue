use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use tmux_rescue::{AutomaticRecovery, PaneRecovery, ValidatedSnapshot};

const SESSION: &str = "e2e";
const WINDOW_INDEX: u32 = 3;
const WINDOW_NAME: &str = "rescue workspace";
const MANUAL_COMMAND: &str = "exec '/usr/bin/sleep' '3600'";

fn os(value: impl AsRef<OsStr>) -> OsString {
    value.as_ref().to_owned()
}

fn isolated_tmux(socket: &Path) -> Command {
    assert!(socket.is_absolute());
    let mut command = Command::new("tmux");
    command
        .args(["-u", "-N", "-S"])
        .arg(socket)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    command
}

fn isolated_tmux_start(socket: &Path) -> Command {
    assert!(socket.is_absolute());
    let mut command = Command::new("tmux");
    command
        .args(["-u", "-S"])
        .arg(socket)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("SHELL", "/bin/sh");
    command
}

fn require_success(operation: &str, output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{operation} failed with {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn tmux_stdout(socket: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = isolated_tmux(socket)
        .args(arguments)
        .output()
        .expect("tmux is installed");
    require_success("isolated tmux command", output)
}

fn tmux_stdout_os(socket: &Path, arguments: &[OsString]) -> Vec<u8> {
    let output = isolated_tmux(socket)
        .args(arguments)
        .output()
        .expect("tmux is installed");
    require_success("isolated tmux command", output)
}

struct IsolatedServerGuard {
    socket: PathBuf,
}

impl IsolatedServerGuard {
    fn new(socket: &Path) -> Self {
        assert!(socket.is_absolute());
        Self {
            socket: socket.to_owned(),
        }
    }

    fn kill(&self) {
        let output = isolated_tmux(&self.socket)
            .arg("kill-server")
            .output()
            .expect("tmux is installed");
        require_success("kill isolated tmux server", output);
    }
}

impl Drop for IsolatedServerGuard {
    fn drop(&mut self) {
        let _ = isolated_tmux(&self.socket).arg("kill-server").output();
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn wait_until(label: &str, timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(predicate(), "timed out waiting for {label}");
}

fn pane_command(socket: &Path, target: &str) -> Option<String> {
    let output = isolated_tmux(socket)
        .args([
            "display-message",
            "-p",
            "-t",
            target,
            "#{pane_dead}:#{pane_current_command}",
        ])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn server_is_running(socket: &Path) -> bool {
    isolated_tmux(socket)
        .arg("has-session")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn find_mdbook() -> PathBuf {
    let output = Command::new("sh")
        .args(["-c", "command -v mdbook"])
        .output()
        .expect("a POSIX shell is installed");
    let path = require_success("resolve mdbook", output);
    fs::canonicalize(Path::new(
        std::str::from_utf8(&path)
            .expect("mdbook path is UTF-8")
            .trim(),
    ))
    .expect("mdbook resolves to an existing executable")
}

fn unused_local_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn port_accepts_connections(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_millis(50),
    )
    .is_ok()
}

fn port_is_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn create_book(directory: &Path) {
    fs::create_dir(directory).unwrap();
    fs::create_dir(directory.join("src")).unwrap();
    fs::write(
        directory.join("book.toml"),
        "[book]\ntitle = \"tmux-rescue e2e\"\n",
    )
    .unwrap();
    fs::write(directory.join("src/SUMMARY.md"), "# Summary\n").unwrap();
}

fn start_source_server(
    socket: &Path,
    idle_directory: &Path,
    manual_directory: &Path,
    book_directory: &Path,
    mdbook: &Path,
    port: u16,
) -> IsolatedServerGuard {
    let guard = IsolatedServerGuard::new(socket);
    let output = isolated_tmux_start(socket)
        .args([
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            SESSION,
            "-n",
            WINDOW_NAME,
            "-c",
        ])
        .arg(idle_directory)
        .output()
        .expect("tmux is installed");
    require_success("start isolated source tmux server", output);

    tmux_stdout(
        socket,
        &[
            "move-window",
            "-s",
            &format!("{SESSION}:0"),
            "-t",
            &format!("{SESSION}:{WINDOW_INDEX}"),
        ],
    );
    tmux_stdout(
        socket,
        &[
            "set-option",
            "-w",
            "-t",
            &format!("{SESSION}:{WINDOW_INDEX}"),
            "automatic-rename",
            "off",
        ],
    );

    let manual_pane = tmux_stdout_os(
        socket,
        &[
            os("split-window"),
            os("-d"),
            os("-P"),
            os("-F"),
            os("#{pane_id}"),
            os("-t"),
            os(format!("{SESSION}:{WINDOW_INDEX}")),
            os("-c"),
            os(manual_directory),
            os(MANUAL_COMMAND),
        ],
    );
    let manual_pane = String::from_utf8(manual_pane).unwrap().trim().to_owned();
    let mdbook_command = format!(
        "exec {} 'serve' '--hostname' '127.0.0.1' '--port' '{}'",
        shell_quote(mdbook.to_str().expect("mdbook path is UTF-8")),
        port
    );
    let mdbook_pane = tmux_stdout_os(
        socket,
        &[
            os("split-window"),
            os("-d"),
            os("-P"),
            os("-F"),
            os("#{pane_id}"),
            os("-t"),
            os(format!("{SESSION}:{WINDOW_INDEX}")),
            os("-c"),
            os(book_directory),
            os(mdbook_command),
        ],
    );
    let mdbook_pane = String::from_utf8(mdbook_pane).unwrap().trim().to_owned();
    tmux_stdout(
        socket,
        &[
            "rename-window",
            "-t",
            &format!("{SESSION}:{WINDOW_INDEX}"),
            WINDOW_NAME,
        ],
    );

    wait_until("manual foreground command", Duration::from_secs(5), || {
        pane_command(socket, &manual_pane).as_deref() == Some("0:sleep")
    });
    wait_until("mdbook foreground command", Duration::from_secs(5), || {
        pane_command(socket, &mdbook_pane).as_deref() == Some("0:mdbook")
    });
    wait_until("mdbook source listener", Duration::from_secs(5), || {
        port_accepts_connections(port)
    });
    guard
}

fn tmux_selection(socket: &Path) -> String {
    let process_id =
        String::from_utf8(tmux_stdout(socket, &["display-message", "-p", "#{pid}"])).unwrap();
    format!(
        "{},{},0",
        socket.to_str().expect("temporary paths are UTF-8"),
        process_id.trim()
    )
}

fn cli(state_home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tmux-rescue"));
    command
        .env("XDG_STATE_HOME", state_home)
        .env("SHELL", "/bin/sh");
    command
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn snapshot_path_from(stdout: &str) -> PathBuf {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("snapshot: "))
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("snapshot output omitted its immutable path: {stdout:?}"))
}

#[derive(Debug)]
struct RestoredPane {
    session_path: PathBuf,
    window_name: String,
    pane_path: PathBuf,
    command: String,
}

fn restored_panes(socket: &Path) -> BTreeMap<u32, RestoredPane> {
    let output = tmux_stdout(
        socket,
        &[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{session_path}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_current_path}\t#{pane_current_command}",
        ],
    );
    String::from_utf8(output)
        .expect("controlled tmux fields are UTF-8")
        .lines()
        .map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            assert_eq!(fields.len(), 7, "unexpected restored pane row: {line:?}");
            assert_eq!(fields[0], SESSION);
            assert_eq!(fields[2].parse::<u32>().unwrap(), WINDOW_INDEX);
            (
                fields[4].parse::<u32>().unwrap(),
                RestoredPane {
                    session_path: PathBuf::from(fields[1]),
                    window_name: fields[3].to_owned(),
                    pane_path: PathBuf::from(fields[5]),
                    command: fields[6].to_owned(),
                },
            )
        })
        .collect()
}

#[test]
fn snapshots_plans_and_restores_an_isolated_real_tmux_server() {
    let temp = tempfile::tempdir().unwrap();
    let state_home = temp.path().join("state");
    let source_socket = temp.path().join("source.sock");
    let target_socket = temp.path().join("target.sock");
    let idle_directory = temp.path().join("idle");
    let manual_directory = temp.path().join("manual");
    let book_directory = temp.path().join("book");
    fs::create_dir(&idle_directory).unwrap();
    fs::create_dir(&manual_directory).unwrap();
    create_book(&book_directory);
    let mdbook = find_mdbook();
    let port = unused_local_port();
    let source = start_source_server(
        &source_socket,
        &idle_directory,
        &manual_directory,
        &book_directory,
        &mdbook,
        port,
    );
    let target = IsolatedServerGuard::new(&target_socket);

    let snapshot_output = cli(&state_home)
        .arg("snapshot")
        .env("TMUX", tmux_selection(&source_socket))
        .env_remove("TMUX_PANE")
        .output()
        .unwrap();

    assert_eq!(
        snapshot_output.status.code(),
        Some(0),
        "snapshot stderr: {}",
        stderr_text(&snapshot_output)
    );
    let snapshot_stdout = stdout_text(&snapshot_output);
    assert!(snapshot_stdout.contains("consistency: stable"));
    assert!(snapshot_stdout.contains("latest: updated"));
    let snapshot_path = snapshot_path_from(&snapshot_stdout);
    let store_root = state_home.join("tmux-rescue");
    assert_eq!(
        snapshot_path.parent(),
        Some(store_root.join("snapshots").as_path())
    );
    let metadata = fs::metadata(&snapshot_path).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    let immutable_snapshot = fs::read(&snapshot_path).unwrap();
    let latest_target = fs::read_link(store_root.join("latest")).unwrap();
    assert!(!latest_target.is_absolute());
    assert_eq!(
        latest_target,
        Path::new("snapshots").join(snapshot_path.file_name().unwrap())
    );

    let captured = ValidatedSnapshot::from_json(&immutable_snapshot).unwrap();
    assert_eq!(
        captured.source().path().as_os_str(),
        source_socket.as_os_str()
    );
    let window = &captured.sessions()[0].windows()[0];
    assert_eq!(window.source_index(), WINDOW_INDEX);
    assert_eq!(window.name(), WINDOW_NAME);
    let panes = window.panes();
    assert_eq!(panes.len(), 3);
    let idle = panes
        .iter()
        .find(|pane| matches!(pane.recovery(), PaneRecovery::Idle))
        .expect("captured idle pane");
    let manual = panes
        .iter()
        .find_map(|pane| match pane.recovery() {
            PaneRecovery::Manual(command) => Some((pane, command)),
            _ => None,
        })
        .expect("captured manual pane");
    let automatic = panes
        .iter()
        .find_map(|pane| match pane.recovery() {
            PaneRecovery::Automatic(AutomaticRecovery::MdBookServe { command }) => {
                Some((pane, command))
            }
            _ => None,
        })
        .expect("captured automatic mdbook pane");
    assert_eq!(
        idle.working_directory().as_os_str(),
        idle_directory.as_os_str()
    );
    assert_eq!(
        manual.0.working_directory().as_os_str(),
        manual_directory.as_os_str()
    );
    assert_eq!(
        automatic.0.working_directory().as_os_str(),
        book_directory.as_os_str()
    );
    assert_eq!(manual.1.argv().len(), 2);
    assert_eq!(
        Path::new(OsStr::from_bytes(manual.1.argv()[0].as_bytes()))
            .file_name()
            .unwrap()
            .as_bytes(),
        b"sleep"
    );
    assert_eq!(manual.1.argv()[1].as_bytes(), b"3600");
    let manual_hint = manual
        .1
        .argv()
        .iter()
        .map(|argument| shell_quote(std::str::from_utf8(argument.as_bytes()).unwrap()))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        automatic.1.command().argv()[0].as_bytes(),
        mdbook.as_os_str().as_bytes()
    );
    assert_eq!(automatic.1.command().argv()[1].as_bytes(), b"serve");

    source.kill();
    wait_until("source server shutdown", Duration::from_secs(5), || {
        !server_is_running(&source_socket)
    });
    wait_until("source mdbook port release", Duration::from_secs(5), || {
        port_is_available(port)
    });

    let plan_output = cli(&state_home)
        .args(["restore", "--target"])
        .arg(&target_socket)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap();

    assert_eq!(
        plan_output.status.code(),
        Some(0),
        "plan-only stderr: {}",
        stderr_text(&plan_output)
    );
    let plan_stdout = stdout_text(&plan_output);
    assert!(plan_stdout.contains(target_socket.to_str().unwrap()));
    assert!(plan_stdout.contains(WINDOW_NAME));
    assert!(plan_stdout.contains("leave idle shell"));
    assert!(plan_stdout.contains("paste manual hint"));
    assert!(plan_stdout.contains("launch automatic recovery"));
    assert!(
        !target_socket.exists(),
        "plan-only restore created its target"
    );
    assert!(port_is_available(port));

    let restore_output = cli(&state_home)
        .args(["restore", "--target"])
        .arg(&target_socket)
        .arg("--run")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap();

    assert_eq!(
        restore_output.status.code(),
        Some(0),
        "restore stdout: {}\nrestore stderr: {}",
        stdout_text(&restore_output),
        stderr_text(&restore_output)
    );
    let restore_stdout = stdout_text(&restore_output);
    assert!(restore_stdout.starts_with(&plan_stdout));
    assert!(restore_stdout.contains("restore: complete"));
    assert!(restore_stdout.contains("restored idle shell"));
    assert!(restore_stdout.contains("prepared manual hint"));
    assert!(restore_stdout.contains("recovered automatically"));

    let restored = restored_panes(&target_socket);
    assert_eq!(restored.len(), 3);
    for pane in restored.values() {
        assert_eq!(pane.session_path, idle_directory);
        assert_eq!(pane.window_name, WINDOW_NAME);
    }
    let (_, idle_target) = restored
        .iter()
        .find(|(_, pane)| pane.pane_path == idle_directory)
        .expect("restored idle pane");
    let (manual_target_index, manual_target) = restored
        .iter()
        .find(|(_, pane)| pane.pane_path == manual_directory)
        .expect("restored manual pane");
    let (_, automatic_target) = restored
        .iter()
        .find(|(_, pane)| pane.pane_path == book_directory)
        .expect("restored mdbook pane");
    assert_eq!(idle_target.command, "sh");
    assert_eq!(manual_target.command, "sh");
    assert_eq!(automatic_target.command, "mdbook");

    let manual_screen = String::from_utf8(tmux_stdout(
        &target_socket,
        &[
            "capture-pane",
            "-p",
            "-J",
            "-S",
            "-20",
            "-t",
            &format!("{SESSION}:{WINDOW_INDEX}.{manual_target_index}"),
        ],
    ))
    .unwrap();
    assert!(
        manual_screen.contains(&manual_hint),
        "manual hint was not pasted literally: {manual_screen:?}"
    );
    wait_until("restored mdbook listener", Duration::from_secs(5), || {
        port_accepts_connections(port)
    });
    assert_eq!(fs::read(&snapshot_path).unwrap(), immutable_snapshot);

    target.kill();
    wait_until("target server shutdown", Duration::from_secs(5), || {
        !server_is_running(&target_socket)
    });
}
