use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use tmux_rescue::{
    AttentionReason, LosslessOsString, PaneRestoreOutcome, PlanningExecutable,
    RecordedAbsolutePath, RestoreEnvironment, RestoreEnvironmentFailure, RestoreExecutionFailure,
    RestoreExecutor, RestoreRunStatus, RestoreTargetState, TargetDisposition, TargetShell,
    TmuxRestoreAdapter, TmuxSelector, ValidatedSnapshot, plan_restore,
};

static ISOLATED_TMUX_TEST: Mutex<()> = Mutex::new(());

fn encoded(value: &str) -> Value {
    json!({"encoding": "utf8", "value": value})
}

fn encoded_path(path: &Path) -> Value {
    encoded(path.to_str().expect("temporary paths are UTF-8"))
}

fn idle_pane(source_index: u32, working_directory: &Path) -> Value {
    json!({
        "source_index": source_index,
        "working_directory": encoded_path(working_directory),
        "recovery": {"kind": "idle"}
    })
}

fn manual_pane(source_index: u32, working_directory: &Path, marker: &Path) -> Value {
    json!({
        "source_index": source_index,
        "working_directory": encoded_path(working_directory),
        "recovery": {
            "kind": "manual",
            "command": {
                "executable": encoded("/usr/bin/touch"),
                "argv": [encoded("/usr/bin/touch"), encoded_path(marker)]
            }
        }
    })
}

fn automatic_mdbook_pane(
    source_index: u32,
    working_directory: &Path,
    executable: &Path,
    port: u16,
) -> Value {
    let executable = executable.to_str().expect("mdbook path is UTF-8");
    json!({
        "source_index": source_index,
        "working_directory": encoded_path(working_directory),
        "recovery": {
            "kind": "automatic",
            "recovery": {
                "kind": "md_book_serve",
                "command": {
                    "executable": encoded(executable),
                    "argv": [
                        encoded(executable),
                        encoded("serve"),
                        encoded("--hostname"),
                        encoded("127.0.0.1"),
                        encoded("--port"),
                        encoded(&port.to_string())
                    ]
                }
            }
        }
    })
}

fn window(source_index: u32, name: &str, panes: Vec<Value>) -> Value {
    json!({
        "source_index": source_index,
        "name": name,
        "panes": panes
    })
}

fn session(name: &str, working_directory: &Path, windows: Vec<Value>) -> Value {
    json!({
        "name": name,
        "working_directory": encoded_path(working_directory),
        "windows": windows
    })
}

fn snapshot(source: &Path, sessions: Vec<Value>) -> ValidatedSnapshot {
    let raw = json!({
        "captured_at": "2026-07-23T00:00:00Z",
        "source": encoded_path(source),
        "consistency": {"kind": "stable"},
        "sessions": sessions
    });
    ValidatedSnapshot::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap()
}

struct PlanningEnvironment {
    home: RecordedAbsolutePath,
    executables: HashMap<Vec<u8>, Vec<u8>>,
}

impl PlanningEnvironment {
    fn new(home: &Path) -> Self {
        Self {
            home: RecordedAbsolutePath::try_from_bytes(home.as_os_str().as_bytes().to_vec())
                .unwrap(),
            executables: HashMap::new(),
        }
    }

    fn with_executable(mut self, command: &Path) -> Self {
        let bytes = command.as_os_str().as_bytes().to_vec();
        let command_name = command.file_name().unwrap().as_bytes().to_vec();
        self.executables.insert(command_name, bytes);
        self
    }
}

impl RestoreEnvironment for PlanningEnvironment {
    fn target_shell(&self) -> Result<TargetShell, RestoreEnvironmentFailure> {
        TargetShell::try_from_bytes(b"/bin/sh".to_vec())
            .map_err(|error| RestoreEnvironmentFailure::new(error.to_string()))
    }

    fn home_directory(&self) -> Result<RecordedAbsolutePath, RestoreEnvironmentFailure> {
        Ok(self.home.clone())
    }

    fn directory_exists(&self, directory: &RecordedAbsolutePath) -> bool {
        Path::new(directory.as_os_str()).is_dir()
    }

    fn resolve_executable(
        &self,
        _directory: &RecordedAbsolutePath,
        command_word: &LosslessOsString,
    ) -> Option<PlanningExecutable> {
        self.executables
            .get(command_word.as_bytes())
            .cloned()
            .and_then(|path| PlanningExecutable::try_from_bytes(path).ok())
    }
}

fn target_selector(socket: &Path) -> TmuxSelector {
    TmuxSelector::SocketPath(socket.as_os_str().to_owned())
}

