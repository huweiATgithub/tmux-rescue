use serde_json::{Value, json};
use tmux_rescue::{
    AutomaticRecovery, MAX_DIAGNOSTIC_BYTES, MAX_OS_VALUE_BYTES, MAX_SESSIONS,
    MAX_TOPOLOGY_VALIDATION_ATTEMPTS, PaneRecovery, SnapshotValidationError, ValidatedSnapshot,
};

fn encoded(value: &str) -> Value {
    json!({"encoding": "utf8", "value": value})
}

fn valid_snapshot() -> Value {
    json!({
        "captured_at": "2026-07-23T00:00:00Z",
        "source": encoded("/tmp/source.sock"),
        "consistency": {"kind": "stable"},
        "sessions": [{
            "name": "work",
            "working_directory": encoded("/tmp/work"),
            "windows": [{
                "source_index": 1,
                "name": "editor",
                "panes": [
                    {
                        "source_index": 0,
                        "working_directory": encoded("/tmp/work"),
                        "recovery": {"kind": "idle"}
                    },
                    {
                        "source_index": 1,
                        "working_directory": encoded("/tmp/work/sub"),
                        "recovery": {
                            "kind": "manual",
                            "command": {
                                "executable": encoded("/usr/bin/printf"),
                                "argv": [
                                    encoded("printf"),
                                    encoded(""),
                                    {"encoding": "base64", "value": "/w=="}
                                ]
                            }
                        }
                    }
                ]
            }]
        }]
    })
}

fn parse(value: &Value) -> Result<ValidatedSnapshot, SnapshotValidationError> {
    ValidatedSnapshot::from_json(&serde_json::to_vec(value).unwrap())
}

#[test]
fn rejects_an_empty_session_tree() {
    let raw = br#"{
      "captured_at":"2026-07-23T00:00:00Z",
      "source":{"encoding":"utf8","value":"/tmp/source.sock"},
      "consistency":{"kind":"stable"},
      "sessions":[]
    }"#;

    assert_eq!(
        ValidatedSnapshot::from_json(raw).unwrap_err(),
        SnapshotValidationError::EmptySessions
    );
}

#[test]
fn rejects_unknown_fields_in_tagged_snapshot_variants() {
    let mut consistency = valid_snapshot();
    consistency["consistency"]["unexpected"] = json!(true);
    assert!(matches!(
        parse(&consistency),
        Err(SnapshotValidationError::InvalidJson(_))
    ));

    let mut pane_recovery = valid_snapshot();
    pane_recovery["sessions"][0]["windows"][0]["panes"][0]["recovery"]["command"] =
        json!({"unexpected": true});
    assert!(matches!(
        parse(&pane_recovery),
        Err(SnapshotValidationError::InvalidJson(_))
    ));

    let mut automatic = valid_snapshot();
    automatic["sessions"][0]["windows"][0]["panes"][0]["recovery"] = json!({
        "kind": "automatic",
        "recovery": {
            "kind": "codex",
            "session_id": "b0cd1f37-9d8e-4d50-bda5-90538fd63343",
            "command": {"unexpected": true}
        }
    });
    assert!(matches!(
        parse(&automatic),
        Err(SnapshotValidationError::InvalidJson(_))
    ));
}

#[test]
fn validates_and_losslessly_round_trips_a_complete_snapshot() {
    let snapshot = parse(&valid_snapshot()).unwrap();

    assert_eq!(snapshot.source().path().as_bytes(), b"/tmp/source.sock");
    assert_eq!(snapshot.sessions()[0].name(), "work");
    assert_eq!(snapshot.sessions()[0].windows()[0].name(), "editor");
    let panes = snapshot.sessions()[0].windows()[0].panes();
    let PaneRecovery::Manual(command) = panes[1].recovery() else {
        panic!("expected a manual command");
    };
    assert_eq!(command.argv()[1].as_bytes(), b"");
    assert_eq!(command.argv()[2].as_bytes(), &[0xff]);

    let serialized = snapshot.to_json_pretty().unwrap();
    assert_eq!(ValidatedSnapshot::from_json(&serialized).unwrap(), snapshot);
}

