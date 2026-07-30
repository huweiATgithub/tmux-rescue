use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use tmux_rescue::{
    AutomaticRecovery, LinuxProcessInspector, LosslessOsString, PaneInitialProcess,
    PaneProcessAnchor, PaneProcessObservation, PaneProcessProbe, PaneRecovery,
    ProcessInspectionFailure, RecordedAbsolutePath, ResolverOutcome, TmuxPaneId, TopologyPane,
    classify_pane, derive_automatic_command, parse_proc_cmdline, parse_proc_stat,
    select_foreground_processes,
};

#[allow(clippy::too_many_arguments)]
fn stat(
    pid: u32,
    name: &str,
    parent: u32,
    group: u32,
    session: u32,
    tty: i64,
    foreground_group: i32,
    start_time: u64,
) -> Vec<u8> {
    format!(
        "{pid} ({name}) S {parent} {group} {session} {tty} {foreground_group} \
         0 0 0 0 0 1 2 0 0 20 0 1 0 {start_time}\n"
    )
    .into_bytes()
}

#[test]
fn parses_proc_stat_when_comm_contains_spaces_and_closing_parentheses() {
    let parsed = parse_proc_stat(
        123,
        &stat(123, "worker ) name", 1, 123, 77, 34_817, 123, 98_765),
    )
    .unwrap();

    assert_eq!(parsed.process_id(), 123);
    assert_eq!(parsed.parent_process_id(), 1);
    assert_eq!(parsed.process_group(), 123);
    assert_eq!(parsed.session_id(), 77);
    assert_eq!(parsed.tty_device(), 34_817);
    assert_eq!(parsed.foreground_process_group(), 123);
    assert_eq!(parsed.start_time(), 98_765);
}

#[test]
fn parses_nul_delimited_cmdline_losslessly() {
    let argv = parse_proc_cmdline(b"command\0\0\xff\0").unwrap();

    assert_eq!(argv.len(), 3);
    assert_eq!(argv[0].as_bytes(), b"command");
    assert_eq!(argv[1].as_bytes(), b"");
    assert_eq!(argv[2].as_bytes(), &[0xff]);
}

#[test]
fn rejects_malformed_or_empty_cmdline() {
    assert!(matches!(
        parse_proc_cmdline(b"command"),
        Err(ProcessInspectionFailure::CmdlineMissingFinalNul)
    ));
    assert!(matches!(
        parse_proc_cmdline(b"\0"),
        Err(ProcessInspectionFailure::EmptyArgvZero)
    ));
}

#[test]
fn selects_one_foreground_group_rooted_at_tpgid() {
    let pane = parse_proc_stat(100, &stat(100, "zsh", 1, 100, 77, 34_817, 200, 10)).unwrap();
    let processes = vec![
        parse_proc_stat(200, &stat(200, "node", 100, 200, 77, 34_817, 200, 20)).unwrap(),
        parse_proc_stat(201, &stat(201, "codex", 200, 200, 77, 34_817, 200, 21)).unwrap(),
        parse_proc_stat(202, &stat(202, "other", 200, 200, 88, 34_817, 200, 22)).unwrap(),
    ];

    let selected = select_foreground_processes(&pane, processes).unwrap();

    assert_eq!(
        selected
            .iter()
            .map(|process| process.process_id())
            .collect::<Vec<_>>(),
        vec![200, 201]
    );
}

#[test]
fn rejects_a_foreground_group_with_multiple_shell_owned_roots() {
    let pane = parse_proc_stat(100, &stat(100, "zsh", 1, 100, 77, 34_817, 200, 10)).unwrap();
    let processes = vec![
        parse_proc_stat(200, &stat(200, "left", 100, 200, 77, 34_817, 200, 20)).unwrap(),
        parse_proc_stat(201, &stat(201, "right", 100, 200, 77, 34_817, 200, 21)).unwrap(),
    ];

    assert!(matches!(
        select_foreground_processes(&pane, processes),
        Err(ProcessInspectionFailure::AmbiguousForegroundJob)
    ));
}

fn os(value: &str) -> LosslessOsString {
    LosslessOsString::try_from_bytes(value.as_bytes().to_vec()).unwrap()
}

fn path(value: &str) -> RecordedAbsolutePath {
    RecordedAbsolutePath::try_from_bytes(value.as_bytes().to_vec()).unwrap()
}

