use std::collections::HashSet;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;

use serde_json::json;
use tmux_rescue::{
    AutomaticFallbackReason, AutomaticRecoveryExpectation, LosslessOsString,
    MAX_RENDERED_SHELL_INPUT_BYTES, PlannedPaneAction, PlanningExecutable, RecordedAbsolutePath,
    ResolvedDirectoryOrigin, RestoreEnvironment, RestoreEnvironmentFailure, RestorePlanningError,
    TargetShell, TmuxSelector, ValidatedSnapshot, plan_restore,
};

const CODEX_PROMPT_TEXT: &str = "The test prompt for recovering.\n\nLine 1.\n\nLine 2.";

fn encoded(value: &str) -> serde_json::Value {
    json!({"encoding": "utf8", "value": value})
}

fn snapshot() -> ValidatedSnapshot {
    let value = json!({
        "captured_at": "2026-07-23T00:00:00Z",
        "source": encoded("/recorded/source.sock"),
        "consistency": {"kind": "stable"},
        "sessions": [{
            "name": "work",
            "working_directory": encoded("/recorded/session"),
            "windows": [{
                "source_index": 0,
                "name": "editor",
                "panes": [
                    {
                        "source_index": 0,
                        "working_directory": encoded("/recorded/idle"),
                        "recovery": {"kind": "idle"}
                    },
                    {
                        "source_index": 1,
                        "working_directory": encoded("/recorded/automatic"),
                        "recovery": {
                            "kind": "automatic",
                            "recovery": {
                                "kind": "codex",
                                "session_id": "1d6381bf-01c5-4c4a-b725-8e376e5ad295",
                                "prompt_area": {"text": CODEX_PROMPT_TEXT}
                            }
                        }
                    },
                    {
                        "source_index": 2,
                        "working_directory": encoded("/recorded/manual"),
                        "recovery": {
                            "kind": "manual",
                            "command": {
                                "executable": encoded("/usr/bin/custom"),
                                "argv": [encoded("custom"), encoded("a'b"), encoded("")]
                            }
                        }
                    },
                    {
                        "source_index": 3,
                        "working_directory": encoded("/recorded/unavailable"),
                        "recovery": {
                            "kind": "unavailable",
                            "failure": "foreground process vanished"
                        }
                    }
                ]
            }]
        }]
    });
    ValidatedSnapshot::from_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}

fn automatic_snapshot() -> ValidatedSnapshot {
    let value = json!({
        "captured_at": "2026-07-23T00:00:00Z",
        "source": encoded("/recorded/source.sock"),
        "consistency": {"kind": "stable"},
        "sessions": [{
            "name": "work",
            "working_directory": encoded("/recorded/session"),
            "windows": [{
                "source_index": 0,
                "name": "automatic",
                "panes": [
                    {
                        "source_index": 0,
                        "working_directory": encoded("/recorded/codex"),
                        "recovery": {
                            "kind": "automatic",
                            "recovery": {
                                "kind": "codex",
                                "session_id": "1d6381bf-01c5-4c4a-b725-8e376e5ad295",
                                "prompt_area": {"text": CODEX_PROMPT_TEXT}
                            }
                        }
                    },
                    {
                        "source_index": 1,
                        "working_directory": encoded("/recorded/claude"),
                        "recovery": {
                            "kind": "automatic",
                            "recovery": {
                                "kind": "claude_code",
                                "session_id": "8f707f38-6fd3-4a11-a03f-853b03d47b0c"
                            }
                        }
                    },
                    {
                        "source_index": 2,
                        "working_directory": encoded("/recorded/mdbook"),
                        "recovery": {
                            "kind": "automatic",
                            "recovery": {
                                "kind": "md_book_serve",
                                "command": {
                                    "executable": encoded("/usr/bin/mdbook"),
                                    "argv": [encoded("mdbook"), encoded("serve")]
                                }
                            }
                        }
                    },
                    {
                        "source_index": 3,
                        "working_directory": encoded("/recorded/book"),
                        "recovery": {
                            "kind": "automatic",
                            "recovery": {
                                "kind": "bookshelf_serve",
                                "command": {
                                    "executable": encoded("/usr/bin/book"),
                                    "argv": [encoded("book"), encoded("serve")]
                                }
                            }
                        }
                    }
                ]
            }]
        }]
    });
    ValidatedSnapshot::from_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}