fn isolated_tmux(socket: &Path) -> Command {
    assert!(socket.is_absolute());
    let mut command = Command::new("tmux");
    command
        .args(["-u", "-N", "-S"])
        .arg(socket)
        .env_remove("TMUX");
    command
}

fn isolated_tmux_start(socket: &Path) -> Command {
    assert!(socket.is_absolute());
    let mut command = Command::new("tmux");
    command.args(["-u", "-S"]).arg(socket).env_remove("TMUX");
    command
}

fn require_success(operation: &str, output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn tmux_stdout(socket: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = isolated_tmux(socket)
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("failed to run isolated tmux: {error}"));
    require_success("isolated tmux command", output)
}

struct IsolatedServerGuard {
    socket: PathBuf,
}

struct EnvironmentGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

struct TargetCommandContext {
    path: Option<OsString>,
    tmux: Option<OsString>,
    log: Option<OsString>,
}

impl Drop for TargetCommandContext {
    fn drop(&mut self) {
        for (key, value) in [
            ("PATH", &self.path),
            ("TMUX", &self.tmux),
            ("FAKE_TMUX_LOG", &self.log),
        ] {
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

fn install_fake_target_tmux(temp: &Path) -> (PathBuf, TargetCommandContext) {
    let bin = temp.join("bin");
    fs::create_dir(&bin).unwrap();
    let executable = bin.join("tmux");
    fs::write(
        &executable,
        b"#!/bin/sh\n{ printf 'BEGIN\\nPWD=%s\\nTMUX=%s\\n' \"$PWD\" \"${TMUX-unset}\"; for arg do printf 'ARG=%s\\n' \"$arg\"; done; } >> \"$FAKE_TMUX_LOG\"\ncase \" $* \" in\n  *' start-server '*) exit 0 ;;\n  *' display-message '*) printf '1:11:11:0\\n' ;;\n  *' show-options '*) printf 'wrong-token\\n' ;;\n  *' if-shell '*) exit 0 ;;\n  *) exit 1 ;;\nesac\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let log = temp.join("target-tmux.log");
    let old_path = std::env::var_os("PATH");
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        old_path.as_deref().unwrap_or(OsStr::new("")),
    )))
    .unwrap();
    let guard = TargetCommandContext {
        path: old_path,
        tmux: std::env::var_os("TMUX"),
        log: std::env::var_os("FAKE_TMUX_LOG"),
    };
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("TMUX", "ambient.sock,1,0");
        std::env::set_var("FAKE_TMUX_LOG", &log);
    }
    (log, guard)
}

impl EnvironmentGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

impl IsolatedServerGuard {
    fn for_socket(socket: &Path) -> Self {
        assert!(socket.is_absolute());
        Self {
            socket: socket.to_owned(),
        }
    }

    fn start_preexisting(socket: &Path, working_directory: &Path) -> Self {
        let guard = Self::for_socket(socket);
        let output = isolated_tmux_start(socket)
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "preexisting",
                "-n",
                "untouched",
                "-c",
            ])
            .arg(working_directory)
            .output()
            .expect("tmux is installed");
        require_success("start pre-existing isolated tmux server", output);
        tmux_stdout(
            socket,
            &[
                "set-option",
                "-g",
                "@tmux_target_test_sentinel",
                "untouched",
            ],
        );
        guard
    }

    fn fingerprint(&self) -> Vec<u8> {
        let mut fingerprint = tmux_stdout(&self.socket, &["show-options", "-g"]);
        fingerprint.push(0);
        fingerprint.extend(tmux_stdout(
            &self.socket,
            &[
                "list-panes",
                "-a",
                "-F",
                "#{session_name}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_current_path}\t#{pane_start_command}\t#{default-shell}",
            ],
        ));
        fingerprint
    }
}

impl Drop for IsolatedServerGuard {
    fn drop(&mut self) {
        let _ = isolated_tmux(&self.socket).arg("kill-server").output();
    }
}

#[derive(Debug)]
struct TopologyRow {
    session_name: String,
    session_path: String,
    window_index: u32,
    window_name: String,
    pane_index: u32,
    pane_path: String,
    pane_start_command: String,
    default_shell: String,
    pane_current_command: String,
}

impl TopologyRow {
    fn location(&self) -> (String, String, u32, String, u32, String) {
        (
            self.session_name.clone(),
            self.session_path.clone(),
            self.window_index,
            self.window_name.clone(),
            self.pane_index,
            self.pane_path.clone(),
        )
    }
}

