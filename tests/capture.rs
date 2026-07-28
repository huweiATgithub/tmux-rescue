use std::collections::VecDeque;

use tmux_rescue::{
    AutomaticRecovery, CaptureConsistency, CaptureEvent, CaptureFailure, CaptureSource,
    CaptureSourceFailure, CaptureTime, CapturedCommand, CodexPromptCaptureFailure,
    ForegroundProcessMember, LosslessOsString, MAX_TOPOLOGY_VALIDATION_ATTEMPTS,
    OpenedCodexSessionFile, PaneInitialProcess, PaneProcessAnchor, PaneProcessObservation,
    PaneRecovery, PaneTiedForegroundEvidence, RecordedAbsolutePath, SnapshotSource, TmuxPaneId,
    TopologyObservation, TopologyPane, TopologySession, TopologyWindow, VisiblePaneGrid,
    VisiblePaneMetadata, capture_snapshot,
};

const CODEX_SESSION_ID: &str = "1d6381bf-01c5-4c4a-b725-8e376e5ad295";
const SENSITIVE_PROMPT: &str = "release the unreleased signing key";

fn os(value: &str) -> LosslessOsString {
    LosslessOsString::try_from_bytes(value.as_bytes().to_vec()).unwrap()
}

fn path(value: &str) -> RecordedAbsolutePath {
    RecordedAbsolutePath::try_from_bytes(value.as_bytes().to_vec()).unwrap()
}

fn topology(session: &str, pane_indexes: &[u32]) -> TopologyObservation {
    let panes = pane_indexes
        .iter()
        .map(|index| {
            TopologyPane::new(
                *index,
                TmuxPaneId::try_from_bytes(format!("%{}", 15 + index).into_bytes()).unwrap(),
                path("/tmp/work"),
                PaneProcessAnchor::try_new(
                    10_000 + *index,
                    os(&format!("/dev/pts/{index}")),
                    PaneInitialProcess::DefaultShell {
                        executable: os("/bin/sh"),
                    },
                )
                .unwrap(),
            )
        })
        .collect();
    TopologyObservation::try_new(vec![TopologySession::new(
        session.to_owned(),
        path("/tmp/work"),
        vec![TopologyWindow::new(1, "editor".to_owned(), panes)],
    )])
    .unwrap()
}

fn topology_with_pane_id(session: &str, pane_index: u32, pane_id: &str) -> TopologyObservation {
    TopologyObservation::try_new(vec![TopologySession::new(
        session.to_owned(),
        path("/tmp/work"),
        vec![TopologyWindow::new(
            1,
            "editor".to_owned(),
            vec![TopologyPane::new(
                pane_index,
                TmuxPaneId::try_from_bytes(pane_id.as_bytes().to_vec()).unwrap(),
                path("/tmp/work"),
                PaneProcessAnchor::try_new(
                    10_000 + pane_index,
                    os(&format!("/dev/pts/{pane_index}")),
                    PaneInitialProcess::DefaultShell {
                        executable: os("/bin/sh"),
                    },
                )
                .unwrap(),
            )],
        )],
    )])
    .unwrap()
}

fn command(executable: &str, argv: &[&str]) -> CapturedCommand {
    CapturedCommand::try_new(
        os(executable),
        argv.iter().map(|argument| os(argument)).collect(),
    )
    .unwrap()
}

fn foreground(executable: &str, argv: &[&str]) -> PaneTiedForegroundEvidence {
    PaneTiedForegroundEvidence::try_new(
        command(executable, argv),
        path("/tmp/work"),
        os("/dev/pts/42"),
        os("/dev/pts/42"),
        12_345,
        12_345,
        12_345,
        99,
    )
    .unwrap()
}

fn codex_foreground() -> PaneProcessObservation {
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
            "/home/user/.codex/sessions/2026/07/23/rollout-{CODEX_SESSION_ID}.jsonl"
        )),
        format!(
            r#"{{"type":"session_meta","payload":{{"id":"{CODEX_SESSION_ID}","originator":"codex-tui","thread_source":"user","cwd":"/tmp/work","parent_thread_id":null}}}}"#
        )
        .into_bytes(),
    )
    .unwrap();
    PaneProcessObservation::Foreground(Box::new(
        foreground("/usr/bin/node", &["node", "/opt/codex/bin/codex.js"])
            .with_foreground_members(vec![native_member])
            .unwrap()
            .with_codex_session_evidence(path("/home/user/.codex/sessions"), vec![session_file])
            .unwrap(),
    ))
}