fn launchable_environment(commands: &[&[u8]]) -> FakeEnvironment {
    FakeEnvironment {
        existing_directories: [
            b"/home/user".to_vec(),
            b"/recorded/session".to_vec(),
            b"/recorded/automatic".to_vec(),
            b"/recorded/codex".to_vec(),
            b"/recorded/claude".to_vec(),
            b"/recorded/mdbook".to_vec(),
            b"/recorded/book".to_vec(),
        ]
        .into_iter()
        .collect(),
        available_commands: commands.iter().map(|command| command.to_vec()).collect(),
    }
}

struct FakeEnvironment {
    existing_directories: HashSet<Vec<u8>>,
    available_commands: HashSet<Vec<u8>>,
}

impl FakeEnvironment {
    fn absent() -> Self {
        Self {
            existing_directories: [b"/home/user".to_vec(), b"/recorded/idle".to_vec()]
                .into_iter()
                .collect(),
            available_commands: HashSet::new(),
        }
    }
}

impl RestoreEnvironment for FakeEnvironment {
    fn target_shell(&self) -> Result<TargetShell, RestoreEnvironmentFailure> {
        TargetShell::try_from_bytes(b"/bin/sh".to_vec())
            .map_err(|error| RestoreEnvironmentFailure::new(error.to_string()))
    }

    fn home_directory(&self) -> Result<RecordedAbsolutePath, RestoreEnvironmentFailure> {
        RecordedAbsolutePath::try_from_bytes(b"/home/user".to_vec())
            .map_err(|error| RestoreEnvironmentFailure::new(error.to_string()))
    }

    fn directory_exists(&self, directory: &RecordedAbsolutePath) -> bool {
        self.existing_directories.contains(directory.as_bytes())
    }

    fn resolve_executable(
        &self,
        _directory: &RecordedAbsolutePath,
        command_word: &LosslessOsString,
    ) -> Option<PlanningExecutable> {
        self.available_commands
            .contains(command_word.as_bytes())
            .then(|| PlanningExecutable::try_from_bytes(b"/bin/sh".to_vec()).unwrap())
    }
}

#[test]
fn destination_refines_explicit_name_path_and_recorded_default_without_resolution() {
    let environment = FakeEnvironment::absent();
    let named = plan_restore(
        &snapshot(),
        Some(TmuxSelector::SocketName(OsString::from("abc"))),
        &environment,
    )
    .unwrap();
    let relative = plan_restore(
        &snapshot(),
        Some(TmuxSelector::SocketPath(OsString::from("./rescue.sock"))),
        &environment,
    )
    .unwrap();
    let recorded = plan_restore(&snapshot(), None, &environment).unwrap();

    assert_eq!(
        named.destination().selector(),
        &TmuxSelector::SocketName(OsString::from("abc"))
    );
    assert_eq!(
        relative.destination().selector(),
        &TmuxSelector::SocketPath(OsString::from("./rescue.sock"))
    );
    assert_eq!(
        recorded.destination().selector(),
        &TmuxSelector::SocketPath(OsString::from("/recorded/source.sock"))
    );
    assert!(named.render_human().starts_with("target: -L abc\n"));
    assert!(
        relative
            .render_human()
            .starts_with("target: -S ./rescue.sock\n")
    );
    assert!(
        recorded
            .render_human()
            .starts_with("target: -S /recorded/source.sock\n")
    );
}

#[test]
fn human_plan_has_no_vacancy_or_resolution_fields() {
    let rendered = plan_restore(&snapshot(), None, &FakeEnvironment::absent())
        .unwrap()
        .render_human();

    for line in rendered.lines() {
        let field = line
            .trim_start()
            .split(':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        assert!(
            ![
                "vacant",
                "absent",
                "available",
                "resolved",
                "target vacancy"
            ]
            .contains(&field.as_str()),
            "{rendered}"
        );
    }
}