fn os_path(value: &Path) -> LosslessOsString {
    LosslessOsString::try_from_bytes(value.as_os_str().as_encoded_bytes().to_vec()).unwrap()
}

fn fixture_executable(root: &Path, relative_path: &str) -> PathBuf {
    let executable = root.join(".executables").join(relative_path);
    fs::create_dir_all(executable.parent().unwrap()).unwrap();
    fs::write(&executable, b"fake executable image").unwrap();
    executable
}

fn fake_process(
    proc_root: &Path,
    pid: u32,
    stat_bytes: &[u8],
    executable: &Path,
    argv: &[u8],
    cwd: &str,
) {
    let process = proc_root.join(pid.to_string());
    fs::create_dir_all(&process).unwrap();
    fs::write(process.join("stat"), stat_bytes).unwrap();
    fs::write(process.join("cmdline"), argv).unwrap();
    assert!(executable.is_file());
    symlink(executable, process.join("exe")).unwrap();
    symlink(cwd, process.join("cwd")).unwrap();
    fs::create_dir(process.join("fd")).unwrap();
    symlink("/dev/pts/42", process.join("fd/0")).unwrap();
}

fn attach_exact_codex_session(
    root: &Path,
    proc_root: &Path,
    holder_process_id: u32,
) -> RecordedAbsolutePath {
    let session_id = "1d6381bf-01c5-4c4a-b725-8e376e5ad295";
    let codex_store = root.join("codex/sessions");
    fs::create_dir_all(&codex_store).unwrap();
    let session_file = codex_store.join(format!("rollout-{session_id}.jsonl"));
    let session_record = format!(
        r#"{{"type":"session_meta","payload":{{"id":"{session_id}","originator":"codex-tui","thread_source":"user","cwd":"/tmp/work","parent_thread_id":null}}}}"#
    );
    fs::write(&session_file, format!("{session_record}\n")).unwrap();
    symlink(
        &session_file,
        proc_root.join(holder_process_id.to_string()).join("fd/7"),
    )
    .unwrap();
    RecordedAbsolutePath::try_from_bytes(codex_store.as_os_str().as_encoded_bytes().to_vec())
        .unwrap()
}

fn pane(initial_process: PaneInitialProcess) -> TopologyPane {
    TopologyPane::new(
        0,
        TmuxPaneId::try_from_bytes(b"%15".to_vec()).unwrap(),
        path("/tmp/work"),
        PaneProcessAnchor::try_new(100, os("/dev/pts/42"), initial_process).unwrap(),
    )
}

#[test]
fn rejects_a_pane_process_not_bound_to_the_recorded_tty() {
    let temp = tempfile::tempdir().unwrap();
    let zsh = fixture_executable(temp.path(), "zsh");
    fake_process(
        temp.path(),
        100,
        &stat(100, "zsh", 1, 100, 77, 34_817, 100, 10),
        &zsh,
        b"zsh\0",
        "/tmp/work",
    );
    let mismatched = TopologyPane::new(
        0,
        TmuxPaneId::try_from_bytes(b"%15".to_vec()).unwrap(),
        path("/tmp/work"),
        PaneProcessAnchor::try_new(
            100,
            os("/dev/pts/99"),
            PaneInitialProcess::DefaultShell {
                executable: os_path(&zsh),
            },
        )
        .unwrap(),
    );

    assert!(matches!(
        LinuxProcessInspector::with_proc_root(temp.path().to_owned()).observe(&mismatched),
        Err(ProcessInspectionFailure::PaneTtyMismatch { .. })
    ));
}

#[test]
fn rejects_an_idle_shell_outside_the_recorded_pane_directory() {
    let temp = tempfile::tempdir().unwrap();
    let zsh = fixture_executable(temp.path(), "zsh");
    fake_process(
        temp.path(),
        100,
        &stat(100, "zsh", 1, 100, 77, 34_817, 100, 10),
        &zsh,
        b"zsh\0",
        "/tmp/other",
    );

    assert!(matches!(
        LinuxProcessInspector::with_proc_root(temp.path().to_owned()).observe(&pane(
            PaneInitialProcess::DefaultShell {
                executable: os_path(&zsh),
            }
        )),
        Err(ProcessInspectionFailure::PaneWorkingDirectoryMismatch { .. })
    ));
}