fn topology_rows(socket: &Path) -> Vec<TopologyRow> {
    let output = tmux_stdout(
        socket,
        &[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{session_path}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_current_path}\t#{pane_start_command}\t#{default-shell}\t#{pane_current_command}",
        ],
    );
    String::from_utf8(output)
        .expect("controlled tmux fields are UTF-8")
        .lines()
        .map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            assert_eq!(fields.len(), 9, "unexpected topology row: {line:?}");
            TopologyRow {
                session_name: fields[0].to_owned(),
                session_path: fields[1].to_owned(),
                window_index: fields[2].parse().unwrap(),
                window_name: fields[3].to_owned(),
                pane_index: fields[4].parse().unwrap(),
                pane_path: fields[5].to_owned(),
                pane_start_command: fields[6].to_owned(),
                default_shell: fields[7].to_owned(),
                pane_current_command: fields[8].to_owned(),
            }
        })
        .collect()
}

fn create_directory(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn claim_and_later_clients_keep_the_exact_explicit_selector() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let session_directory = create_directory(temp.path(), "session");
    let pane_directory = create_directory(temp.path(), "pane");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "work",
            &session_directory,
            vec![window(0, "work", vec![idle_pane(0, &pane_directory)])],
        )],
    );
    let (log_path, _context) = install_fake_target_tmux(temp.path());
    let expected_directory = format!("PWD={}\n", std::env::current_dir().unwrap().display());
    for (selector, selector_arguments) in [
        (
            TmuxSelector::SocketName(OsString::from("exact-name")),
            b"ARG=-L\nARG=exact-name\n".as_slice(),
        ),
        (
            TmuxSelector::SocketPath(OsString::from("./relative.sock")),
            b"ARG=-S\nARG=./relative.sock\n".as_slice(),
        ),
    ] {
        fs::write(&log_path, b"").unwrap();
        let plan = plan_restore(
            &snapshot,
            Some(selector),
            &PlanningEnvironment::new(temp.path()),
        )
        .unwrap();
        let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

        let result = executor.execute(plan);

        assert_eq!(result.status(), RestoreRunStatus::Fatal);
        let log = fs::read(&log_path).unwrap();
        let starts = log
            .windows(b"BEGIN\n".len())
            .enumerate()
            .filter_map(|(index, bytes)| (bytes == b"BEGIN\n").then_some(index))
            .collect::<Vec<_>>();
        assert!(starts.len() >= 2, "{}", String::from_utf8_lossy(&log));
        for (index, start) in starts.iter().copied().enumerate() {
            let end = starts.get(index + 1).copied().unwrap_or(log.len());
            let command = &log[start..end];
            assert!(
                command
                    .windows(selector_arguments.len())
                    .any(|bytes| bytes == selector_arguments)
            );
            assert!(
                command
                    .windows(b"TMUX=unset\n".len())
                    .any(|bytes| bytes == b"TMUX=unset\n")
            );
            assert!(
                command
                    .windows(expected_directory.len())
                    .any(|bytes| bytes == expected_directory.as_bytes())
            );
            if index == 0 {
                assert!(
                    command
                        .windows(b"ARG=start-server\n".len())
                        .any(|bytes| bytes == b"ARG=start-server\n")
                );
                assert!(
                    !command
                        .windows(b"ARG=-N\n".len())
                        .any(|bytes| bytes == b"ARG=-N\n")
                );
            } else {
                assert!(
                    command
                        .windows(b"ARG=-N\n".len())
                        .any(|bytes| bytes == b"ARG=-N\n")
                );
            }
        }
    }
}

#[test]
fn claim_rejects_a_preexisting_target_and_leaves_it_unchanged() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let session_directory = create_directory(temp.path(), "planned-session");
    let pane_directory = create_directory(temp.path(), "planned-pane");
    let existing_directory = create_directory(temp.path(), "preexisting-session");
    let socket = temp.path().join("race-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "planned",
            &session_directory,
            vec![window(
                4,
                "planned-window",
                vec![idle_pane(0, &pane_directory)],
            )],
        )],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()),
    )
    .unwrap();

    let server = IsolatedServerGuard::start_preexisting(&socket, &existing_directory);
    let before = server.fingerprint();
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(result.status(), RestoreRunStatus::Fatal);
    assert_eq!(
        result.target_state(),
        &RestoreTargetState::Observed(TargetDisposition::Unknown)
    );
    assert!(matches!(
        result.failure(),
        Some(RestoreExecutionFailure::TargetClaimFailed { .. })
    ));
    assert_eq!(server.fingerprint(), before);
}

