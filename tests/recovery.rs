use tmux_rescue::{
    AutomaticRecovery, AutomaticRecoveryExpectation, CapturedCommand, ForegroundProcessMember,
    LosslessOsString, OpenedClaudeSessionFile, OpenedCodexSessionFile, PaneRecovery,
    PaneTiedForegroundEvidence, RecordedAbsolutePath, ResolverOutcome, SessionTool,
    ToolAttributedTailSession, classify_pane, derive_automatic_command,
};

fn os(value: &str) -> LosslessOsString {
    LosslessOsString::try_from_bytes(value.as_bytes().to_vec()).unwrap()
}

fn command(executable: &str, argv: &[&str]) -> CapturedCommand {
    CapturedCommand::try_new(
        os(executable),
        argv.iter().map(|argument| os(argument)).collect(),
    )
    .unwrap()
}

fn evidence(executable: &str, argv: &[&str]) -> PaneTiedForegroundEvidence {
    PaneTiedForegroundEvidence::try_new(
        command(executable, argv),
        RecordedAbsolutePath::try_from_bytes(b"/tmp/work".to_vec()).unwrap(),
        os("/dev/pts/42"),
        os("/dev/pts/42"),
        12_345,
        12_345,
        12_345,
        99,
    )
    .unwrap()
}

fn path(value: &str) -> RecordedAbsolutePath {
    RecordedAbsolutePath::try_from_bytes(value.as_bytes().to_vec()).unwrap()
}

#[test]
fn recognizes_only_exact_mdbook_serve_argv() {
    let classification = classify_pane(evidence(
        "/usr/bin/mdbook",
        &["mdbook", "serve", "-p", "3000"],
    ));

    assert!(matches!(
        classification.recovery(),
        PaneRecovery::Automatic(AutomaticRecovery::MdBookServe { .. })
    ));
}

#[test]
fn serve_recovery_uses_a_fixed_target_command_name() {
    let classification = classify_pane(evidence(
        "/untrusted/location/mdbook",
        &["/untrusted/location/mdbook", "serve", "-p", "3000"],
    ));
    let PaneRecovery::Automatic(recovery) = classification.recovery() else {
        panic!("expected recognized serve recovery");
    };

    let command = derive_automatic_command(recovery);
    assert_eq!(command.argv()[0].as_bytes(), b"mdbook");
    assert_eq!(command.argv()[1].as_bytes(), b"serve");
}

#[test]
fn recognizes_only_exact_bookshelf_serve_argv() {
    let classification = classify_pane(evidence(
        "/home/user/.cargo/bin/book",
        &["book", "serve", "--open"],
    ));

    assert!(matches!(
        classification.recovery(),
        PaneRecovery::Automatic(AutomaticRecovery::BookshelfServe { .. })
    ));
}

#[test]
fn downgrades_other_readable_commands_to_manual_without_losing_argv() {
    let classification = classify_pane(evidence(
        "/usr/bin/mdbook",
        &["mdbook", "build", "", "--dest-dir", "out"],
    ));

    let PaneRecovery::Manual(command) = classification.recovery() else {
        panic!("expected a manual recovery");
    };
    assert_eq!(command.argv()[0].as_bytes(), b"mdbook");
    assert_eq!(command.argv()[1].as_bytes(), b"build");
    assert_eq!(command.argv()[2].as_bytes(), b"");
    assert_eq!(command.argv()[4].as_bytes(), b"out");
}

#[test]
fn executable_and_argv_zero_must_both_match_the_whitelist() {
    let classification = classify_pane(evidence(
        "/tmp/not-mdbook",
        &["mdbook", "serve", "-p", "3000"],
    ));

    assert!(matches!(classification.recovery(), PaneRecovery::Manual(_)));
}

#[test]
fn serve_success_matches_replayed_argv_across_target_executable_paths() {
    let source = classify_pane(evidence(
        "/source/bin/mdbook",
        &["mdbook", "serve", "-p", "3000"],
    ));
    let target = classify_pane(evidence(
        "/target/bin/mdbook",
        &["/target/bin/mdbook", "serve", "-p", "3000"],
    ));
    let PaneRecovery::Automatic(source) = source.recovery() else {
        panic!("expected recognized source command");
    };
    let PaneRecovery::Automatic(target) = target.recovery() else {
        panic!("expected recognized target command");
    };

    assert!(AutomaticRecoveryExpectation::from(source).matches(target));
}