#[test]
fn proves_idle_for_default_and_conservatively_recognized_explicit_shells() {
    let temp = tempfile::tempdir().unwrap();
    let zsh = fixture_executable(temp.path(), "zsh");
    fake_process(
        temp.path(),
        100,
        &stat(100, "zsh", 1, 100, 77, 34_817, 100, 10),
        &zsh,
        b"zsh\0",
        "/tmp/work",
    );
    let inspector = LinuxProcessInspector::with_proc_root(temp.path().to_owned());

    assert!(matches!(
        inspector
            .observe(&pane(PaneInitialProcess::DefaultShell {
                executable: os_path(&zsh)
            }))
            .unwrap(),
        PaneProcessObservation::Idle
    ));
    assert!(matches!(
        inspector
            .observe(&pane(PaneInitialProcess::ExplicitCommand))
            .unwrap(),
        PaneProcessObservation::Idle
    ));

    fs::write(temp.path().join("100/cmdline"), b"/usr/bin/zsh\0-i\0").unwrap();
    assert!(matches!(
        inspector
            .observe(&pane(PaneInitialProcess::ExplicitCommand))
            .unwrap(),
        PaneProcessObservation::Idle
    ));
}

#[test]
fn keeps_an_explicit_shell_command_as_foreground_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let zsh = fixture_executable(temp.path(), "zsh");
    fake_process(
        temp.path(),
        100,
        &stat(100, "zsh", 1, 100, 77, 34_817, 100, 10),
        &zsh,
        b"/usr/bin/zsh\0-c\0sleep 30\0",
        "/tmp/work",
    );
    let inspector = LinuxProcessInspector::with_proc_root(temp.path().to_owned());

    assert!(matches!(
        inspector
            .observe(&pane(PaneInitialProcess::ExplicitCommand))
            .unwrap(),
        PaneProcessObservation::Foreground(_)
    ));
}

#[test]
fn retains_a_rooted_native_child_as_transient_foreground_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let zsh = fixture_executable(temp.path(), "zsh");
    let node_executable = fixture_executable(temp.path(), "node");
    let codex_executable = fixture_executable(temp.path(), "codex");
    fake_process(
        temp.path(),
        100,
        &stat(100, "zsh", 1, 100, 77, 34_817, 200, 10),
        &zsh,
        b"zsh\0",
        "/tmp/work",
    );
    fake_process(
        temp.path(),
        200,
        &stat(200, "node", 100, 200, 77, 34_817, 200, 20),
        &node_executable,
        b"node\0/opt/codex/bin/codex.js\0",
        "/tmp/work",
    );
    fake_process(
        temp.path(),
        201,
        &stat(201, "codex", 200, 200, 77, 34_817, 200, 21),
        &codex_executable,
        b"codex\0",
        "/tmp/work",
    );
    let inspector = LinuxProcessInspector::with_proc_root(temp.path().to_owned());

    let observation = inspector
        .observe(&pane(PaneInitialProcess::DefaultShell {
            executable: os_path(&zsh),
        }))
        .unwrap();
    let PaneProcessObservation::Foreground(evidence) = observation else {
        panic!("expected a foreground command");
    };
    assert_eq!(
        evidence.command().executable().as_bytes(),
        node_executable.as_os_str().as_encoded_bytes(),
    );
    assert_eq!(evidence.members().len(), 1);
    assert_eq!(evidence.members()[0].process_id(), 201);
}