#[test]
fn restore_creates_the_selected_target_server() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let session_directory = create_directory(temp.path(), "crash-session");
    let pane_directory = create_directory(temp.path(), "crash-pane");
    let socket = temp.path().join("crashed-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "recovered",
            &session_directory,
            vec![window(
                0,
                "recovered-window",
                vec![idle_pane(0, &pane_directory)],
            )],
        )],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()),
    )
    .unwrap();
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(result.status(), RestoreRunStatus::Complete, "{result:#?}");
    assert_eq!(
        result.target_state(),
        &RestoreTargetState::Observed(TargetDisposition::Retained)
    );
    assert_eq!(topology_rows(&socket).len(), 1);
}

#[test]
fn creates_recorded_multi_session_topology_with_interactive_target_shells() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let alpha_session = create_directory(temp.path(), "alpha-session");
    let alpha_editor_first = create_directory(temp.path(), "alpha-editor-first");
    let alpha_editor_second = create_directory(temp.path(), "alpha-editor-second");
    let alpha_logs = create_directory(temp.path(), "alpha-logs");
    let beta_session = create_directory(temp.path(), "beta-session");
    let beta_ops = create_directory(temp.path(), "beta-ops");
    let shell_start_log = temp.path().join("interactive-shell-starts");
    let shell_startup = temp.path().join("interactive-shell-startup");
    fs::write(
        &shell_startup,
        format!("printf x >> '{}'\n", shell_start_log.display()),
    )
    .unwrap();
    let _environment = EnvironmentGuard::set("ENV", &shell_startup);
    let socket = temp.path().join("topology-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![
            session(
                "alpha",
                &alpha_session,
                vec![
                    window(
                        3,
                        "editor main",
                        vec![
                            idle_pane(0, &alpha_editor_first),
                            idle_pane(1, &alpha_editor_second),
                        ],
                    ),
                    window(8, "logs", vec![idle_pane(0, &alpha_logs)]),
                ],
            ),
            session(
                "beta",
                &beta_session,
                vec![window(4, "operations", vec![idle_pane(0, &beta_ops)])],
            ),
        ],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()),
    )
    .unwrap();
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(
        result.status(),
        RestoreRunStatus::Complete,
        "restore failed: {result:?}"
    );
    assert_eq!(
        result.target_state(),
        &RestoreTargetState::Observed(TargetDisposition::Retained)
    );
    assert_eq!(result.panes().len(), 4);
    assert!(
        result
            .panes()
            .iter()
            .all(|pane| pane.outcome() == &PaneRestoreOutcome::RestoredIdleShell)
    );

    let rows = topology_rows(&socket);
    let actual_locations = rows
        .iter()
        .map(TopologyRow::location)
        .collect::<BTreeSet<_>>();
    let expected_locations = [
        (
            "alpha".to_owned(),
            alpha_session.to_string_lossy().into_owned(),
            3,
            "editor main".to_owned(),
            0,
            alpha_editor_first.to_string_lossy().into_owned(),
        ),
        (
            "alpha".to_owned(),
            alpha_session.to_string_lossy().into_owned(),
            3,
            "editor main".to_owned(),
            1,
            alpha_editor_second.to_string_lossy().into_owned(),
        ),
        (
            "alpha".to_owned(),
            alpha_session.to_string_lossy().into_owned(),
            8,
            "logs".to_owned(),
            0,
            alpha_logs.to_string_lossy().into_owned(),
        ),
        (
            "beta".to_owned(),
            beta_session.to_string_lossy().into_owned(),
            4,
            "operations".to_owned(),
            0,
            beta_ops.to_string_lossy().into_owned(),
        ),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(actual_locations, expected_locations);
    for row in rows {
        assert_eq!(row.default_shell, "/bin/sh");
        assert_eq!(row.pane_start_command, "/bin/sh -i");
        assert_eq!(row.pane_current_command, "sh");
    }
    assert_eq!(
        fs::read(&shell_start_log).unwrap(),
        b"xxxx",
        "each of the four restored panes must start its interactive shell once"
    );
}