#[test]
fn resolves_codex_from_one_exact_opened_root_session_record() {
    let session_id = "1d6381bf-01c5-4c4a-b725-8e376e5ad295";
    let native_member = ForegroundProcessMember::try_new(
        12_346,
        12_345,
        12_345,
        100,
        os("/dev/pts/42"),
        command("/opt/codex/vendor/codex", &["codex"]),
        path("/tmp/work"),
    )
    .unwrap();
    let session_file = OpenedCodexSessionFile::try_new(
        12_346,
        8,
        42,
        path(&format!(
            "/home/user/.codex/sessions/2026/07/23/rollout-{session_id}.jsonl"
        )),
        format!(
            r#"{{"type":"session_meta","payload":{{"id":"{session_id}","originator":"codex-tui","thread_source":"user","cwd":"/tmp/work","parent_thread_id":null}}}}"#
        )
        .into_bytes(),
    )
    .unwrap();
    let evidence = evidence("/usr/bin/node", &["node", "/opt/codex/bin/codex.js"])
        .with_foreground_members(vec![native_member])
        .unwrap()
        .with_codex_session_evidence(path("/home/user/.codex/sessions"), vec![session_file])
        .unwrap();

    let classification = classify_pane(evidence);
    let PaneRecovery::Automatic(recovery @ AutomaticRecovery::Codex { session_id: id, .. }) =
        classification.recovery()
    else {
        panic!("expected automatic Codex recovery");
    };
    assert_eq!(id.as_uuid().to_string(), session_id);
    let command = derive_automatic_command(recovery);
    assert_eq!(
        command
            .argv()
            .iter()
            .map(|value| value.as_bytes())
            .collect::<Vec<_>>(),
        vec![
            b"codex".as_slice(),
            b"resume".as_slice(),
            session_id.as_bytes()
        ]
    );
}

#[test]
fn public_raw_codex_suffix_cannot_forge_unlinked_identity() {
    let session_id = "1d6381bf-01c5-4c4a-b725-8e376e5ad295";
    let session_file = OpenedCodexSessionFile::try_new(
        12_345,
        8,
        42,
        path(&format!(
            "/home/user/.codex/sessions/2026/07/23/rollout-{session_id}.jsonl"
        )),
        format!(
            r#"{{"type":"session_meta","payload":{{"id":"{session_id}","originator":"codex-tui","thread_source":"user","cwd":"/tmp/work","parent_thread_id":null}}}}"#
        )
        .into_bytes(),
    )
    .unwrap();
    let evidence = evidence("/tmp/codex (deleted)", &["codex"])
        .with_codex_session_evidence(path("/home/user/.codex/sessions"), vec![session_file])
        .unwrap();

    let classification = classify_pane(evidence);
    let PaneRecovery::Manual(command) = classification.recovery() else {
        panic!("public raw evidence must remain manual");
    };
    assert_eq!(command.executable().as_bytes(), b"/tmp/codex (deleted)");
}

#[test]
fn conflicting_codex_root_ids_downgrade_to_manual() {
    let ids = [
        "1d6381bf-01c5-4c4a-b725-8e376e5ad295",
        "a27834ae-6192-4287-a005-86063335c28e",
    ];
    let files = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            OpenedCodexSessionFile::try_new(
                12_345,
                8,
                42 + index as u64,
                path(&format!("/home/user/.codex/sessions/rollout-{id}.jsonl")),
                format!(
                    r#"{{"type":"session_meta","payload":{{"id":"{id}","originator":"codex-tui","thread_source":"user","cwd":"/tmp/work","parent_thread_id":null}}}}"#
                )
                .into_bytes(),
            )
            .unwrap()
        })
        .collect();
    let evidence = evidence("/usr/bin/codex", &["codex"])
        .with_codex_session_evidence(path("/home/user/.codex/sessions"), files)
        .unwrap();

    let classification = classify_pane(evidence);
    assert!(matches!(classification.recovery(), PaneRecovery::Manual(_)));
    assert!(matches!(
        classification.resolver_outcome(),
        ResolverOutcome::ConflictingEvidence(_)
    ));
}

#[test]
fn multiple_codex_candidates_with_one_id_still_downgrade_to_manual() {
    let session_id = "1d6381bf-01c5-4c4a-b725-8e376e5ad295";
    let files = [41, 42]
        .into_iter()
        .map(|inode| {
            OpenedCodexSessionFile::try_new(
                12_345,
                8,
                inode,
                path(&format!("/home/user/.codex/sessions/rollout-{inode}.jsonl")),
                format!(
                    r#"{{"type":"session_meta","payload":{{"id":"{session_id}","originator":"codex-tui","thread_source":"user","cwd":"/tmp/work","parent_thread_id":null}}}}"#
                )
                .into_bytes(),
            )
            .unwrap()
        })
        .collect();
    let evidence = evidence("/usr/bin/codex", &["codex"])
        .with_codex_session_evidence(path("/home/user/.codex/sessions"), files)
        .unwrap();

    assert!(matches!(
        classify_pane(evidence).resolver_outcome(),
        ResolverOutcome::ConflictingEvidence(_)
    ));
}