#[test]
fn resolves_directories_and_refines_every_pane_action() {
    let environment = FakeEnvironment::absent();

    let plan = plan_restore(&snapshot(), None, &environment).unwrap();

    assert_eq!(
        plan.destination().selector(),
        &TmuxSelector::SocketPath(OsString::from("/recorded/source.sock"))
    );
    assert_eq!(plan.panes().len(), 4);
    assert!(matches!(
        plan.panes()[0].action(),
        PlannedPaneAction::LeaveIdle { directory }
            if directory.origin() == ResolvedDirectoryOrigin::Recorded
    ));
    assert!(matches!(
        plan.panes()[1].action(),
        PlannedPaneAction::PasteAutomaticFallback {
            directory,
            reason: AutomaticFallbackReason::RecordedDirectoryUnavailable,
            ..
        } if directory.origin() == ResolvedDirectoryOrigin::SessionFallback
    ));
    let PlannedPaneAction::PasteManualHint { input, .. } = plan.panes()[2].action() else {
        panic!("expected a manual hint");
    };
    assert_eq!(input.as_bytes(), b"'custom' 'a'\\''b' ''");
    assert!(matches!(
        plan.panes()[3].action(),
        PlannedPaneAction::NoInput { .. }
    ));
    assert!(!plan.degradations().is_empty());
}

#[test]
fn automatic_launch_exists_only_with_recorded_directory_and_available_executable() {
    let mut environment = FakeEnvironment::absent();
    environment.existing_directories.extend([
        b"/recorded/session".to_vec(),
        b"/recorded/automatic".to_vec(),
    ]);

    let missing_executable = plan_restore(&snapshot(), None, &environment).unwrap();
    assert!(matches!(
        missing_executable.panes()[1].action(),
        PlannedPaneAction::PasteAutomaticFallback {
            reason: AutomaticFallbackReason::ExecutableUnavailable,
            ..
        }
    ));

    environment.available_commands.insert(b"codex".to_vec());
    let launchable = plan_restore(&snapshot(), None, &environment).unwrap();
    let PlannedPaneAction::LaunchAutomatic(launch) = launchable.panes()[1].action() else {
        panic!("expected an automatic launch");
    };
    assert_eq!(launch.input().executable().path().as_bytes(), b"/bin/sh");
    assert_eq!(
        launch.input().rendered().as_bytes(),
        b"'/bin/sh' 'resume' '1d6381bf-01c5-4c4a-b725-8e376e5ad295'"
    );
}

#[test]
fn codex_prompt_input_is_bound_to_its_enclosing_session_launch() {
    let environment = launchable_environment(&[b"codex"]);
    let plan = plan_restore(&automatic_snapshot(), None, &environment).unwrap();
    let PlannedPaneAction::LaunchAutomatic(launch) = plan.panes()[0].action() else {
        panic!("expected a Codex automatic launch");
    };

    assert_eq!(launch.directory().path().as_bytes(), b"/recorded/codex");
    assert_eq!(launch.input().executable().path().as_bytes(), b"/bin/sh");
    assert!(!format!("{plan:?}").contains(CODEX_PROMPT_TEXT));
    assert!(matches!(
        launch.expectation(),
        AutomaticRecoveryExpectation::Codex(session_id)
            if session_id.as_uuid().to_string() == "1d6381bf-01c5-4c4a-b725-8e376e5ad295"
    ));
    let rendered = plan.render_human();
    let codex_pane = rendered
        .split("  pane work:0:1")
        .next()
        .unwrap()
        .rsplit_once("  pane work:0:0")
        .unwrap()
        .1;
    assert!(codex_pane.contains("after recovery  paste 5 visible rows without Enter"));
    assert!(!rendered.contains(CODEX_PROMPT_TEXT));
}