#[test]
fn supplies_opened_codex_session_metadata_to_the_resolver() {
    let temp = tempfile::tempdir().unwrap();
    let proc_root = temp.path().join("proc");
    fs::create_dir(&proc_root).unwrap();
    let zsh = fixture_executable(temp.path(), "zsh");
    let node_executable = fixture_executable(temp.path(), "node");
    let codex_executable = fixture_executable(temp.path(), "codex");
    fake_process(
        &proc_root,
        100,
        &stat(100, "zsh", 1, 100, 77, 34_817, 200, 10),
        &zsh,
        b"zsh\0",
        "/tmp/work",
    );
    fake_process(
        &proc_root,
        200,
        &stat(200, "node", 100, 200, 77, 34_817, 200, 20),
        &node_executable,
        b"node\0/opt/codex/bin/codex.js\0",
        "/tmp/work",
    );
    fake_process(
        &proc_root,
        201,
        &stat(201, "codex", 200, 200, 77, 34_817, 200, 21),
        &codex_executable,
        b"codex\0",
        "/tmp/work",
    );
    let session_id = "1d6381bf-01c5-4c4a-b725-8e376e5ad295";
    let codex_store = temp.path().join("codex/sessions");
    fs::create_dir_all(&codex_store).unwrap();
    let session_file = codex_store.join(format!("rollout-{session_id}.jsonl"));
    let session_record = format!(
        r#"{{"type":"session_meta","payload":{{"id":"{session_id}","originator":"codex-tui","thread_source":"user","cwd":"/tmp/work","parent_thread_id":null}}}}"#
    );
    fs::write(&session_file, format!("{session_record}\n")).unwrap();
    symlink(&session_file, proc_root.join("201/fd/7")).unwrap();
    let inspector = LinuxProcessInspector::with_proc_root_and_tool_stores(
        proc_root,
        Some(
            RecordedAbsolutePath::try_from_bytes(
                codex_store.as_os_str().as_encoded_bytes().to_vec(),
            )
            .unwrap(),
        ),
        None,
    );

    let PaneProcessObservation::Foreground(evidence) = inspector
        .observe(&pane(PaneInitialProcess::DefaultShell {
            executable: os_path(&zsh),
        }))
        .unwrap()
    else {
        panic!("expected foreground evidence");
    };
    assert!(matches!(
        classify_pane(*evidence).recovery(),
        PaneRecovery::Automatic(AutomaticRecovery::Codex {
            session_id: id,
            ..
        })
            if id.as_uuid().to_string() == session_id
    ));
}

#[test]
fn recognizes_codex_when_the_native_executable_is_held_open_after_unlink() {
    let temp = tempfile::tempdir().unwrap();
    let proc_root = temp.path().join("proc");
    fs::create_dir(&proc_root).unwrap();
    let zsh = fixture_executable(temp.path(), "zsh");
    let node_executable = fixture_executable(temp.path(), "node");
    fake_process(
        &proc_root,
        100,
        &stat(100, "zsh", 1, 100, 77, 34_817, 200, 10),
        &zsh,
        b"zsh\0",
        "/tmp/work",
    );
    fake_process(
        &proc_root,
        200,
        &stat(200, "node", 100, 200, 77, 34_817, 200, 20),
        &node_executable,
        b"node\0/opt/codex/bin/codex.js\0",
        "/tmp/work",
    );
    let native_path = temp.path().join("codex");
    fs::write(&native_path, b"held native image").unwrap();
    let native = File::open(&native_path).unwrap();
    fs::remove_file(&native_path).unwrap();
    let held_path = PathBuf::from(format!("/proc/self/fd/{}", native.as_raw_fd()));
    fake_process(
        &proc_root,
        201,
        &stat(201, "codex", 200, 200, 77, 34_817, 200, 21),
        &held_path,
        b"codex\0",
        "/tmp/work",
    );
    assert_eq!(
        fs::read_link(proc_root.join("201/exe"))
            .unwrap()
            .as_os_str()
            .as_encoded_bytes(),
        held_path.as_os_str().as_encoded_bytes(),
    );

    let session_id = "1d6381bf-01c5-4c4a-b725-8e376e5ad295";
    let codex_store = temp.path().join("codex/sessions");
    fs::create_dir_all(&codex_store).unwrap();
    let session_file = codex_store.join(format!("rollout-{session_id}.jsonl"));
    let session_record = format!(
        r#"{{"type":"session_meta","payload":{{"id":"{session_id}","originator":"codex-tui","thread_source":"user","cwd":"/tmp/work","parent_thread_id":null}}}}"#
    );
    fs::write(&session_file, format!("{session_record}\n")).unwrap();
    symlink(&session_file, proc_root.join("201/fd/7")).unwrap();
    let inspector = LinuxProcessInspector::with_proc_root_and_tool_stores(
        proc_root,
        Some(
            RecordedAbsolutePath::try_from_bytes(
                codex_store.as_os_str().as_encoded_bytes().to_vec(),
            )
            .unwrap(),
        ),
        None,
    );

    let PaneProcessObservation::Foreground(evidence) = inspector
        .observe(&pane(PaneInitialProcess::DefaultShell {
            executable: os_path(&zsh),
        }))
        .unwrap()
    else {
        panic!("expected foreground evidence");
    };
    let classification = classify_pane(*evidence);
    let PaneRecovery::Automatic(recovery @ AutomaticRecovery::Codex { session_id: id, .. }) =
        classification.recovery()
    else {
        panic!("expected automatic Codex recovery");
    };
    assert_eq!(id.as_uuid().to_string(), session_id);
    assert_eq!(
        derive_automatic_command(recovery)
            .argv()
            .iter()
            .map(LosslessOsString::as_bytes)
            .collect::<Vec<_>>(),
        vec![
            b"codex".as_slice(),
            b"resume".as_slice(),
            session_id.as_bytes()
        ],
    );
}