#[test]
fn a_non_codex_group_member_cannot_supply_codex_identity() {
    let session_id = "1d6381bf-01c5-4c4a-b725-8e376e5ad295";
    let helper = ForegroundProcessMember::try_new(
        12_346,
        12_345,
        12_345,
        100,
        os("/dev/pts/42"),
        command("/usr/bin/helper", &["helper"]),
        path("/tmp/work"),
    )
    .unwrap();
    let file = OpenedCodexSessionFile::try_new(
        12_346,
        8,
        41,
        path("/home/user/.codex/sessions/rollout.jsonl"),
        format!(
            r#"{{"type":"session_meta","payload":{{"id":"{session_id}","originator":"codex-tui","thread_source":"user","cwd":"/tmp/work","parent_thread_id":null}}}}"#
        )
        .into_bytes(),
    )
    .unwrap();
    let evidence = evidence("/usr/bin/codex", &["codex"])
        .with_foreground_members(vec![helper])
        .unwrap()
        .with_codex_session_evidence(path("/home/user/.codex/sessions"), vec![file])
        .unwrap();

    assert!(matches!(
        classify_pane(evidence).resolver_outcome(),
        ResolverOutcome::InsufficientEvidence(_)
    ));
}

#[test]
fn multiple_recognized_tools_in_one_foreground_group_are_conflicting() {
    let codex_id = "1d6381bf-01c5-4c4a-b725-8e376e5ad295";
    let claude_id = "27ea5a6d-5b84-4770-998e-a1a8285b0e9a";
    let claude_member = ForegroundProcessMember::try_new(
        12_346,
        12_345,
        12_345,
        100,
        os("/dev/pts/42"),
        command("/usr/bin/claude", &["claude", "--session-id", claude_id]),
        path("/tmp/work"),
    )
    .unwrap();
    let codex_record = OpenedCodexSessionFile::try_new(
        12_345,
        8,
        42,
        path("/home/user/.codex/sessions/rollout.jsonl"),
        format!(
            r#"{{"type":"session_meta","payload":{{"id":"{codex_id}","originator":"codex-tui","thread_source":"user","cwd":"/tmp/work","parent_thread_id":null}}}}"#
        )
        .into_bytes(),
    )
    .unwrap();
    let evidence = evidence("/usr/bin/codex", &["codex"])
        .with_foreground_members(vec![claude_member])
        .unwrap()
        .with_codex_session_evidence(path("/home/user/.codex/sessions"), vec![codex_record])
        .unwrap();

    let classification = classify_pane(evidence);

    assert!(matches!(classification.recovery(), PaneRecovery::Manual(_)));
    assert!(matches!(
        classification.resolver_outcome(),
        ResolverOutcome::ConflictingEvidence(_)
    ));
}

#[test]
fn resolves_only_exact_claude_uuid_flags() {
    let session_id = "b0cd1f37-9d8e-4d50-bda5-90538fd63343";
    let classification = classify_pane(evidence(
        "/home/user/.local/share/claude/versions/2.1.195",
        &["claude", "--resume", session_id],
    ));

    let PaneRecovery::Automatic(recovery @ AutomaticRecovery::ClaudeCode { session_id: id }) =
        classification.recovery()
    else {
        panic!("expected automatic Claude recovery");
    };
    assert_eq!(id.as_uuid().to_string(), session_id);
    let command = derive_automatic_command(recovery);
    assert_eq!(
        command
            .argv()
            .iter()
            .map(|value| value.as_bytes())
            .collect::<Vec<_>>(),
        vec![
            b"claude".as_slice(),
            b"--resume".as_slice(),
            session_id.as_bytes()
        ]
    );

    let picker = classify_pane(evidence("/usr/bin/claude", &["claude", "--resume"]));
    assert!(matches!(picker.recovery(), PaneRecovery::Manual(_)));
    assert!(matches!(
        picker.resolver_outcome(),
        ResolverOutcome::InsufficientEvidence(_)
    ));
}