#[test]
fn rejects_duplicate_names_and_scoped_indexes() {
    let mut duplicate_session = valid_snapshot();
    let session = duplicate_session["sessions"][0].clone();
    duplicate_session["sessions"]
        .as_array_mut()
        .unwrap()
        .push(session);
    assert!(matches!(
        parse(&duplicate_session),
        Err(SnapshotValidationError::DuplicateSessionName { .. })
    ));

    let mut duplicate_window = valid_snapshot();
    let window = duplicate_window["sessions"][0]["windows"][0].clone();
    duplicate_window["sessions"][0]["windows"]
        .as_array_mut()
        .unwrap()
        .push(window);
    assert!(matches!(
        parse(&duplicate_window),
        Err(SnapshotValidationError::DuplicateWindowIndex { .. })
    ));

    let mut duplicate_pane = valid_snapshot();
    duplicate_pane["sessions"][0]["windows"][0]["panes"][1]["source_index"] = json!(0);
    assert!(matches!(
        parse(&duplicate_pane),
        Err(SnapshotValidationError::DuplicatePaneIndex { .. })
    ));
}

#[test]
fn rejects_empty_nested_collections() {
    let mut no_windows = valid_snapshot();
    no_windows["sessions"][0]["windows"] = json!([]);
    assert!(matches!(
        parse(&no_windows),
        Err(SnapshotValidationError::EmptyWindows { .. })
    ));

    let mut no_panes = valid_snapshot();
    no_panes["sessions"][0]["windows"][0]["panes"] = json!([]);
    assert!(matches!(
        parse(&no_panes),
        Err(SnapshotValidationError::EmptyPanes { .. })
    ));
}

#[test]
fn rejects_relative_recorded_paths() {
    for pointer in [
        "/source/value",
        "/sessions/0/working_directory/value",
        "/sessions/0/windows/0/panes/0/working_directory/value",
    ] {
        let mut value = valid_snapshot();
        *value.pointer_mut(pointer).unwrap() = json!("relative/path");
        assert!(matches!(
            parse(&value),
            Err(SnapshotValidationError::PathNotAbsolute { .. })
        ));
    }
}

#[test]
fn rejects_non_exhausted_unstable_attempt_counts() {
    let mut value = valid_snapshot();
    value["consistency"] = json!({
        "kind": "unstable",
        "attempts": MAX_TOPOLOGY_VALIDATION_ATTEMPTS - 1
    });

    assert_eq!(
        parse(&value).unwrap_err(),
        SnapshotValidationError::InvalidUnstableAttemptCount {
            actual: MAX_TOPOLOGY_VALIDATION_ATTEMPTS - 1,
            expected: MAX_TOPOLOGY_VALIDATION_ATTEMPTS,
        }
    );
}

#[test]
fn rejects_invalid_command_boundaries_but_preserves_later_empty_arguments() {
    let mut no_argv = valid_snapshot();
    no_argv["sessions"][0]["windows"][0]["panes"][1]["recovery"]["command"]["argv"] = json!([]);
    assert!(matches!(
        parse(&no_argv),
        Err(SnapshotValidationError::EmptyArgv { .. })
    ));

    let mut empty_argv_zero = valid_snapshot();
    empty_argv_zero["sessions"][0]["windows"][0]["panes"][1]["recovery"]["command"]["argv"][0] =
        encoded("");
    assert!(matches!(
        parse(&empty_argv_zero),
        Err(SnapshotValidationError::EmptyArgvZero { .. })
    ));
}