#[test]
fn keeps_an_unlinked_literal_codex_deleted_executable_manual() {
    let temp = tempfile::tempdir().unwrap();
    let proc_root = temp.path().join("proc");
    fs::create_dir(&proc_root).unwrap();
    let zsh = fixture_executable(temp.path(), "zsh");
    let native_path = fixture_executable(temp.path(), "codex (deleted)");
    let native = File::open(&native_path).unwrap();
    fs::remove_file(&native_path).unwrap();
    let held_path = PathBuf::from(format!("/proc/self/fd/{}", native.as_raw_fd()));
    fake_process(
        &proc_root,
        100,
        &stat(100, "zsh", 1, 100, 77, 34_817, 200, 10),
        &zsh,
        b"zsh\0",
        "/tmp/work",
    );
    fake_process(
        &proc_root,
        200,
        &stat(200, "codex", 100, 200, 77, 34_817, 200, 20),
        &held_path,
        b"codex\0",
        "/tmp/work",
    );
    let codex_store = attach_exact_codex_session(temp.path(), &proc_root, 200);
    let inspector =
        LinuxProcessInspector::with_proc_root_and_tool_stores(proc_root, Some(codex_store), None);

    let PaneProcessObservation::Foreground(evidence) = inspector
        .observe(&pane(PaneInitialProcess::DefaultShell {
            executable: os_path(&zsh),
        }))
        .unwrap()
    else {
        panic!("expected foreground evidence");
    };
    let classification = classify_pane(*evidence);
    let PaneRecovery::Manual(command) = classification.recovery() else {
        panic!("literal suffix must remain manual");
    };
    let mut expected_executable = native_path.as_os_str().as_encoded_bytes().to_vec();
    expected_executable.extend_from_slice(b" (deleted)");
    assert_eq!(
        command.executable().as_bytes(),
        expected_executable.as_slice()
    );
}

#[test]
fn keeps_noninteractive_codex_modes_manual_with_proved_unlink_and_session_evidence() {
    for mode in [b"app-server".as_slice(), b"exec", b"mcp-server"] {
        let temp = tempfile::tempdir().unwrap();
        let proc_root = temp.path().join("proc");
        fs::create_dir(&proc_root).unwrap();
        let zsh = fixture_executable(temp.path(), "zsh");
        let native_path = fixture_executable(temp.path(), "codex");
        let native = File::open(&native_path).unwrap();
        fs::remove_file(&native_path).unwrap();
        let held_path = PathBuf::from(format!("/proc/self/fd/{}", native.as_raw_fd()));
        fake_process(
            &proc_root,
            100,
            &stat(100, "zsh", 1, 100, 77, 34_817, 200, 10),
            &zsh,
            b"zsh\0",
            "/tmp/work",
        );
        let argv = [b"codex".as_slice(), mode].join(&0);
        let mut nul_terminated_argv = argv;
        nul_terminated_argv.push(0);
        fake_process(
            &proc_root,
            200,
            &stat(200, "codex", 100, 200, 77, 34_817, 200, 20),
            &held_path,
            &nul_terminated_argv,
            "/tmp/work",
        );
        let codex_store = attach_exact_codex_session(temp.path(), &proc_root, 200);
        let inspector = LinuxProcessInspector::with_proc_root_and_tool_stores(
            proc_root,
            Some(codex_store),
            None,
        );

        let PaneProcessObservation::Foreground(evidence) = inspector
            .observe(&pane(PaneInitialProcess::DefaultShell {
                executable: os_path(&zsh),
            }))
            .unwrap()
        else {
            panic!("expected foreground evidence");
        };
        assert!(matches!(
            classify_pane(*evidence).recovery(),
            PaneRecovery::Manual(_)
        ));
    }
}

