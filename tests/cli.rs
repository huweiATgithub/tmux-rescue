use std::io::Write as _;
use std::process::Command;

use serde_json::json;
use tmux_rescue::{SnapshotPublication, StateStore, ValidatedSnapshot};

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tmux-rescue"))
}

fn encoded(value: &str) -> serde_json::Value {
    json!({"encoding": "utf8", "value": value})
}

fn inspect_fixture(consistency: serde_json::Value) -> serde_json::Value {
    json!({
        "captured_at": "2026-07-24T05:31:32.581307924+08:00",
        "source": encoded("/tmp/tmux-1000/default"),
        "consistency": consistency,
        "sessions": [{
            "name": "work",
            "working_directory": encoded("/workspace"),
            "windows": [{
                "source_index": 0,
                "name": "editor",
                "panes": [{
                    "source_index": 0,
                    "working_directory": encoded("/workspace"),
                    "recovery": {"kind": "idle"},
                }],
            }],
        }],
    })
}

fn publish_latest(state_home: &std::path::Path, value: &serde_json::Value) -> std::path::PathBuf {
    let snapshot = ValidatedSnapshot::from_json(&serde_json::to_vec(value).unwrap()).unwrap();
    let publication = StateStore::new(state_home.join("tmux-rescue")).publish(&snapshot);
    match publication {
        SnapshotPublication::Published { snapshot_path, .. } => snapshot_path,
        other => panic!("snapshot publication failed: {other:?}"),
    }
}

fn write_inspect_fixture(path: &std::path::Path) {
    std::fs::write(
        path,
        serde_json::to_vec(&inspect_fixture(json!({"kind": "stable"}))).unwrap(),
    )
    .unwrap();
}

fn strip_ansi(bytes: &[u8]) -> Vec<u8> {
    let mut stripped = anstream::StripStream::new(Vec::new());
    stripped.write_all(bytes).unwrap();
    stripped.into_inner()
}

#[test]
fn explicit_inspect_bypasses_state_root_and_live_systems() {
    let temp = tempfile::tempdir().unwrap();
    let snapshot = temp.path().join("snapshot.json");
    std::fs::write(
        &snapshot,
        serde_json::to_vec(&inspect_fixture(json!({"kind": "stable"}))).unwrap(),
    )
    .unwrap();

    let output = binary()
        .arg("inspect")
        .arg(&snapshot)
        .args(["--color", "never"])
        .env_remove("XDG_STATE_HOME")
        .env_remove("HOME")
        .env("TMUX", "/definitely/not/a/tmux/socket")
        .env("PATH", "/definitely/no/programs")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("Snapshot     explicit\n"));
    assert!(stdout.contains("Consistency  ● stable topology\n"));
    assert!(stdout.contains(&format!("File         {}\n", snapshot.display())));
    assert!(stdout.contains("◆ work · 1 window · 1 pane\n"));
    assert!(stdout.ends_with("└─ [0] editor › (0) shell\n      cwd = ◆\n"));
}

#[test]
fn explicit_inspect_renders_nerd_icons_for_a_compact_single_pane_window() {
    let temp = tempfile::tempdir().unwrap();
    let snapshot = temp.path().join("snapshot.json");
    write_inspect_fixture(&snapshot);

    let output = binary()
        .arg("inspect")
        .arg(&snapshot)
        .args(["--color", "never", "--icons", "nerd"])
        .env_remove("XDG_STATE_HOME")
        .env_remove("HOME")
        .env("TMUX", "/definitely/not/a/tmux/socket")
        .env("PATH", "/definitely/no/programs")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.ends_with(
        "◆ work · 1 window · 1 pane\n   /workspace\n└─  0 editor ›  0 shell\n       = ◆\n"
    ));
}

#[test]
fn unstable_latest_inspect_warns_and_keeps_rendering_after_unavailable_programs() {
    let temp = tempfile::tempdir().unwrap();
    let state_home = temp.path().join("state");
    let mut fixture = inspect_fixture(json!({"kind": "unstable", "attempts": 3}));
    fixture["sessions"][0]["windows"][0]["panes"] = json!([
        {
            "source_index": 0,
            "working_directory": encoded("/workspace/missing"),
            "recovery": {
                "kind": "unavailable",
                "failure": "foreground process disappeared",
            },
        },
        {
            "source_index": 1,
            "working_directory": encoded("/workspace"),
            "recovery": {"kind": "idle"},
        },
    ]);
    let selected_path = publish_latest(&state_home, &fixture);

    let output = binary()
        .arg("inspect")
        .args(["--color", "never"])
        .env("XDG_STATE_HOME", &state_home)
        .env_remove("HOME")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("Snapshot     latest\n"));
    assert!(stdout.contains(&format!("File         {}\n", selected_path.display())));
    assert!(stdout.contains("Consistency  ▲ unstable topology after 3 attempts\n"));
    let unavailable = stdout.find("(0) ! program not captured").unwrap();
    let later_pane = stdout.find("(1) shell").unwrap();
    assert!(unavailable < later_pane);
    assert!(stdout.ends_with("   └─ (1) shell\n         cwd = ◆\n"));
}