#[test]
fn rejects_invalid_lossless_encodings_and_nul_bytes() {
    let mut bad_base64 = valid_snapshot();
    bad_base64["source"] = json!({"encoding": "base64", "value": "not base64"});
    assert!(matches!(
        parse(&bad_base64),
        Err(SnapshotValidationError::InvalidOsEncoding { .. })
    ));

    let mut nul = valid_snapshot();
    nul["source"] = json!({"encoding": "base64", "value": "L3RtcC8Ac29jaw=="});
    assert!(matches!(
        parse(&nul),
        Err(SnapshotValidationError::OsValueContainsNul { .. })
    ));

    let mut noncanonical = valid_snapshot();
    noncanonical["source"] = json!({
        "encoding": "base64",
        "value": "L3RtcC9zb3VyY2Uuc29jaw=="
    });
    assert!(matches!(
        parse(&noncanonical),
        Err(SnapshotValidationError::InvalidOsEncoding { .. })
    ));
}

#[test]
fn rejects_incoherent_automatic_recovery_payloads() {
    let mut invalid_uuid = valid_snapshot();
    invalid_uuid["sessions"][0]["windows"][0]["panes"][0]["recovery"] = json!({
        "kind": "automatic",
        "recovery": {"kind": "codex", "session_id": "not-a-uuid"}
    });
    assert!(matches!(
        parse(&invalid_uuid),
        Err(SnapshotValidationError::InvalidSessionId { .. })
    ));

    let mut wrong_serve = valid_snapshot();
    wrong_serve["sessions"][0]["windows"][0]["panes"][0]["recovery"] = json!({
        "kind": "automatic",
        "recovery": {
            "kind": "md_book_serve",
            "command": {
                "executable": encoded("/usr/bin/mdbook"),
                "argv": [encoded("mdbook"), encoded("build")]
            }
        }
    });
    assert!(matches!(
        parse(&wrong_serve),
        Err(SnapshotValidationError::InvalidRecognizedServeCommand { .. })
    ));
}

#[test]
fn preserves_distinct_automatic_variants() {
    let mut value = valid_snapshot();
    value["sessions"][0]["windows"][0]["panes"][0]["recovery"] = json!({
        "kind": "automatic",
        "recovery": {
            "kind": "claude_code",
            "session_id": "8f707f38-6fd3-4a11-a03f-853b03d47b0c"
        }
    });
    let snapshot = parse(&value).unwrap();

    assert!(matches!(
        snapshot.sessions()[0].windows()[0].panes()[0].recovery(),
        PaneRecovery::Automatic(AutomaticRecovery::ClaudeCode { .. })
    ));
}

#[test]
fn rejects_invalid_time_and_defensive_limit_violations() {
    let mut invalid_time = valid_snapshot();
    invalid_time["captured_at"] = json!("tomorrow");
    assert!(matches!(
        parse(&invalid_time),
        Err(SnapshotValidationError::InvalidCaptureTime { .. })
    ));

    let mut too_many_sessions = valid_snapshot();
    let session = too_many_sessions["sessions"][0].clone();
    too_many_sessions["sessions"] = Value::Array(vec![session; MAX_SESSIONS + 1]);
    assert!(matches!(
        parse(&too_many_sessions),
        Err(SnapshotValidationError::TooManySessions { .. })
    ));

    let mut path_too_long = valid_snapshot();
    path_too_long["source"]["value"] = json!(format!("/{}", "x".repeat(MAX_OS_VALUE_BYTES)));
    assert!(matches!(
        parse(&path_too_long),
        Err(SnapshotValidationError::OsValueTooLong { .. })
    ));

    let mut diagnostic_too_long = valid_snapshot();
    diagnostic_too_long["sessions"][0]["windows"][0]["panes"][0]["recovery"] = json!({
        "kind": "unavailable",
        "failure": "x".repeat(MAX_DIAGNOSTIC_BYTES + 1)
    });
    assert!(matches!(
        parse(&diagnostic_too_long),
        Err(SnapshotValidationError::InvalidCaptureFailure { .. })
    ));
}
