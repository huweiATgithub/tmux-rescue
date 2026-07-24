use std::collections::HashSet;
use std::path::Path;

use thiserror::Error;

use crate::{
    CaptureFailure, CaptureTime, LosslessOsString, MAX_PANES_PER_WINDOW, MAX_SESSIONS,
    MAX_TOPOLOGY_VALIDATION_ATTEMPTS, MAX_WINDOWS_PER_SESSION, PaneRecovery,
    PaneTiedForegroundEvidence, RawCaptureConsistency, RawPaneRecovery, RawPaneSnapshot,
    RawSessionSnapshot, RawSnapshot, RawWindowSnapshot, RecordedAbsolutePath, ResolverOutcome,
    SnapshotSource, SnapshotValidationError, ValidatedSnapshot, classify_pane,
    model::{validate_session_name, validate_window_name},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PaneInitialProcess {
    DefaultShell { executable: LosslessOsString },
    ExplicitCommand,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneProcessAnchor {
    pane_pid: u32,
    pane_tty: LosslessOsString,
    initial_process: PaneInitialProcess,
}

impl PaneProcessAnchor {
    pub fn try_new(
        pane_pid: u32,
        pane_tty: LosslessOsString,
        initial_process: PaneInitialProcess,
    ) -> Result<Self, PaneAnchorError> {
        if pane_pid == 0 {
            return Err(PaneAnchorError::ZeroPaneProcessId);
        }
        if !Path::new(pane_tty.as_os_str()).is_absolute() {
            return Err(PaneAnchorError::PaneTtyNotAbsolute);
        }
        if let PaneInitialProcess::DefaultShell { executable } = &initial_process
            && executable.as_bytes().is_empty()
        {
            return Err(PaneAnchorError::EmptyDefaultShell);
        }
        Ok(Self {
            pane_pid,
            pane_tty,
            initial_process,
        })
    }

    pub fn pane_pid(&self) -> u32 {
        self.pane_pid
    }

    pub fn pane_tty(&self) -> &LosslessOsString {
        &self.pane_tty
    }

    pub fn initial_process(&self) -> &PaneInitialProcess {
        &self.initial_process
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PaneAnchorError {
    #[error("pane process ID must be nonzero")]
    ZeroPaneProcessId,
    #[error("pane tty must be an absolute path")]
    PaneTtyNotAbsolute,
    #[error("default shell executable must be nonempty")]
    EmptyDefaultShell,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologyPane {
    source_index: u32,
    working_directory: RecordedAbsolutePath,
    process_anchor: PaneProcessAnchor,
}

impl TopologyPane {
    pub fn new(
        source_index: u32,
        working_directory: RecordedAbsolutePath,
        process_anchor: PaneProcessAnchor,
    ) -> Self {
        Self {
            source_index,
            working_directory,
            process_anchor,
        }
    }

    pub fn source_index(&self) -> u32 {
        self.source_index
    }

    pub fn working_directory(&self) -> &RecordedAbsolutePath {
        &self.working_directory
    }

    pub fn process_anchor(&self) -> &PaneProcessAnchor {
        &self.process_anchor
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologyWindow {
    source_index: u32,
    name: String,
    panes: Vec<TopologyPane>,
}

impl TopologyWindow {
    pub fn new(source_index: u32, name: String, panes: Vec<TopologyPane>) -> Self {
        Self {
            source_index,
            name,
            panes,
        }
    }

    pub fn source_index(&self) -> u32 {
        self.source_index
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn panes(&self) -> &[TopologyPane] {
        &self.panes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologySession {
    name: String,
    working_directory: RecordedAbsolutePath,
    windows: Vec<TopologyWindow>,
}

impl TopologySession {
    pub fn new(
        name: String,
        working_directory: RecordedAbsolutePath,
        windows: Vec<TopologyWindow>,
    ) -> Self {
        Self {
            name,
            working_directory,
            windows,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn working_directory(&self) -> &RecordedAbsolutePath {
        &self.working_directory
    }

    pub fn windows(&self) -> &[TopologyWindow] {
        &self.windows
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologyObservation {
    sessions: Vec<TopologySession>,
}

impl TopologyObservation {
    pub fn try_new(mut sessions: Vec<TopologySession>) -> Result<Self, SnapshotValidationError> {
        if sessions.is_empty() {
            return Err(SnapshotValidationError::EmptySessions);
        }
        if sessions.len() > MAX_SESSIONS {
            return Err(SnapshotValidationError::TooManySessions {
                actual: sessions.len(),
                maximum: MAX_SESSIONS,
            });
        }

        let mut session_names = HashSet::with_capacity(sessions.len());
        for (session_position, session) in sessions.iter_mut().enumerate() {
            validate_session_name(&session.name, session_position)?;
            if !session_names.insert(session.name.clone()) {
                return Err(SnapshotValidationError::DuplicateSessionName {
                    name: session.name.clone(),
                });
            }
            if session.windows.is_empty() {
                return Err(SnapshotValidationError::EmptyWindows {
                    session: session.name.clone(),
                });
            }
            if session.windows.len() > MAX_WINDOWS_PER_SESSION {
                return Err(SnapshotValidationError::TooManyWindows {
                    session: session.name.clone(),
                    actual: session.windows.len(),
                    maximum: MAX_WINDOWS_PER_SESSION,
                });
            }

            let mut window_indexes = HashSet::with_capacity(session.windows.len());
            for (window_position, window) in session.windows.iter_mut().enumerate() {
                validate_window_name(&window.name, session_position, window_position)?;
                if !window_indexes.insert(window.source_index) {
                    return Err(SnapshotValidationError::DuplicateWindowIndex {
                        session: session.name.clone(),
                        index: window.source_index,
                    });
                }
                if window.panes.is_empty() {
                    return Err(SnapshotValidationError::EmptyPanes {
                        session: session.name.clone(),
                        window_index: window.source_index,
                    });
                }
                if window.panes.len() > MAX_PANES_PER_WINDOW {
                    return Err(SnapshotValidationError::TooManyPanes {
                        session: session.name.clone(),
                        window_index: window.source_index,
                        actual: window.panes.len(),
                        maximum: MAX_PANES_PER_WINDOW,
                    });
                }
                let mut pane_indexes = HashSet::with_capacity(window.panes.len());
                for pane in &window.panes {
                    if !pane_indexes.insert(pane.source_index) {
                        return Err(SnapshotValidationError::DuplicatePaneIndex {
                            session: session.name.clone(),
                            window_index: window.source_index,
                            index: pane.source_index,
                        });
                    }
                }
                window.panes.sort_by_key(|pane| pane.source_index);
            }
            session.windows.sort_by_key(|window| window.source_index);
        }
        sessions.sort_by(|left, right| left.name.cmp(&right.name));

        Ok(Self { sessions })
    }

    pub fn sessions(&self) -> &[TopologySession] {
        &self.sessions
    }

    fn fingerprint(&self) -> TopologyFingerprint {
        TopologyFingerprint(
            self.sessions
                .iter()
                .map(|session| {
                    (
                        session.name.clone(),
                        session
                            .windows
                            .iter()
                            .map(|window| {
                                (
                                    window.source_index,
                                    window.panes.iter().map(|pane| pane.source_index).collect(),
                                )
                            })
                            .collect(),
                    )
                })
                .collect(),
        )
    }
}

type PaneFingerprint = Vec<u32>;
type WindowFingerprint = (u32, PaneFingerprint);
type SessionFingerprint = (String, Vec<WindowFingerprint>);

#[derive(Clone, Debug, Eq, PartialEq)]
struct TopologyFingerprint(Vec<SessionFingerprint>);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PaneProcessObservation {
    Idle,
    Foreground(Box<PaneTiedForegroundEvidence>),
    Unavailable(CaptureFailure),
}

pub trait CaptureSource {
    fn source(&self) -> &SnapshotSource;
    fn read_topology(&mut self) -> Result<TopologyObservation, CaptureSourceFailure>;
    fn inspect_pane(&mut self, pane: &TopologyPane) -> PaneProcessObservation;
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{0}")]
pub struct CaptureSourceFailure(CaptureFailure);

impl CaptureSourceFailure {
    pub fn try_new(message: impl Into<String>) -> Result<Self, SnapshotValidationError> {
        CaptureFailure::try_new(message).map(Self)
    }

    pub fn message(&self) -> &str {
        self.0.message()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TopologyReadPhase {
    Before,
    After,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SourcePaneCoordinate {
    pub session_name: String,
    pub window_index: u32,
    pub pane_index: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaptureEvent {
    TopologyReadFailed {
        attempt: usize,
        phase: TopologyReadPhase,
        failure: CaptureSourceFailure,
    },
    TopologyMismatch {
        attempt: usize,
    },
    PaneRecoveryUnavailable {
        attempt: usize,
        pane: SourcePaneCoordinate,
        failure: CaptureFailure,
    },
    ResolverDowngraded {
        attempt: usize,
        pane: SourcePaneCoordinate,
        outcome: ResolverOutcome,
    },
    UnstableCandidateSaved {
        attempts: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureResult {
    snapshot: ValidatedSnapshot,
    attempts: usize,
    events: Vec<CaptureEvent>,
}

impl CaptureResult {
    pub fn snapshot(&self) -> &ValidatedSnapshot {
        &self.snapshot
    }

    pub fn attempts(&self) -> usize {
        self.attempts
    }

    pub fn events(&self) -> &[CaptureEvent] {
        &self.events
    }

    pub fn into_snapshot(self) -> ValidatedSnapshot {
        self.snapshot
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CaptureError {
    #[error("no complete capture candidate was produced in {attempts} attempts")]
    NoCompleteCandidate {
        attempts: usize,
        events: Vec<CaptureEvent>,
    },
    #[error("capture produced an invalid internal candidate: {0}")]
    InvalidCandidate(SnapshotValidationError),
}

impl CaptureError {
    pub fn attempts(&self) -> usize {
        match self {
            Self::NoCompleteCandidate { attempts, .. } => *attempts,
            Self::InvalidCandidate(_) => 0,
        }
    }

    pub fn events(&self) -> &[CaptureEvent] {
        match self {
            Self::NoCompleteCandidate { events, .. } => events,
            Self::InvalidCandidate(_) => &[],
        }
    }
}

pub fn capture_snapshot(
    source: &mut impl CaptureSource,
    captured_at: CaptureTime,
) -> Result<CaptureResult, CaptureError> {
    let snapshot_source = source.source().clone();
    let mut events = Vec::new();
    let mut most_recent_candidate = None;

    for attempt in 1..=MAX_TOPOLOGY_VALIDATION_ATTEMPTS {
        let before = match source.read_topology() {
            Ok(topology) => topology,
            Err(failure) => {
                events.push(CaptureEvent::TopologyReadFailed {
                    attempt,
                    phase: TopologyReadPhase::Before,
                    failure,
                });
                continue;
            }
        };
        let before_fingerprint = before.fingerprint();
        let candidate = capture_candidate(source, &before, attempt, &mut events);

        match source.read_topology() {
            Ok(after) if after.fingerprint() == before_fingerprint => {
                return build_result(
                    captured_at,
                    snapshot_source,
                    RawCaptureConsistency::Stable {},
                    candidate,
                    attempt,
                    events,
                );
            }
            Ok(_) => events.push(CaptureEvent::TopologyMismatch { attempt }),
            Err(failure) => events.push(CaptureEvent::TopologyReadFailed {
                attempt,
                phase: TopologyReadPhase::After,
                failure,
            }),
        }
        most_recent_candidate = Some(candidate);
    }

    let Some(candidate) = most_recent_candidate else {
        return Err(CaptureError::NoCompleteCandidate {
            attempts: MAX_TOPOLOGY_VALIDATION_ATTEMPTS,
            events,
        });
    };
    events.push(CaptureEvent::UnstableCandidateSaved {
        attempts: MAX_TOPOLOGY_VALIDATION_ATTEMPTS,
    });
    build_result(
        captured_at,
        snapshot_source,
        RawCaptureConsistency::Unstable {
            attempts: MAX_TOPOLOGY_VALIDATION_ATTEMPTS,
        },
        candidate,
        MAX_TOPOLOGY_VALIDATION_ATTEMPTS,
        events,
    )
}

fn capture_candidate(
    source: &mut impl CaptureSource,
    topology: &TopologyObservation,
    attempt: usize,
    events: &mut Vec<CaptureEvent>,
) -> Vec<RawSessionSnapshot> {
    topology
        .sessions
        .iter()
        .map(|session| RawSessionSnapshot {
            name: session.name.clone(),
            working_directory: session.working_directory.to_raw(),
            windows: session
                .windows
                .iter()
                .map(|window| RawWindowSnapshot {
                    source_index: window.source_index,
                    name: window.name.clone(),
                    panes: window
                        .panes
                        .iter()
                        .map(|pane| {
                            let coordinate = SourcePaneCoordinate {
                                session_name: session.name.clone(),
                                window_index: window.source_index,
                                pane_index: pane.source_index,
                            };
                            let recovery = match source.inspect_pane(pane) {
                                PaneProcessObservation::Idle => PaneRecovery::Idle,
                                PaneProcessObservation::Unavailable(failure) => {
                                    events.push(CaptureEvent::PaneRecoveryUnavailable {
                                        attempt,
                                        pane: coordinate,
                                        failure: failure.clone(),
                                    });
                                    PaneRecovery::Unavailable(failure)
                                }
                                PaneProcessObservation::Foreground(evidence) => {
                                    let classification = classify_pane(*evidence);
                                    if matches!(
                                        classification.resolver_outcome(),
                                        ResolverOutcome::InsufficientEvidence(_)
                                            | ResolverOutcome::ConflictingEvidence(_)
                                    ) {
                                        events.push(CaptureEvent::ResolverDowngraded {
                                            attempt,
                                            pane: coordinate,
                                            outcome: classification.resolver_outcome().clone(),
                                        });
                                    }
                                    classification.into_recovery()
                                }
                            };
                            RawPaneSnapshot {
                                source_index: pane.source_index,
                                working_directory: pane.working_directory.to_raw(),
                                recovery: RawPaneRecovery::from(&recovery),
                            }
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect()
}

fn build_result(
    captured_at: CaptureTime,
    source: SnapshotSource,
    consistency: RawCaptureConsistency,
    sessions: Vec<RawSessionSnapshot>,
    attempts: usize,
    events: Vec<CaptureEvent>,
) -> Result<CaptureResult, CaptureError> {
    let snapshot = ValidatedSnapshot::from_capture_raw(RawSnapshot {
        captured_at: captured_at.encoded().to_owned(),
        source: source.path().to_raw(),
        consistency,
        sessions,
    })
    .map_err(CaptureError::InvalidCandidate)?;
    Ok(CaptureResult {
        snapshot,
        attempts,
        events,
    })
}
