use std::path::{Path, PathBuf};
use std::process::Command;

use tmux_rescue::{
    CaptureSource, LinuxProcessInspector, PaneProcessObservation, SnapshotSource, TmuxAdapter,
};

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