#[test]
fn claude_and_serve_launches_cannot_acquire_codex_prompt_input() {
    let environment = launchable_environment(&[b"codex", b"claude", b"mdbook", b"book"]);
    let plan = plan_restore(&automatic_snapshot(), None, &environment).unwrap();
    let rendered = plan.render_human();

    for pane_index in 0..4 {
        let PlannedPaneAction::LaunchAutomatic(launch) = plan.panes()[pane_index].action() else {
            panic!("expected automatic launch for pane {pane_index}");
        };
        assert_eq!(
            launch.directory().path().as_bytes(),
            match pane_index {
                0 => b"/recorded/codex" as &[u8],
                1 => b"/recorded/claude",
                2 => b"/recorded/mdbook",
                3 => b"/recorded/book",
                _ => unreachable!(),
            }
        );
    }

    assert_eq!(rendered.matches("after recovery  paste").count(), 1);
    assert!(!rendered.contains(CODEX_PROMPT_TEXT));
}

#[test]
fn automatic_fallback_drops_post_recovery_prompt_input() {
    let plan = plan_restore(&snapshot(), None, &FakeEnvironment::absent()).unwrap();

    assert!(matches!(
        plan.panes()[1].action(),
        PlannedPaneAction::PasteAutomaticFallback { .. }
    ));
    let rendered = plan.render_human();
    assert!(!rendered.contains("after recovery"));
    assert!(!rendered.contains(CODEX_PROMPT_TEXT));
}

#[test]
fn rejects_terminal_control_bytes_in_rendered_input() {
    let mut raw: serde_json::Value =
        serde_json::from_slice(&snapshot().to_json_pretty().unwrap()).unwrap();
    raw["sessions"][0]["windows"][0]["panes"][2]["recovery"]["command"]["argv"][1] =
        encoded("line\nbreak");
    let unsafe_snapshot = ValidatedSnapshot::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();

    assert!(matches!(
        plan_restore(&unsafe_snapshot, None, &FakeEnvironment::absent()),
        Err(RestorePlanningError::UnsafeShellInput { .. })
    ));

    raw["sessions"][0]["windows"][0]["panes"][2]["recovery"]["command"]["argv"][1] =
        serde_json::json!({"encoding": "base64", "value": "mw=="});
    let c1_snapshot = ValidatedSnapshot::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();
    assert!(matches!(
        plan_restore(&c1_snapshot, None, &FakeEnvironment::absent()),
        Err(RestorePlanningError::UnsafeShellInput { .. })
    ));

    raw["sessions"][0]["windows"][0]["panes"][2]["recovery"]["command"]["argv"][1] =
        serde_json::json!({"encoding": "base64", "value": "5paHmw=="});
    let mixed_c1_snapshot =
        ValidatedSnapshot::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();
    assert!(matches!(
        plan_restore(&mixed_c1_snapshot, None, &FakeEnvironment::absent()),
        Err(RestorePlanningError::UnsafeShellInput { .. })
    ));

    raw["sessions"][0]["windows"][0]["panes"][2]["recovery"]["command"]["argv"][1] =
        encoded("\u{009b}");
    let unicode_control = ValidatedSnapshot::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();
    assert!(matches!(
        plan_restore(&unicode_control, None, &FakeEnvironment::absent()),
        Err(RestorePlanningError::UnsafeShellInput { .. })
    ));
}

#[test]
fn rejects_rendered_input_that_cannot_fit_one_guarded_tmux_argument() {
    let mut raw: serde_json::Value =
        serde_json::from_slice(&snapshot().to_json_pretty().unwrap()).unwrap();
    raw["sessions"][0]["windows"][0]["panes"][2]["recovery"]["command"]["argv"][1] =
        encoded(&"x".repeat(MAX_RENDERED_SHELL_INPUT_BYTES));
    let snapshot = ValidatedSnapshot::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();

    assert!(matches!(
        plan_restore(&snapshot, None, &FakeEnvironment::absent()),
        Err(RestorePlanningError::ShellInputTooLarge {
            maximum: MAX_RENDERED_SHELL_INPUT_BYTES,
            ..
        })
    ));
}