#[test]
fn tool_names_in_spoofed_argv_or_prompt_data_do_not_authorize_recovery() {
    let session_id = "b0cd1f37-9d8e-4d50-bda5-90538fd63343";
    for evidence in [
        evidence("/usr/bin/unrelated", &["claude", "--resume", session_id]),
        evidence("/usr/bin/claude", &["claude", "--", "--resume", session_id]),
    ] {
        assert!(matches!(
            classify_pane(evidence).recovery(),
            PaneRecovery::Manual(_)
        ));
    }

    let codex_record = OpenedCodexSessionFile::try_new(
        12_345,
        8,
        41,
        path("/home/user/.codex/sessions/rollout.jsonl"),
        format!(
            r#"{{"type":"session_meta","payload":{{"id":"{session_id}","originator":"codex-tui","thread_source":"user","cwd":"/tmp/work","parent_thread_id":null}}}}"#
        )
        .into_bytes(),
    )
    .unwrap();
    let spoofed_codex = evidence("/usr/bin/unrelated", &["codex"])
        .with_codex_session_evidence(path("/home/user/.codex/sessions"), vec![codex_record])
        .unwrap();
    assert!(matches!(
        classify_pane(spoofed_codex).recovery(),
        PaneRecovery::Manual(_)
    ));
}

#[test]
fn conflicting_claude_flags_downgrade_to_manual() {
    let classification = classify_pane(evidence(
        "/usr/bin/claude",
        &[
            "claude",
            "--session-id",
            "b0cd1f37-9d8e-4d50-bda5-90538fd63343",
            "--resume=a27834ae-6192-4287-a005-86063335c28e",
        ],
    ));

    assert!(matches!(classification.recovery(), PaneRecovery::Manual(_)));
    assert!(matches!(
        classification.resolver_outcome(),
        ResolverOutcome::ConflictingEvidence(_)
    ));
}

#[test]
fn resolves_claude_from_an_exact_interactive_process_record() {
    let session_id = "27ea5a6d-5b84-4770-998e-a1a8285b0e9a";
    let record = OpenedClaudeSessionFile::try_new(
        12_345,
        path("/home/user/.claude/sessions/12345.json"),
        format!(
            r#"{{"pid":12345,"sessionId":"{session_id}","cwd":"/tmp/work","procStart":"99","kind":"interactive","entrypoint":"cli","transport":{{"kind":"pty","identity":"/dev/pts/42"}}}}"#
        )
        .into_bytes(),
    )
    .unwrap();
    let evidence = evidence(
        "/home/user/.local/share/claude/versions/2.1.195",
        &["claude"],
    )
    .with_claude_session_evidence(path("/home/user/.claude/sessions"), vec![record])
    .unwrap();

    let classification = classify_pane(evidence);
    assert!(matches!(
        classification.recovery(),
        PaneRecovery::Automatic(AutomaticRecovery::ClaudeCode { session_id: id })
            if id.as_uuid().to_string() == session_id
    ));
}

#[test]
fn matching_claude_argv_and_process_record_are_one_candidate() {
    let session_id = "27ea5a6d-5b84-4770-998e-a1a8285b0e9a";
    let record = OpenedClaudeSessionFile::try_new(
        12_345,
        path("/home/user/.claude/sessions/12345.json"),
        format!(
            r#"{{"pid":12345,"sessionId":"{session_id}","cwd":"/tmp/work","procStart":"99","kind":"interactive","transport":{{"kind":"pty","identity":"/dev/pts/42"}}}}"#
        )
        .into_bytes(),
    )
    .unwrap();
    let evidence = evidence(
        "/home/user/.local/share/claude/versions/2.1.195",
        &["claude", "--resume", session_id],
    )
    .with_claude_session_evidence(path("/home/user/.claude/sessions"), vec![record])
    .unwrap();

    assert!(matches!(
        classify_pane(evidence).recovery(),
        PaneRecovery::Automatic(AutomaticRecovery::ClaudeCode { session_id: id })
            if id.as_uuid().to_string() == session_id
    ));
}

#[test]
fn claude_background_and_service_modes_are_not_tuis() {
    let session_id = "27ea5a6d-5b84-4770-998e-a1a8285b0e9a";
    for argv in [
        vec!["claude", "--background", "--session-id", session_id],
        vec!["claude", "--bg", "--session-id", session_id],
        vec!["claude", "--help", "--session-id", session_id],
        vec!["claude", "--version", "--session-id", session_id],
        vec!["claude", "--resume", session_id, "--fork-session"],
        vec!["claude", "--future-mode", "--session-id", session_id],
        vec!["claude", "agents", "--session-id", session_id],
        vec!["claude", "gateway", "--session-id", session_id],
    ] {
        assert!(matches!(
            classify_pane(evidence("/usr/bin/claude", &argv)).recovery(),
            PaneRecovery::Manual(_)
        ));
    }
}