#[test]
fn keeps_the_raw_node_leader_as_manual_fallback_when_codex_lacks_session_proof() {
    let temp = tempfile::tempdir().unwrap();
    let proc_root = temp.path().join("proc");
    fs::create_dir(&proc_root).unwrap();
    let zsh = fixture_executable(temp.path(), "zsh");
    let node_executable = fixture_executable(temp.path(), "node");
    let native_path = fixture_executable(temp.path(), "codex");
    let native = File::open(&native_path).unwrap();
    fs::remove_file(&native_path).unwrap();
    let held_path = PathBuf::from(format!("/proc/self/fd/{}", native.as_raw_fd()));
    fake_process(
        &proc_root,
        100,
        &stat(100, "zsh", 1, 100, 77, 34_817, 200, 10),
        &zsh,
        b"zsh\0",
        "/tmp/work",
    );
    fake_process(
        &proc_root,
        200,
        &stat(200, "node", 100, 200, 77, 34_817, 200, 20),
        &node_executable,
        b"node\0/opt/codex/bin/codex.js\0",
        "/tmp/work",
    );
    fake_process(
        &proc_root,
        201,
        &stat(201, "codex", 200, 200, 77, 34_817, 200, 21),
        &held_path,
        b"codex\0",
        "/tmp/work",
    );
    let codex_store = temp.path().join("codex/sessions");
    fs::create_dir_all(&codex_store).unwrap();
    let inspector = LinuxProcessInspector::with_proc_root_and_tool_stores(
        proc_root,
        Some(
            RecordedAbsolutePath::try_from_bytes(
                codex_store.as_os_str().as_encoded_bytes().to_vec(),
            )
            .unwrap(),
        ),
        None,
    );

    let PaneProcessObservation::Foreground(evidence) = inspector
        .observe(&pane(PaneInitialProcess::DefaultShell {
            executable: os_path(&zsh),
        }))
        .unwrap()
    else {
        panic!("expected foreground evidence");
    };
    let classification = classify_pane(*evidence);
    let PaneRecovery::Manual(command) = classification.recovery() else {
        panic!("missing Codex session proof must remain manual");
    };
    assert!(matches!(
        classification.resolver_outcome(),
        ResolverOutcome::InsufficientEvidence(_)
    ));
    assert_eq!(
        command.executable().as_bytes(),
        node_executable.as_os_str().as_encoded_bytes()
    );
    assert_eq!(
        command
            .argv()
            .iter()
            .map(LosslessOsString::as_bytes)
            .collect::<Vec<_>>(),
        vec![b"node".as_slice(), b"/opt/codex/bin/codex.js".as_slice()]
    );
}

#[test]
fn supplies_exact_claude_process_metadata_to_the_resolver() {
    let temp = tempfile::tempdir().unwrap();
    let proc_root = temp.path().join("proc");
    fs::create_dir(&proc_root).unwrap();
    let zsh = fixture_executable(temp.path(), "zsh");
    let claude_executable = fixture_executable(temp.path(), "claude/versions/2.1.195");
    fake_process(
        &proc_root,
        100,
        &stat(100, "zsh", 1, 100, 77, 34_817, 200, 10),
        &zsh,
        b"zsh\0",
        "/tmp/work",
    );
    fake_process(
        &proc_root,
        200,
        &stat(200, "claude", 100, 200, 77, 34_817, 200, 20),
        &claude_executable,
        b"claude\0",
        "/tmp/work",
    );
    let session_id = "27ea5a6d-5b84-4770-998e-a1a8285b0e9a";
    let claude_store = temp.path().join("claude/sessions");
    fs::create_dir_all(&claude_store).unwrap();
    fs::write(
        claude_store.join("200.json"),
        format!(
            r#"{{"pid":200,"sessionId":"{session_id}","cwd":"/tmp/work","procStart":"20","kind":"interactive","transport":{{"kind":"pty","identity":"/dev/pts/42"}}}}"#
        ),
    )
    .unwrap();
    let inspector = LinuxProcessInspector::with_proc_root_and_tool_stores(
        proc_root,
        None,
        Some(
            RecordedAbsolutePath::try_from_bytes(
                claude_store.as_os_str().as_encoded_bytes().to_vec(),
            )
            .unwrap(),
        ),
    );

    let PaneProcessObservation::Foreground(evidence) = inspector
        .observe(&pane(PaneInitialProcess::DefaultShell {
            executable: os_path(&zsh),
        }))
        .unwrap()
    else {
        panic!("expected foreground evidence");
    };
    assert!(matches!(
        classify_pane(*evidence).recovery(),
        PaneRecovery::Automatic(AutomaticRecovery::ClaudeCode { session_id: id })
            if id.as_uuid().to_string() == session_id
    ));
}
