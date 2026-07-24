use std::collections::VecDeque;

use tmux_rescue::{
    CaptureConsistency, CaptureEvent, CaptureFailure, CaptureSource, CaptureSourceFailure,
    CaptureTime, LosslessOsString, MAX_TOPOLOGY_VALIDATION_ATTEMPTS, PaneInitialProcess,
    PaneProcessAnchor, PaneProcessObservation, PaneRecovery, RecordedAbsolutePath, SnapshotSource,
    TopologyObservation, TopologyPane, TopologySession, TopologyWindow, capture_snapshot,
};

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

struct ScriptedSource {
    source: SnapshotSource,
    reads: VecDeque<Result<TopologyObservation, CaptureSourceFailure>>,
    inspections: usize,
    observation: PaneProcessObservation,
}

impl ScriptedSource {
    fn new(reads: Vec<Result<TopologyObservation, CaptureSourceFailure>>) -> Self {
        Self {
            source: SnapshotSource::try_from_bytes(b"/tmp/source.sock".to_vec()).unwrap(),
            reads: reads.into(),
            inspections: 0,
            observation: PaneProcessObservation::Idle,
        }
    }

    fn with_observation(mut self, observation: PaneProcessObservation) -> Self {
        self.observation = observation;
        self
    }
}

impl CaptureSource for ScriptedSource {
    fn source(&self) -> &SnapshotSource {
        &self.source
    }

    fn read_topology(&mut self) -> Result<TopologyObservation, CaptureSourceFailure> {
        self.reads.pop_front().expect("scripted topology read")
    }

    fn inspect_pane(&mut self, _pane: &TopologyPane) -> PaneProcessObservation {
        self.inspections += 1;
        self.observation.clone()
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