fn downgraded_codex_foreground() -> PaneProcessObservation {
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
    PaneProcessObservation::Foreground(Box::new(
        foreground("/usr/bin/node", &["node", "/opt/codex/bin/codex.js"])
            .with_foreground_members(vec![native_member])
            .unwrap(),
    ))
}

fn visible_grid(pane_id: &str, rows: &[&str], cursor_x: u16, cursor_y: u16) -> VisiblePaneGrid {
    VisiblePaneGrid::try_from_tmux_styled_capture(
        VisiblePaneMetadata::try_new(
            TmuxPaneId::try_from_bytes(pane_id.as_bytes().to_vec()).unwrap(),
            80,
            u16::try_from(rows.len()).unwrap(),
            cursor_x,
            cursor_y,
            false,
        )
        .unwrap(),
        format!("{}\n", rows.join("\n")).into_bytes(),
    )
    .unwrap()
}

fn captured_grid(pane_id: &str) -> VisiblePaneGrid {
    visible_grid(
        pane_id,
        &["» draft", "  second", "", "  95% context left"],
        8,
        1,
    )
}

fn absent_grid(pane_id: &str) -> VisiblePaneGrid {
    visible_grid(
        pane_id,
        &[
            "› \x1b[2mAsk Codex to do anything\x1b[22m",
            "",
            "  95% context left",
        ],
        2,
        0,
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SourceCall {
    ReadTopology,
    InspectPane(String),
    ReadVisiblePane(String),
}

struct ScriptedSource {
    source: SnapshotSource,
    reads: VecDeque<Result<TopologyObservation, CaptureSourceFailure>>,
    inspections: usize,
    observation: PaneProcessObservation,
    observations: VecDeque<PaneProcessObservation>,
    visible_reads: VecDeque<Result<VisiblePaneGrid, CodexPromptCaptureFailure>>,
    calls: Vec<SourceCall>,
}

impl ScriptedSource {
    fn new(reads: Vec<Result<TopologyObservation, CaptureSourceFailure>>) -> Self {
        Self {
            source: SnapshotSource::try_from_bytes(b"/tmp/source.sock".to_vec()).unwrap(),
            reads: reads.into(),
            inspections: 0,
            observation: PaneProcessObservation::Idle,
            observations: VecDeque::new(),
            visible_reads: VecDeque::new(),
            calls: Vec::new(),
        }
    }

    fn with_observation(mut self, observation: PaneProcessObservation) -> Self {
        self.observation = observation;
        self
    }

    fn with_observations(mut self, observations: Vec<PaneProcessObservation>) -> Self {
        self.observations = observations.into();
        self
    }

    fn with_visible_reads(
        mut self,
        reads: Vec<Result<VisiblePaneGrid, CodexPromptCaptureFailure>>,
    ) -> Self {
        self.visible_reads = reads.into();
        self
    }
}

impl CaptureSource for ScriptedSource {
    fn source(&self) -> &SnapshotSource {
        &self.source
    }

    fn read_topology(&mut self) -> Result<TopologyObservation, CaptureSourceFailure> {
        self.calls.push(SourceCall::ReadTopology);
        self.reads.pop_front().expect("scripted topology read")
    }

    fn inspect_pane(&mut self, pane: &TopologyPane) -> PaneProcessObservation {
        self.inspections += 1;
        self.calls
            .push(SourceCall::InspectPane(pane.pane_id().as_str().to_owned()));
        self.observations
            .pop_front()
            .unwrap_or_else(|| self.observation.clone())
    }

    fn read_visible_pane(
        &mut self,
        pane: &TopologyPane,
    ) -> Result<VisiblePaneGrid, CodexPromptCaptureFailure> {
        self.calls.push(SourceCall::ReadVisiblePane(
            pane.pane_id().as_str().to_owned(),
        ));
        self.visible_reads
            .pop_front()
            .expect("scripted visible-pane read")
    }
}

fn capture_time() -> CaptureTime {
    CaptureTime::parse_rfc3339("2026-07-23T00:00:00Z").unwrap()
}

#[test]
fn stable_capture_uses_one_complete_attempt() {
    let observed = topology("work", &[0, 1]);
    let mut source = ScriptedSource::new(vec![Ok(observed.clone()), Ok(observed)]);

    let result = capture_snapshot(&mut source, capture_time()).unwrap();

    assert_eq!(result.attempts(), 1);
    assert_eq!(source.inspections, 2);
    assert!(matches!(
        result.snapshot().consistency(),
        CaptureConsistency::Stable
    ));
    assert!(result.events().is_empty());
}

#[test]
fn topology_mismatch_retries_the_full_capture() {
    let first = topology("work", &[0]);
    let second = topology("work", &[0, 1]);
    let mut source = ScriptedSource::new(vec![
        Ok(first),
        Ok(second.clone()),
        Ok(second.clone()),
        Ok(second),
    ]);

    let result = capture_snapshot(&mut source, capture_time()).unwrap();

    assert_eq!(result.attempts(), 2);
    assert_eq!(source.inspections, 3);
    assert_eq!(
        result.snapshot().sessions()[0].windows()[0].panes().len(),
        2
    );
    assert!(matches!(
        result.events(),
        [CaptureEvent::TopologyMismatch { attempt: 1 }]
    ));
}

#[test]
fn exhaustion_saves_the_most_recent_complete_candidate_as_unstable() {
    let mut reads = Vec::new();
    for attempt in 0..MAX_TOPOLOGY_VALIDATION_ATTEMPTS {
        reads.push(Ok(topology(&format!("before-{attempt}"), &[0])));
        reads.push(Ok(topology(&format!("after-{attempt}"), &[0])));
    }
    let mut source = ScriptedSource::new(reads);

    let result = capture_snapshot(&mut source, capture_time()).unwrap();

    assert_eq!(result.attempts(), MAX_TOPOLOGY_VALIDATION_ATTEMPTS);
    assert_eq!(
        result.snapshot().sessions()[0].name(),
        format!("before-{}", MAX_TOPOLOGY_VALIDATION_ATTEMPTS - 1)
    );
    assert!(matches!(
        result.snapshot().consistency(),
        CaptureConsistency::Unstable { attempts }
            if attempts.get() == MAX_TOPOLOGY_VALIDATION_ATTEMPTS
    ));
    assert!(matches!(
        result.events().last(),
        Some(CaptureEvent::UnstableCandidateSaved { attempts })
            if *attempts == MAX_TOPOLOGY_VALIDATION_ATTEMPTS
    ));
}

#[test]
fn failed_after_read_retains_a_candidate_but_failed_before_read_does_not_create_one() {
    let failure = CaptureSourceFailure::try_new("tmux read failed").unwrap();
    let mut source = ScriptedSource::new(vec![
        Ok(topology("candidate", &[0])),
        Err(failure.clone()),
        Err(failure.clone()),
        Err(failure.clone()),
    ]);

    let result = capture_snapshot(&mut source, capture_time()).unwrap();

    assert_eq!(result.snapshot().sessions()[0].name(), "candidate");
    assert!(matches!(
        result.snapshot().consistency(),
        CaptureConsistency::Unstable { .. }
    ));
}

#[test]
fn exhaustion_without_any_complete_candidate_is_fatal() {
    let failure = CaptureSourceFailure::try_new("tmux read failed").unwrap();
    let mut source = ScriptedSource::new(
        (0..MAX_TOPOLOGY_VALIDATION_ATTEMPTS)
            .map(|_| Err(failure.clone()))
            .collect(),
    );

    let error = capture_snapshot(&mut source, capture_time()).unwrap_err();

    assert_eq!(error.attempts(), MAX_TOPOLOGY_VALIDATION_ATTEMPTS);
    assert_eq!(error.events().len(), MAX_TOPOLOGY_VALIDATION_ATTEMPTS);
}

#[test]
fn unavailable_process_data_keeps_the_candidate_complete_and_emits_an_event() {
    let observed = topology("work", &[0]);
    let failure = CaptureFailure::try_new("foreground process disappeared").unwrap();
    let mut source = ScriptedSource::new(vec![Ok(observed.clone()), Ok(observed)])
        .with_observation(PaneProcessObservation::Unavailable(failure));

    let result = capture_snapshot(&mut source, capture_time()).unwrap();

    assert!(matches!(
        result.snapshot().sessions()[0].windows()[0].panes()[0].recovery(),
        PaneRecovery::Unavailable(_)
    ));
    assert!(matches!(
        result.events(),
        [CaptureEvent::PaneRecoveryUnavailable { attempt: 1, .. }]
    ));
}

#[test]
fn exact_codex_capture_attaches_visible_prompt_input() {
    let observed = topology_with_pane_id("work", 0, "%15");
    let mut source = ScriptedSource::new(vec![Ok(observed.clone()), Ok(observed)])
        .with_observation(codex_foreground())
        .with_visible_reads(vec![Ok(captured_grid("%15"))]);

    let result = capture_snapshot(&mut source, capture_time()).unwrap();

    let PaneRecovery::Automatic(AutomaticRecovery::Codex {
        session_id,
        prompt_area: Some(prompt_area),
    }) = result.snapshot().sessions()[0].windows()[0].panes()[0].recovery()
    else {
        panic!("expected enriched automatic Codex recovery");
    };
    assert_eq!(session_id.as_uuid().to_string(), CODEX_SESSION_ID);
    assert_eq!(prompt_area.text().as_str(), "draft\nsecond");
    assert_eq!(
        source.calls,
        [
            SourceCall::ReadTopology,
            SourceCall::InspectPane("%15".to_owned()),
            SourceCall::ReadVisiblePane("%15".to_owned()),
            SourceCall::ReadTopology,
        ]
    );
}

#[test]
fn non_codex_and_downgraded_panes_never_read_the_visible_grid() {
    let observed = topology("work", &[0, 1]);
    let mut source = ScriptedSource::new(vec![Ok(observed.clone()), Ok(observed)])
        .with_observations(vec![
            PaneProcessObservation::Foreground(Box::new(foreground(
                "/usr/bin/mdbook",
                &["mdbook", "serve", "-p", "3000"],
            ))),
            downgraded_codex_foreground(),
        ]);

    let result = capture_snapshot(&mut source, capture_time()).unwrap();

    assert!(
        source
            .calls
            .iter()
            .all(|call| !matches!(call, SourceCall::ReadVisiblePane(_)))
    );
    assert!(matches!(
        result.snapshot().sessions()[0].windows()[0].panes()[0].recovery(),
        PaneRecovery::Automatic(AutomaticRecovery::MdBookServe { .. })
    ));
    assert!(matches!(
        result.snapshot().sessions()[0].windows()[0].panes()[1].recovery(),
        PaneRecovery::Manual(_)
    ));
    assert!(matches!(
        result.events(),
        [CaptureEvent::ResolverDowngraded { attempt: 1, .. }]
    ));
}

#[test]
fn absent_prompt_input_emits_no_event() {
    let observed = topology_with_pane_id("work", 0, "%15");
    let mut source = ScriptedSource::new(vec![Ok(observed.clone()), Ok(observed)])
        .with_observation(codex_foreground())
        .with_visible_reads(vec![Ok(absent_grid("%15"))]);

    let result = capture_snapshot(&mut source, capture_time()).unwrap();

    assert!(matches!(
        result.snapshot().sessions()[0].windows()[0].panes()[0].recovery(),
        PaneRecovery::Automatic(AutomaticRecovery::Codex {
            prompt_area: None,
            ..
        })
    ));
    assert!(result.events().is_empty());
}

#[test]
fn unstyled_suggestion_retains_automatic_recovery_and_emits_one_safe_warning() {
    let observed = topology_with_pane_id("work", 0, "%15");
    let suggestion = "Ask Codex to do anything";
    let row = format!("› {suggestion}");
    let unstyled_grid = visible_grid("%15", &[&row, "", "  95% context left"], 2, 0);
    let mut source = ScriptedSource::new(vec![Ok(observed.clone()), Ok(observed)])
        .with_observation(codex_foreground())
        .with_visible_reads(vec![Ok(unstyled_grid)]);

    let result = capture_snapshot(&mut source, capture_time()).unwrap();

    assert!(matches!(
        result.snapshot().sessions()[0].windows()[0].panes()[0].recovery(),
        PaneRecovery::Automatic(AutomaticRecovery::Codex {
            prompt_area: None,
            ..
        })
    ));
    let [event] = result.events() else {
        panic!("expected exactly one prompt-capture skip event");
    };
    let CaptureEvent::CodexPromptCaptureSkipped {
        attempt: 1,
        failure,
        ..
    } = event
    else {
        panic!("expected a prompt-capture skip event");
    };
    let debug = format!("{event:?}");
    assert!(!failure.message().contains(suggestion));
    assert!(!debug.contains(suggestion), "prompt leaked: {debug}");
}

#[test]
fn skipped_prompt_input_retains_automatic_codex_recovery() {
    let observed = topology_with_pane_id("work", 0, "%15");
    let mut source = ScriptedSource::new(vec![Ok(observed.clone()), Ok(observed)])
        .with_observation(codex_foreground())
        .with_visible_reads(vec![Err(CodexPromptCaptureFailure::try_from_read_failure(
            "pane metadata changed",
        )
        .unwrap())]);

    let result = capture_snapshot(&mut source, capture_time()).unwrap();

    let PaneRecovery::Automatic(AutomaticRecovery::Codex {
        session_id,
        prompt_area: None,
    }) = result.snapshot().sessions()[0].windows()[0].panes()[0].recovery()
    else {
        panic!("expected automatic Codex recovery without prompt enrichment");
    };
    assert_eq!(session_id.as_uuid().to_string(), CODEX_SESSION_ID);
    assert!(matches!(
        result.events(),
        [CaptureEvent::CodexPromptCaptureSkipped { attempt: 1, .. }]
    ));
}

#[test]
fn prompt_failure_events_never_contain_prompt_text() {
    let observed = topology_with_pane_id("work", 0, "%15");
    let sensitive_grid = visible_grid(
        "%15",
        &[
            &format!("» {SENSITIVE_PROMPT}"),
            "  keep this private too",
            "",
            "  unknown footer",
        ],
        23,
        1,
    );
    let mut source = ScriptedSource::new(vec![Ok(observed.clone()), Ok(observed)])
        .with_observation(codex_foreground())
        .with_visible_reads(vec![Ok(sensitive_grid)]);

    let result = capture_snapshot(&mut source, capture_time()).unwrap();

    let PaneRecovery::Automatic(AutomaticRecovery::Codex {
        session_id,
        prompt_area: None,
    }) = result.snapshot().sessions()[0].windows()[0].panes()[0].recovery()
    else {
        panic!("expected prompt-free automatic Codex recovery");
    };
    assert_eq!(session_id.as_uuid().to_string(), CODEX_SESSION_ID);
    let [event] = result.events() else {
        panic!("expected exactly one prompt-capture skip event");
    };
    let CaptureEvent::CodexPromptCaptureSkipped {
        attempt,
        pane,
        failure,
    } = event
    else {
        panic!("expected a prompt-capture skip event");
    };
    assert_eq!(*attempt, 1);
    assert_eq!(
        pane,
        &tmux_rescue::SourcePaneCoordinate {
            session_name: "work".to_owned(),
            window_index: 1,
            pane_index: 0,
        }
    );
    let debug = format!("{event:?}");
    assert!(!debug.contains(SENSITIVE_PROMPT), "prompt leaked: {debug}");
    assert!(!failure.message().contains(SENSITIVE_PROMPT));
}

#[test]
fn topology_replacement_with_the_same_coordinate_retries_capture() {
    let first = topology_with_pane_id("work", 0, "%15");
    let replacement = topology_with_pane_id("work", 0, "%16");
    let mut source = ScriptedSource::new(vec![
        Ok(first),
        Ok(replacement.clone()),
        Ok(replacement.clone()),
        Ok(replacement),
    ])
    .with_observation(codex_foreground())
    .with_visible_reads(vec![Ok(absent_grid("%15")), Ok(absent_grid("%16"))]);

    let result = capture_snapshot(&mut source, capture_time()).unwrap();

    assert_eq!(result.attempts(), 2);
    assert_eq!(
        source
            .calls
            .iter()
            .filter_map(|call| match call {
                SourceCall::ReadVisiblePane(pane_id) => Some(pane_id.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        ["%15", "%16"]
    );
    assert!(matches!(
        result.events(),
        [CaptureEvent::TopologyMismatch { attempt: 1 }]
    ));
}