#[test]
fn renders_printable_unicode_arguments_without_treating_utf8_bytes_as_controls() {
    let mut raw: serde_json::Value =
        serde_json::from_slice(&snapshot().to_json_pretty().unwrap()).unwrap();
    raw["sessions"][0]["windows"][0]["panes"][2]["recovery"]["command"]["argv"][1] =
        encoded("文🙂");
    let snapshot = ValidatedSnapshot::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();

    let plan = plan_restore(&snapshot, None, &FakeEnvironment::absent()).unwrap();
    let PlannedPaneAction::PasteManualHint { input, .. } = plan.panes()[2].action() else {
        panic!("expected a manual hint");
    };

    assert_eq!(input.as_bytes(), "'custom' '文🙂' ''".as_bytes());
}

#[test]
fn renders_printable_utf8_chunks_next_to_non_utf8_bytes() {
    let mut raw: serde_json::Value =
        serde_json::from_slice(&snapshot().to_json_pretty().unwrap()).unwrap();
    raw["sessions"][0]["windows"][0]["panes"][2]["recovery"]["command"]["argv"][1] =
        serde_json::json!({"encoding": "base64", "value": "5paH/w=="});
    let snapshot = ValidatedSnapshot::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap();

    let plan = plan_restore(&snapshot, None, &FakeEnvironment::absent()).unwrap();
    let PlannedPaneAction::PasteManualHint { input, .. } = plan.panes()[2].action() else {
        panic!("expected a manual hint");
    };

    assert_eq!(input.as_bytes(), b"'custom' '\xe6\x96\x87\xff' ''");
}

#[test]
fn rejects_shells_without_the_posix_renderer_contract() {
    assert!(TargetShell::try_from_bytes(b"/usr/bin/fish".to_vec()).is_err());
    assert!(TargetShell::try_from_bytes(b"/definitely/missing/sh".to_vec()).is_err());

    let shell = TargetShell::try_from_bytes(b"/bin/sh".to_vec()).unwrap();
    assert_eq!(shell.executable().as_bytes(), b"/bin/sh");
    assert!(std::path::Path::new(shell.executable_identity().as_os_str()).is_absolute());
}