#[test]
fn explicit_relative_inspect_path_remains_relative_in_the_document() {
    let temp = tempfile::tempdir().unwrap();
    write_inspect_fixture(&temp.path().join("snapshot.json"));

    let output = binary()
        .current_dir(temp.path())
        .args(["inspect", "snapshot.json", "--color", "never"])
        .env_remove("XDG_STATE_HOME")
        .env_remove("HOME")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("File         snapshot.json\n"));
    assert!(!stdout.contains(&temp.path().display().to_string()));
}

#[test]
fn explicit_color_modes_override_redirected_stream_detection() {
    let temp = tempfile::tempdir().unwrap();
    let snapshot = temp.path().join("snapshot.json");
    write_inspect_fixture(&snapshot);

    let always = binary()
        .arg("inspect")
        .arg(&snapshot)
        .args(["--color", "always"])
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .output()
        .unwrap();
    let never = binary()
        .arg("inspect")
        .arg(&snapshot)
        .args(["--color", "never"])
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .output()
        .unwrap();

    assert_eq!(always.status.code(), Some(0));
    assert_eq!(never.status.code(), Some(0));
    assert!(always.stderr.is_empty());
    assert!(never.stderr.is_empty());
    assert!(always.stdout.windows(2).any(|window| window == b"\x1b["));
    assert!(!never.stdout.windows(2).any(|window| window == b"\x1b["));
    assert_eq!(strip_ansi(&always.stdout), never.stdout);
}

#[test]
fn automatic_color_honors_force_and_no_color_environment() {
    let temp = tempfile::tempdir().unwrap();
    let snapshot = temp.path().join("snapshot.json");
    write_inspect_fixture(&snapshot);

    let forced = binary()
        .arg("inspect")
        .arg(&snapshot)
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .output()
        .unwrap();
    let disabled = binary()
        .arg("inspect")
        .arg(&snapshot)
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .output()
        .unwrap();

    assert_eq!(forced.status.code(), Some(0));
    assert_eq!(disabled.status.code(), Some(0));
    assert!(forced.stdout.windows(2).any(|window| window == b"\x1b["));
    assert!(!disabled.stdout.windows(2).any(|window| window == b"\x1b["));
    assert_eq!(strip_ansi(&forced.stdout), disabled.stdout);
}

#[test]
fn invalid_inspect_is_fatal_with_empty_stdout_and_a_token_local_error_color() {
    let temp = tempfile::tempdir().unwrap();
    let snapshot = temp.path().join("invalid.json");
    std::fs::write(&snapshot, b"{}").unwrap();

    let output = binary()
        .arg("inspect")
        .arg(&snapshot)
        .args(["--color", "always"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("\x1b[31merror:\x1b[0m load snapshot: "));
    assert!(stderr.contains("snapshot validation failed"));
    assert_eq!(stderr.matches('\x1b').count(), 2);
}

#[test]
fn missing_latest_inspect_is_fatal_without_a_partial_document() {
    let temp = tempfile::tempdir().unwrap();
    let state_home = temp.path().join("state");

    let output = binary()
        .arg("inspect")
        .args(["--color", "never"])
        .env("XDG_STATE_HOME", &state_home)
        .env_remove("HOME")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("error: load snapshot: latest pointer is invalid:"));
    assert!(!stderr.contains("Snapshot     "));
}

#[test]
fn explicit_restore_bypasses_state_root_and_renders_the_exact_selector() {
    let temp = tempfile::tempdir().unwrap();
    let snapshot = temp.path().join("snapshot.json");
    std::fs::write(
        &snapshot,
        serde_json::to_vec(&serde_json::json!({
            "captured_at": "2026-07-23T00:00:00Z",
            "source": {"encoding": "utf8", "value": "/tmp/source.sock"},
            "consistency": {"kind": "stable"},
            "sessions": [{
                "name": "work",
                "working_directory": {"encoding": "utf8", "value": "/tmp"},
                "windows": [{
                    "source_index": 0,
                    "name": "work",
                    "panes": [{
                        "source_index": 0,
                        "working_directory": {"encoding": "utf8", "value": "/tmp"},
                        "recovery": {"kind": "idle"}
                    }]
                }]
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let output = binary()
        .args(["-L", "abc", "restore"])
        .arg(&snapshot)
        .env_remove("XDG_STATE_HOME")
        .env_remove("HOME")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.starts_with("target: -L abc\n"));
    assert!(stderr.is_empty(), "{stderr}");
    assert!(!stderr.contains("HOME is not set"));
}

#[test]
fn malformed_usage_does_not_reuse_the_partial_recovery_exit_code() {
    let output = binary().args(["snapshot", "--run"]).output().unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
}

#[test]
fn selector_after_restore_is_rejected_before_snapshot_io() {
    let output = binary()
        .args([
            "restore",
            "/definitely/missing/snapshot.json",
            "-S",
            "relative.sock",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument '-S'"));
    assert!(!stderr.contains("load snapshot"));
}