#[test]
fn pastes_manual_input_literally_without_enter() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let session_directory = create_directory(temp.path(), "manual-session");
    let pane_directory = create_directory(temp.path(), "manual-pane");
    let marker = temp.path().join("manual wasn't executed");
    let socket = temp.path().join("manual-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "manual",
            &session_directory,
            vec![window(
                0,
                "manual hint",
                vec![manual_pane(0, &pane_directory, &marker)],
            )],
        )],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()),
    )
    .unwrap();
    let expected_input = match plan.panes()[0].action() {
        tmux_rescue::PlannedPaneAction::PasteManualHint { input, .. } => {
            String::from_utf8(input.as_bytes().to_vec()).unwrap()
        }
        action => panic!("expected a manual hint, got {action:?}"),
    };
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(
        result.status(),
        RestoreRunStatus::Complete,
        "restore failed: {result:?}"
    );
    assert_eq!(
        result.panes()[0].outcome(),
        &PaneRestoreOutcome::PreparedManualHint
    );
    thread::sleep(Duration::from_millis(100));
    let captured = String::from_utf8(tmux_stdout(
        &socket,
        &["capture-pane", "-p", "-J", "-S", "-20", "-t", "manual:0.0"],
    ))
    .unwrap();
    assert!(
        captured.contains(&expected_input),
        "pane did not contain the exact literal hint: {captured:?}"
    );
    assert!(!marker.exists(), "manual hint was submitted with Enter");
    assert_eq!(
        String::from_utf8(tmux_stdout(
            &socket,
            &[
                "display-message",
                "-p",
                "-t",
                "manual:0.0",
                "#{pane_current_command}"
            ],
        ))
        .unwrap()
        .trim(),
        "sh"
    );
}

#[test]
fn topology_failure_rolls_back_only_the_newly_owned_server() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let session_directory = create_directory(temp.path(), "rollback-session");
    let pane_directory = create_directory(temp.path(), "rollback-pane");
    let socket = temp.path().join("rollback-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "rollback",
            &session_directory,
            vec![window(0, "rollback", vec![idle_pane(0, &pane_directory)])],
        )],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()),
    )
    .unwrap();
    fs::remove_dir(&pane_directory).unwrap();
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(result.status(), RestoreRunStatus::Fatal);
    assert_eq!(
        result.target_state(),
        &RestoreTargetState::Observed(TargetDisposition::Removed)
    );
    assert!(matches!(
        result.failure(),
        Some(RestoreExecutionFailure::TopologyFailed { .. })
    ));
    assert!(
        !isolated_tmux(&socket)
            .arg("has-session")
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn automatic_recovery_sends_literal_input_then_a_separate_enter() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let book_directory = create_directory(temp.path(), "book");
    let source_directory = create_directory(&book_directory, "src");
    fs::write(
        book_directory.join("book.toml"),
        "[book]\ntitle = \"Restore test\"\n",
    )
    .unwrap();
    fs::write(source_directory.join("SUMMARY.md"), "# Summary\n").unwrap();
    let mdbook = require_success(
        "resolve mdbook",
        Command::new("sh")
            .args(["-c", "command -v mdbook"])
            .output()
            .unwrap(),
    );
    let mdbook = fs::canonicalize(Path::new(
        std::str::from_utf8(&mdbook).unwrap().trim_end_matches('\n'),
    ))
    .unwrap();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let socket = temp.path().join("automatic-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "automatic",
            &book_directory,
            vec![window(
                0,
                "mdbook",
                vec![automatic_mdbook_pane(0, &book_directory, &mdbook, port)],
            )],
        )],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()).with_executable(&mdbook),
    )
    .unwrap();
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(result.status(), RestoreRunStatus::Complete, "{result:#?}");
    assert_eq!(
        result.panes()[0].outcome(),
        &PaneRestoreOutcome::RecoveredAutomatically
    );
    assert_eq!(
        String::from_utf8(tmux_stdout(
            &socket,
            &[
                "display-message",
                "-p",
                "-t",
                "automatic:0.0",
                "#{pane_current_command}"
            ],
        ))
        .unwrap()
        .trim(),
        "mdbook"
    );
}

#[test]
fn changed_automatic_executable_is_not_launched() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let session_directory = create_directory(temp.path(), "identity-session");
    let pane_directory = create_directory(temp.path(), "identity-pane");
    let executable_directory = create_directory(temp.path(), "identity-bin");
    let executable = executable_directory.join("mdbook");
    fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = temp.path().join("identity-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "identity",
            &session_directory,
            vec![window(
                0,
                "identity",
                vec![automatic_mdbook_pane(
                    0,
                    &pane_directory,
                    &executable,
                    31_111,
                )],
            )],
        )],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()).with_executable(&executable),
    )
    .unwrap();
    fs::remove_file(&executable).unwrap();
    fs::write(&executable, b"#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(result.status(), RestoreRunStatus::Partial);
    assert!(
        matches!(
            result.panes()[0].outcome(),
            PaneRestoreOutcome::NeedsAttention(AttentionReason::GuardedOperationFailed(reason))
                if reason.contains("automatic executable changed")
        ),
        "{:#?}",
        result.panes()[0].outcome()
    );
    assert_eq!(topology_rows(&socket)[0].pane_current_command, "sh");
}