#[test]
fn system_executable_check_uses_effective_user_permissions() {
    let directory = tempfile::tempdir().unwrap();
    let command_path = directory.path().join("command");
    std::fs::write(&command_path, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&command_path, std::fs::Permissions::from_mode(0o001)).unwrap();

    let environment = tmux_rescue::SystemRestoreEnvironment;
    let working_directory = RecordedAbsolutePath::try_from_bytes(
        directory.path().as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap();
    let command =
        LosslessOsString::try_from_bytes(command_path.as_os_str().as_encoded_bytes().to_vec())
            .unwrap();

    assert!(
        environment
            .resolve_executable(&working_directory, &command)
            .is_none()
    );

    std::fs::set_permissions(&command_path, std::fs::Permissions::from_mode(0o100)).unwrap();
    assert!(
        environment
            .resolve_executable(&working_directory, &command)
            .is_some()
    );
}

#[test]
fn executable_and_shell_proofs_detect_path_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let executable_path = temp.path().join("command");
    let shell_path = temp.path().join("sh");
    std::fs::write(&executable_path, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink("/bin/sh", &shell_path).unwrap();
    let executable =
        PlanningExecutable::try_from_bytes(executable_path.as_os_str().as_encoded_bytes().to_vec())
            .unwrap();
    let shell =
        TargetShell::try_from_bytes(shell_path.as_os_str().as_encoded_bytes().to_vec()).unwrap();

    std::fs::remove_file(&executable_path).unwrap();
    std::fs::write(&executable_path, b"#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_file(&shell_path).unwrap();
    std::os::unix::fs::symlink("/bin/bash", &shell_path).unwrap();

    assert!(!executable.matches_current_file());
    assert!(!shell.matches_current_file());
}

#[test]
fn rejects_script_shell_wrappers_whose_runtime_identity_would_be_the_interpreter() {
    let temp = tempfile::tempdir().unwrap();
    let shell_path = temp.path().join("sh");
    std::fs::write(&shell_path, b"#!/bin/sh\nexec /bin/sh \"$@\"\n").unwrap();
    std::fs::set_permissions(&shell_path, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert!(
        TargetShell::try_from_bytes(shell_path.as_os_str().as_encoded_bytes().to_vec()).is_err()
    );
}

#[test]
fn rejects_a_supported_shell_alias_to_an_unrelated_native_program() {
    let temp = tempfile::tempdir().unwrap();
    let alias = temp.path().join("sh");
    std::os::unix::fs::symlink("/bin/false", &alias).unwrap();

    assert!(TargetShell::try_from_bytes(alias.as_os_str().as_encoded_bytes().to_vec()).is_err());
}

#[test]
fn rejects_shell_named_copies_of_unrelated_or_malformed_native_files() {
    let temp = tempfile::tempdir().unwrap();
    let unrelated = temp.path().join("sh");
    std::fs::copy("/bin/false", &unrelated).unwrap();
    std::fs::set_permissions(&unrelated, std::fs::Permissions::from_mode(0o700)).unwrap();

    let malformed = temp.path().join("bash");
    std::fs::write(&malformed, b"\x7fELF").unwrap();
    std::fs::set_permissions(&malformed, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(
        TargetShell::try_from_bytes(unrelated.as_os_str().as_encoded_bytes().to_vec()).unwrap_err(),
        tmux_rescue::TargetShellError::RuntimeNotAuthorized
    );
    assert_eq!(
        TargetShell::try_from_bytes(malformed.as_os_str().as_encoded_bytes().to_vec()).unwrap_err(),
        tmux_rescue::TargetShellError::MalformedNativeExecutable
    );
}

#[test]
fn human_plan_prints_execution_relevant_fallbacks_and_inputs() {
    let fallback_plan = plan_restore(&snapshot(), None, &FakeEnvironment::absent()).unwrap();
    let fallback_rendered = fallback_plan.render_human();

    assert!(fallback_rendered.contains("pane work:0:1 cwd /home/user [session fallback]"));
    assert!(fallback_rendered.contains("input 'codex' 'resume'"));
    assert!(fallback_rendered.contains("reason recorded directory unavailable"));
    assert!(!fallback_rendered.contains("after recovery"));
    assert!(!fallback_rendered.contains(CODEX_PROMPT_TEXT));
    assert!(fallback_rendered.contains("input 'custom' 'a'\\\\''b' ''"));
    assert!(fallback_rendered.contains("capture failure foreground process vanished"));
    assert!(fallback_rendered.contains("degradations:"));
    assert!(fallback_rendered.contains("pane work:0:1 directory fallback"));

    let launch_plan =
        plan_restore(&snapshot(), None, &launchable_environment(&[b"codex"])).unwrap();
    let launch_rendered = launch_plan.render_human();

    assert!(launch_rendered.contains("after recovery  paste 5 visible rows without Enter"));
    assert!(!launch_rendered.contains(CODEX_PROMPT_TEXT));
}

#[test]
fn human_plan_renders_non_utf8_paths_as_exact_byte_escapes() {
    let selector = TmuxSelector::SocketPath(OsString::from_vec(b"./\xff".to_vec()));
    let rendered = plan_restore(&snapshot(), Some(selector), &FakeEnvironment::absent())
        .unwrap()
        .render_human();

    assert!(rendered.contains(r"target: -S ./\xff"));
    assert!(!rendered.contains('\u{fffd}'));

    let ascii_rendered = plan_restore(
        &snapshot(),
        Some(TmuxSelector::SocketPath(OsString::from(r"./\xff"))),
        &FakeEnvironment::absent(),
    )
    .unwrap()
    .render_human();
    assert_ne!(rendered, ascii_rendered);
    assert!(ascii_rendered.contains(r"target: -S ./\\xff"));
}
