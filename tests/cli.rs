use std::process::Command;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tmux-rescue"))
}

#[test]
fn explicit_restore_bypasses_state_root_and_prints_a_fatal_target_summary() {
    let temp = tempfile::tempdir().unwrap();
    let snapshot = temp.path().join("snapshot.json");
    let target = temp.path().join("occupied");
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
    std::fs::write(&target, b"occupied").unwrap();

    let output = binary()
        .arg("restore")
        .arg(&snapshot)
        .arg("--target")
        .arg(&target)
        .env_remove("XDG_STATE_HOME")
        .env_remove("HOME")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("restore: fatal"));
    assert!(stdout.contains("target state: not established"));
    assert!(stderr.contains("target state is indeterminate"));
    assert!(!stderr.contains("HOME is not set"));
}

#[test]
fn malformed_usage_does_not_reuse_the_partial_recovery_exit_code() {
    let output = binary().args(["snapshot", "--run"]).output().unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
}

#[test]
fn invalid_target_is_rejected_at_cli_parse_before_snapshot_io() {
    let output = binary()
        .args([
            "restore",
            "/definitely/missing/snapshot.json",
            "--target",
            "relative.sock",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--target requires an absolute socket path"));
    assert!(!stderr.contains("load snapshot"));
}