#[test]
fn claude_process_records_require_one_matching_pane_transport() {
    let session_id = "27ea5a6d-5b84-4770-998e-a1a8285b0e9a";
    for record in [
        format!(
            r#"{{"pid":12345,"sessionId":"{session_id}","cwd":"/tmp/work","procStart":"99","kind":"interactive"}}"#
        ),
        format!(
            r#"{{"pid":12345,"sessionId":"{session_id}","cwd":"/tmp/work","procStart":"99","kind":"interactive","transport":{{"kind":"pty","identity":"/dev/pts/99"}}}}"#
        ),
    ] {
        let record = OpenedClaudeSessionFile::try_new(
            12_345,
            path("/home/user/.claude/sessions/12345.json"),
            record.into_bytes(),
        )
        .unwrap();
        let evidence = evidence(
            "/home/user/.local/share/claude/versions/2.1.195",
            &["claude"],
        )
        .with_claude_session_evidence(path("/home/user/.claude/sessions"), vec![record])
        .unwrap();

        assert!(matches!(
            classify_pane(evidence).resolver_outcome(),
            ResolverOutcome::InsufficientEvidence(_)
        ));
    }

    let records = ["12345.json", "12345.json"]
        .into_iter()
        .map(|name| {
            OpenedClaudeSessionFile::try_new(
                12_345,
                path(&format!("/home/user/.claude/sessions/{name}")),
                format!(
                    r#"{{"pid":12345,"sessionId":"{session_id}","cwd":"/tmp/work","procStart":"99","kind":"interactive","transport":{{"kind":"pty","identity":"/dev/pts/42"}}}}"#
                )
                .into_bytes(),
            )
            .unwrap()
        })
        .collect();
    let duplicate = evidence(
        "/home/user/.local/share/claude/versions/2.1.195",
        &["claude"],
    )
    .with_claude_session_evidence(path("/home/user/.claude/sessions"), records)
    .unwrap();
    assert!(matches!(
        classify_pane(duplicate).resolver_outcome(),
        ResolverOutcome::ConflictingEvidence(_)
    ));
}

#[test]
fn rejects_noninteractive_or_conflicting_claude_records() {
    let argv_id = "b0cd1f37-9d8e-4d50-bda5-90538fd63343";
    let record_id = "a27834ae-6192-4287-a005-86063335c28e";
    let conflicting_record = OpenedClaudeSessionFile::try_new(
        12_345,
        path("/home/user/.claude/sessions/12345.json"),
        format!(
            r#"{{"pid":12345,"sessionId":"{record_id}","cwd":"/tmp/work","procStart":"99","kind":"interactive","transport":{{"kind":"pty","identity":"/dev/pts/42"}}}}"#
        )
        .into_bytes(),
    )
    .unwrap();
    let conflicting = evidence(
        "/home/user/.local/share/claude/versions/2.1.195",
        &["claude", "--session-id", argv_id],
    )
    .with_claude_session_evidence(
        path("/home/user/.claude/sessions"),
        vec![conflicting_record],
    )
    .unwrap();
    let classification = classify_pane(conflicting);
    assert!(matches!(
        classification.resolver_outcome(),
        ResolverOutcome::ConflictingEvidence(_)
    ));

    let background_record = OpenedClaudeSessionFile::try_new(
        12_345,
        path("/home/user/.claude/sessions/12345.json"),
        format!(
            r#"{{"pid":12345,"sessionId":"{argv_id}","cwd":"/tmp/work","procStart":"99","kind":"daemon-worker","transport":{{"kind":"pty","identity":"/dev/pts/42"}}}}"#
        )
        .into_bytes(),
    )
    .unwrap();
    let background = evidence("/usr/bin/claude", &["claude"])
        .with_claude_session_evidence(path("/home/user/.claude/sessions"), vec![background_record])
        .unwrap();
    assert!(matches!(
        classify_pane(background).resolver_outcome(),
        ResolverOutcome::InsufficientEvidence(_)
    ));
}

#[test]
fn a_same_tool_tail_id_conflict_blocks_automatic_recovery() {
    let resolved_id = "b0cd1f37-9d8e-4d50-bda5-90538fd63343";
    let tail_id = "a27834ae-6192-4287-a005-86063335c28e";
    let tail = ToolAttributedTailSession::try_new(SessionTool::ClaudeCode, tail_id).unwrap();
    let evidence = evidence("/usr/bin/claude", &["claude", "--session-id", resolved_id])
        .with_tail_session(tail);

    let classification = classify_pane(evidence);
    assert!(matches!(classification.recovery(), PaneRecovery::Manual(_)));
    assert!(matches!(
        classification.resolver_outcome(),
        ResolverOutcome::ConflictingEvidence(_)
    ));
}
