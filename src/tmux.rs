use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use thiserror::Error;

use crate::{
    AutomaticPaneObservation, AutomaticRecovery, AutomaticRecoveryExpectation, CaptureFailure,
    CaptureSource, CaptureSourceFailure, CapturedCodexPromptArea, CodexPromptCaptureFailure,
    CodexPromptPasteFailure, CodexPromptPasteResult, CodexSessionId, GuardedPaneFailure,
    GuardedPaneOperation, GuardedPaneResult, LinuxProcessInspector, LosslessOsString,
    OwnedRestoreTarget, PaneInitialProcess, PaneProcessAnchor, PaneProcessObservation,
    PaneProcessProbe, PaneRecovery, RecordedAbsolutePath, RecoveryRestoreTarget,
    RestoreDestination, RestorePlan, RestoreTargetCapability, RestoreTargetState, RollbackFailure,
    RollbackFailureDisposition, RollbackOutcome, SnapshotSource, SourcePaneCoordinate,
    TargetClaimFailure, TargetDisposition, TargetShell, TmuxPaneId, TmuxSelector, TopologyFailure,
    TopologyObservation, TopologyPane, TopologySession, TopologyWindow, VisiblePaneGrid,
    VisiblePaneMetadata, classify_pane, parse_proc_stat,
};

const SOURCE_FIELDS: usize = 11;
const SOURCE_FORMAT: &str = concat!(
    "#{n:session_name}:#{session_name}",
    "#{n:window_index}:#{window_index}",
    "#{n:pane_index}:#{pane_index}",
    "#{n:session_path}:#{session_path}",
    "#{n:window_name}:#{window_name}",
    "#{n:pane_current_path}:#{pane_current_path}",
    "#{n:pane_pid}:#{pane_pid}",
    "#{n:pane_tty}:#{pane_tty}",
    "#{n:pane_start_command}:#{pane_start_command}",
    "#{n:default-shell}:#{default-shell}",
    "#{n:pane_id}:#{pane_id}",
);
const VISIBLE_PANE_METADATA_FIELDS: usize = 6;
const VISIBLE_PANE_METADATA_FORMAT: &str = concat!(
    "#{n:pane_id}:#{pane_id}",
    "#{n:pane_width}:#{pane_width}",
    "#{n:pane_height}:#{pane_height}",
    "#{n:cursor_x}:#{cursor_x}",
    "#{n:cursor_y}:#{cursor_y}",
    "#{n:pane_in_mode}:#{pane_in_mode}",
);

pub struct TmuxAdapter<P = LinuxProcessInspector> {
    source: SnapshotSource,
    selector: TmuxSelector,
    process_probe: P,
}

impl<P> TmuxAdapter<P> {
    pub fn new(source: SnapshotSource, process_probe: P) -> Self {
        let selector = TmuxSelector::SocketPath(source.path().as_os_str().to_owned());
        Self {
            source,
            selector,
            process_probe,
        }
    }

    pub fn source(&self) -> &SnapshotSource {
        &self.source
    }
}

impl TmuxAdapter<LinuxProcessInspector> {
    pub fn selected_source(
        selector: Option<TmuxSelector>,
    ) -> Result<TmuxAdapter<LinuxProcessInspector>, TmuxAdapterError> {
        let mut command = source_client_command(selector.as_ref());
        let output = command
            .args(["display-message", "-p", "#{n:socket_path}:#{socket_path}"])
            .output()
            .map_err(|error| TmuxAdapterError::CommandUnavailable(error.to_string()))?;
        let source =
            require_success("resolve selected tmux server", output).and_then(|stdout| {
                let mut records = parse_length_prefixed_records(&stdout, 1)?;
                if records.len() != 1 {
                    return Err(TmuxAdapterError::MalformedOutput(
                        "selected server response did not contain exactly one record".to_owned(),
                    ));
                }
                SnapshotSource::try_from_bytes(records.remove(0).remove(0))
                    .map_err(|error| TmuxAdapterError::MalformedOutput(error.to_string()))
            })?;
        let selector = selector
            .unwrap_or_else(|| TmuxSelector::SocketPath(source.path().as_os_str().to_owned()));
        Ok(TmuxAdapter {
            source,
            selector,
            process_probe: LinuxProcessInspector::new(),
        })
    }
}

impl<P: PaneProcessProbe> CaptureSource for TmuxAdapter<P> {
    fn source(&self) -> &SnapshotSource {
        &self.source
    }

    fn read_topology(&mut self) -> Result<TopologyObservation, CaptureSourceFailure> {
        self.read_source_topology().map_err(capture_source_failure)
    }

    fn inspect_pane(&mut self, pane: &TopologyPane) -> PaneProcessObservation {
        match self.process_probe.observe(pane) {
            Ok(observation) => observation,
            Err(error) => PaneProcessObservation::Unavailable(
                CaptureFailure::try_new(safe_text(error.to_string().as_bytes()))
                    .expect("escaped process diagnostics satisfy capture-failure invariants"),
            ),
        }
    }

    fn read_visible_pane(
        &mut self,
        pane: &TopologyPane,
    ) -> Result<VisiblePaneGrid, CodexPromptCaptureFailure> {
        self.read_source_visible_pane(pane)
    }
}

impl<P> TmuxAdapter<P> {
    fn read_source_topology(&self) -> Result<TopologyObservation, TmuxAdapterError> {
        let output = source_client_command(Some(&self.selector))
            .args(["list-panes", "-a", "-F", SOURCE_FORMAT])
            .output()
            .map_err(|error| TmuxAdapterError::CommandUnavailable(error.to_string()))?;
        let stdout = require_success("read tmux source topology", output)?;
        let records = parse_length_prefixed_records(&stdout, SOURCE_FIELDS)?;
        topology_from_records(records)
    }

    fn read_source_visible_pane(
        &self,
        pane: &TopologyPane,
    ) -> Result<VisiblePaneGrid, CodexPromptCaptureFailure> {
        let read_metadata = || {
            let output = source_client_command(Some(&self.selector))
                .args([
                    "display-message",
                    "-p",
                    "-t",
                    pane.pane_id().as_str(),
                    VISIBLE_PANE_METADATA_FORMAT,
                ])
                .output()
                .map_err(|error| TmuxAdapterError::CommandUnavailable(error.to_string()))?;
            let stdout = require_success("read visible tmux pane metadata", output)?;
            parse_visible_pane_metadata(&stdout)
        };
        let before =
            read_metadata().map_err(|_| CodexPromptCaptureFailure::visible_pane_read_failed())?;
        let output = source_client_command(Some(&self.selector))
            .args([
                "capture-pane",
                "-p",
                "-e",
                "-N",
                "-T",
                "-t",
                pane.pane_id().as_str(),
            ])
            .output()
            .map_err(|error| TmuxAdapterError::CommandUnavailable(error.to_string()))
            .and_then(|output| require_success("capture visible tmux pane", output))
            .map_err(|_| CodexPromptCaptureFailure::visible_pane_read_failed())?;
        let after =
            read_metadata().map_err(|_| CodexPromptCaptureFailure::visible_pane_read_failed())?;
        if before.pane_id() != pane.pane_id() || before != after {
            return Err(CodexPromptCaptureFailure::pane_metadata_changed());
        }
        VisiblePaneGrid::try_from_tmux_styled_capture(before, output)
            .map_err(|_| CodexPromptCaptureFailure::invalid_visible_pane())
    }
}

fn source_client_command(selector: Option<&TmuxSelector>) -> Command {
    let mut command = Command::new("tmux");
    command.args(["-u", "-N"]);
    if let Some(selector) = selector {
        selector.append_to(&mut command);
        command.env_remove("TMUX");
    }
    command
}

#[derive(Clone)]
struct SessionBuilder {
    working_directory: RecordedAbsolutePath,
    windows: BTreeMap<u32, WindowBuilder>,
}

#[derive(Clone)]
struct WindowBuilder {
    name: String,
    panes: BTreeMap<u32, TopologyPane>,
}

fn topology_from_records(
    records: Vec<Vec<Vec<u8>>>,
) -> Result<TopologyObservation, TmuxAdapterError> {
    let mut sessions: BTreeMap<String, SessionBuilder> = BTreeMap::new();
    for fields in records {
        let [
            session_name,
            window_index,
            pane_index,
            session_path,
            window_name,
            pane_path,
            pane_pid,
            pane_tty,
            pane_start_command,
            default_shell,
            pane_id,
        ]: [Vec<u8>; SOURCE_FIELDS] = fields.try_into().map_err(|_| {
            TmuxAdapterError::MalformedOutput("wrong source field count".to_owned())
        })?;
        let session_name = parse_utf8(session_name, "session name")?;
        let window_index = parse_u32(window_index, "window index")?;
        let pane_index = parse_u32(pane_index, "pane index")?;
        let pane_id = TmuxPaneId::try_from_bytes(pane_id)
            .map_err(|error| TmuxAdapterError::MalformedOutput(error.to_string()))?;
        let session_path = RecordedAbsolutePath::try_from_bytes(session_path)
            .map_err(|error| TmuxAdapterError::MalformedOutput(error.to_string()))?;
        let window_name = parse_utf8(window_name, "window name")?;
        let pane_path = RecordedAbsolutePath::try_from_bytes(pane_path)
            .map_err(|error| TmuxAdapterError::MalformedOutput(error.to_string()))?;
        let pane_pid = parse_u32(pane_pid, "pane process ID")?;
        let pane_tty = LosslessOsString::try_from_bytes(pane_tty)
            .map_err(|error| TmuxAdapterError::MalformedOutput(error.to_string()))?;
        let initial_process = if pane_start_command.is_empty() {
            let default_shell = canonical_shell(default_shell)?;
            PaneInitialProcess::DefaultShell {
                executable: default_shell,
            }
        } else {
            PaneInitialProcess::ExplicitCommand
        };
        let pane = TopologyPane::new(
            pane_index,
            pane_id,
            pane_path,
            PaneProcessAnchor::try_new(pane_pid, pane_tty, initial_process)
                .map_err(|error| TmuxAdapterError::MalformedOutput(error.to_string()))?,
        );

        let session = sessions
            .entry(session_name.clone())
            .or_insert_with(|| SessionBuilder {
                working_directory: session_path.clone(),
                windows: BTreeMap::new(),
            });
        if session.working_directory != session_path {
            return Err(TmuxAdapterError::MalformedOutput(format!(
                "conflicting session paths for {session_name:?}"
            )));
        }
        let window = session
            .windows
            .entry(window_index)
            .or_insert_with(|| WindowBuilder {
                name: window_name.clone(),
                panes: BTreeMap::new(),
            });
        if window.name != window_name {
            return Err(TmuxAdapterError::MalformedOutput(format!(
                "conflicting names for window {window_index} in {session_name:?}"
            )));
        }
        if window.panes.insert(pane_index, pane).is_some() {
            return Err(TmuxAdapterError::MalformedOutput(format!(
                "duplicate pane {pane_index} in window {window_index} of {session_name:?}"
            )));
        }
    }

    let sessions = sessions
        .into_iter()
        .map(|(name, session)| {
            let windows = session
                .windows
                .into_iter()
                .map(|(source_index, window)| {
                    TopologyWindow::new(
                        source_index,
                        window.name,
                        window.panes.into_values().collect(),
                    )
                })
                .collect();
            TopologySession::new(name, session.working_directory, windows)
        })
        .collect();
    TopologyObservation::try_new(sessions)
        .map_err(|error| TmuxAdapterError::MalformedOutput(error.to_string()))
}

fn canonical_shell(bytes: Vec<u8>) -> Result<LosslessOsString, TmuxAdapterError> {
    if bytes.is_empty() {
        return Err(TmuxAdapterError::MalformedOutput(
            "tmux default-shell is empty".to_owned(),
        ));
    }
    let shell = OsString::from_vec(bytes);
    let bytes = fs::canonicalize(&shell)
        .map(|path| path.into_os_string().into_vec())
        .unwrap_or_else(|_| shell.into_vec());
    LosslessOsString::try_from_bytes(bytes)
        .map_err(|error| TmuxAdapterError::MalformedOutput(error.to_string()))
}

fn parse_length_prefixed_records(
    output: &[u8],
    fields_per_record: usize,
) -> Result<Vec<Vec<Vec<u8>>>, TmuxAdapterError> {
    let mut cursor = 0;
    let mut records = Vec::new();
    while cursor < output.len() {
        let mut fields = Vec::with_capacity(fields_per_record);
        for _ in 0..fields_per_record {
            let length_start = cursor;
            while output.get(cursor).is_some_and(u8::is_ascii_digit) {
                cursor += 1;
            }
            if cursor == length_start || output.get(cursor) != Some(&b':') {
                return Err(TmuxAdapterError::MalformedOutput(
                    "invalid length-prefixed tmux field".to_owned(),
                ));
            }
            let length = std::str::from_utf8(&output[length_start..cursor])
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or_else(|| {
                    TmuxAdapterError::MalformedOutput("invalid tmux field length".to_owned())
                })?;
            cursor += 1;
            let end = cursor.checked_add(length).ok_or_else(|| {
                TmuxAdapterError::MalformedOutput("tmux field length overflow".to_owned())
            })?;
            let value = output.get(cursor..end).ok_or_else(|| {
                TmuxAdapterError::MalformedOutput("truncated tmux field".to_owned())
            })?;
            fields.push(value.to_vec());
            cursor = end;
        }
        if output.get(cursor) != Some(&b'\n') {
            return Err(TmuxAdapterError::MalformedOutput(
                "tmux record lacks its terminating newline".to_owned(),
            ));
        }
        cursor += 1;
        records.push(fields);
    }
    Ok(records)
}

fn parse_utf8(value: Vec<u8>, field: &str) -> Result<String, TmuxAdapterError> {
    String::from_utf8(value)
        .map_err(|_| TmuxAdapterError::MalformedOutput(format!("{field} is not valid UTF-8")))
}

fn parse_u32(value: Vec<u8>, field: &str) -> Result<u32, TmuxAdapterError> {
    let value = parse_utf8(value, field)?;
    value
        .parse()
        .map_err(|_| TmuxAdapterError::MalformedOutput(format!("{field} is not a u32")))
}

fn parse_u16(value: Vec<u8>, field: &str) -> Result<u16, TmuxAdapterError> {
    let value = parse_utf8(value, field)?;
    value
        .parse()
        .map_err(|_| TmuxAdapterError::MalformedOutput(format!("{field} is not a u16")))
}

fn parse_visible_pane_metadata(output: &[u8]) -> Result<VisiblePaneMetadata, TmuxAdapterError> {
    let mut records = parse_length_prefixed_records(output, VISIBLE_PANE_METADATA_FIELDS)?;
    if records.len() != 1 {
        return Err(TmuxAdapterError::MalformedOutput(
            "visible pane metadata did not contain exactly one record".to_owned(),
        ));
    }
    let [pane_id, width, height, cursor_x, cursor_y, in_mode]: [Vec<u8>;
        VISIBLE_PANE_METADATA_FIELDS] = records.remove(0).try_into().map_err(|_| {
        TmuxAdapterError::MalformedOutput(
            "visible pane metadata had the wrong field count".to_owned(),
        )
    })?;
    let in_mode = match in_mode.as_slice() {
        b"0" => false,
        b"1" => true,
        _ => {
            return Err(TmuxAdapterError::MalformedOutput(
                "pane mode is not canonical ASCII 0 or 1".to_owned(),
            ));
        }
    };
    VisiblePaneMetadata::try_new(
        TmuxPaneId::try_from_bytes(pane_id)
            .map_err(|error| TmuxAdapterError::MalformedOutput(error.to_string()))?,
        parse_u16(width, "pane width")?,
        parse_u16(height, "pane height")?,
        parse_u16(cursor_x, "cursor x")?,
        parse_u16(cursor_y, "cursor y")?,
        in_mode,
    )
    .map_err(|error| TmuxAdapterError::MalformedOutput(error.to_string()))
}

fn require_success(operation: &str, output: Output) -> Result<Vec<u8>, TmuxAdapterError> {
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(TmuxAdapterError::CommandFailed {
        operation: operation.to_owned(),
        diagnostic: safe_text(&output.stderr),
    })
}

const SERVER_FIELDS: usize = 4;
const SERVER_FORMAT: &str = concat!(
    "#{n:pid}:#{pid}",
    "#{n:start_time}:#{start_time}",
    "#{n:server_sessions}:#{server_sessions}",
    "#{n:@tmux_rescue_owner}:#{@tmux_rescue_owner}",
);
const CREATED_PANE_FIELDS: usize = 9;
const CREATED_PANE_FORMAT: &str = concat!(
    "#{n:session_id}:#{session_id}",
    "#{n:session_path}:#{session_path}",
    "#{n:window_id}:#{window_id}",
    "#{n:window_index}:#{window_index}",
    "#{n:window_name}:#{window_name}",
    "#{n:pane_id}:#{pane_id}",
    "#{n:pane_pid}:#{pane_pid}",
    "#{n:pane_tty}:#{pane_tty}",
    "#{n:pane_current_path}:#{pane_current_path}",
);
pub const MAX_AUTOMATIC_SETTLE_ATTEMPTS: usize = 21;
const AUTOMATIC_SETTLE_INTERVAL: Duration = Duration::from_millis(100);
const ROLLBACK_OBSERVATION_ATTEMPTS: usize = 20;
const ROLLBACK_OBSERVATION_INTERVAL: Duration = Duration::from_millis(50);

pub struct TmuxRestoreAdapter<P = LinuxProcessInspector> {
    process_probe: Option<P>,
}

impl TmuxRestoreAdapter<LinuxProcessInspector> {
    pub fn new() -> Self {
        Self::with_process_probe(LinuxProcessInspector::new())
    }
}

impl<P> TmuxRestoreAdapter<P> {
    pub fn with_process_probe(process_probe: P) -> Self {
        Self {
            process_probe: Some(process_probe),
        }
    }
}

impl Default for TmuxRestoreAdapter<LinuxProcessInspector> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: PaneProcessProbe + 'static> RestoreTargetCapability for TmuxRestoreAdapter<P> {
    fn claim(
        &mut self,
        destination: &RestoreDestination,
        shell: &TargetShell,
    ) -> Result<Box<dyn OwnedRestoreTarget>, TargetClaimFailure> {
        let process_probe = self.process_probe.take().ok_or_else(|| {
            TargetClaimFailure::new("this restore adapter has already claimed a target")
        })?;
        let token = random_owner_token()?;
        let config = TemporaryClaimConfig::create(&token)?;
        let child = start_capable_target_command(destination)
            .args(["-f"])
            .arg(config.path())
            .arg("start-server")
            .env_remove("TMUX")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                TargetClaimFailure::new(format!("tmux start-server is unavailable: {error}"))
            })?;
        let unconfirmed = UnconfirmedClaim {
            destination: destination.clone(),
            token,
        };
        let output = match child.wait_with_output() {
            Ok(output) => output,
            Err(error) => {
                return Err(unconfirmed
                    .into_failure(format!("waiting for tmux start-server failed: {error}")));
            }
        };
        if !output.status.success() {
            return Err(unconfirmed.into_failure(format!(
                "tmux start-server failed: {}",
                safe_text(&output.stderr)
            )));
        }

        let server = unconfirmed.confirm()?;
        Ok(Box::new(TmuxOwnedTarget {
            server,
            shell: shell.clone(),
            panes: HashMap::new(),
            process_probe,
        }))
    }
}

struct UnconfirmedClaim {
    destination: RestoreDestination,
    token: String,
}

enum UnconfirmedCleanup {
    NotAuthorized(String),
    CommandSucceeded,
    CommandFailed(String),
}

impl UnconfirmedCleanup {
    fn permits_absent_endpoint_as_removal(&self) -> bool {
        matches!(self, Self::CommandSucceeded)
    }
}

#[derive(Clone)]
struct MatchedServerIdentity {
    server: OwnedServer,
    sessions: u32,
}

struct FailedClaimCleanupProof {
    server: OwnedServer,
}

impl MatchedServerIdentity {
    fn observe(destination: &RestoreDestination, expected_token: &str) -> Result<Self, String> {
        let first = read_server_observation(destination)?;
        if first.owner_token != expected_token.as_bytes() {
            return Err("target ownership token does not match this claim attempt".to_owned());
        }
        let process = OwnedProcessIdentity::capture(first.process_id)?;
        let confirmed = read_server_observation(destination)?;
        if confirmed != first {
            return Err("target server identity changed during claim observation".to_owned());
        }
        process.ensure_same_live()?;
        Ok(Self {
            server: OwnedServer {
                destination: destination.clone(),
                token: expected_token.to_owned(),
                process,
                start_time: first.start_time,
            },
            sessions: first.sessions,
        })
    }

    fn cleanup_proof(&self) -> Result<FailedClaimCleanupProof, String> {
        if self.sessions != 0 {
            return Err("the matched target has sessions and cannot be cleaned up".to_owned());
        }
        Ok(FailedClaimCleanupProof {
            server: self.server.clone(),
        })
    }
}

impl FailedClaimCleanupProof {
    fn remove_if_still_owned(&self) -> Result<(), String> {
        self.server.ensure_cleanup_identity()?;
        self.server
            .run_cleanup_guarded(&[os("kill-server")], "remove unconfirmed restore target")?;
        Ok(())
    }
}

impl UnconfirmedClaim {
    fn confirm(self) -> Result<OwnedServer, TargetClaimFailure> {
        let matched = match MatchedServerIdentity::observe(&self.destination, &self.token) {
            Ok(matched) => matched,
            Err(reason) => return Err(self.into_failure(reason)),
        };
        if matched.sessions != 0 {
            return Err(self.into_failure_with_observation(
                "target ownership was not established after start-server",
                Some(matched),
            ));
        }
        Ok(matched.server)
    }

    fn into_failure(self, reason: impl Into<String>) -> TargetClaimFailure {
        let matched = MatchedServerIdentity::observe(&self.destination, &self.token).ok();
        self.into_failure_with_observation(reason, matched)
    }

    fn into_failure_with_observation(
        self,
        reason: impl Into<String>,
        matched: Option<MatchedServerIdentity>,
    ) -> TargetClaimFailure {
        let cleanup_attempt = match matched
            .as_ref()
            .ok_or_else(|| "complete matching cleanup evidence was unavailable".to_owned())
            .and_then(MatchedServerIdentity::cleanup_proof)
        {
            Ok(proof) => match proof.remove_if_still_owned() {
                Ok(()) => UnconfirmedCleanup::CommandSucceeded,
                Err(failure) => UnconfirmedCleanup::CommandFailed(safe_text(failure.as_bytes())),
            },
            Err(reason) => UnconfirmedCleanup::NotAuthorized(reason),
        };
        let disposition = self.observe_cleanup_disposition(matched.as_ref(), &cleanup_attempt);
        let cleanup = match (disposition, &cleanup_attempt) {
            (disposition, UnconfirmedCleanup::NotAuthorized(failure)) => format!(
                "unconfirmed-target cleanup was not authorized: {failure}; final disposition: {disposition:?}"
            ),
            (TargetDisposition::Removed, UnconfirmedCleanup::CommandSucceeded) => {
                "unconfirmed target was observably removed".to_owned()
            }
            (TargetDisposition::Removed, UnconfirmedCleanup::CommandFailed(failure)) => format!(
                "unconfirmed-target cleanup failed: {failure}; target was nevertheless observably removed"
            ),
            (disposition, UnconfirmedCleanup::CommandFailed(failure)) => format!(
                "unconfirmed-target cleanup failed: {failure}; final disposition: {disposition:?}"
            ),
            (disposition, UnconfirmedCleanup::CommandSucceeded) => format!(
                "unconfirmed target was not observably removed; final disposition: {disposition:?}"
            ),
        };
        TargetClaimFailure::with_target_state(
            format!("{}; {cleanup}", reason.into()),
            RestoreTargetState::Observed(disposition),
        )
    }

    fn observe_cleanup_disposition(
        &self,
        matched: Option<&MatchedServerIdentity>,
        cleanup: &UnconfirmedCleanup,
    ) -> TargetDisposition {
        if matches!(cleanup, UnconfirmedCleanup::NotAuthorized(_)) {
            return self.current_cleanup_disposition(matched, cleanup);
        }
        for _ in 0..ROLLBACK_OBSERVATION_ATTEMPTS {
            let disposition = self.current_cleanup_disposition(matched, cleanup);
            if disposition == TargetDisposition::Removed {
                return disposition;
            }
            thread::sleep(ROLLBACK_OBSERVATION_INTERVAL);
        }
        self.current_cleanup_disposition(matched, cleanup)
    }

    fn current_cleanup_disposition(
        &self,
        matched: Option<&MatchedServerIdentity>,
        cleanup: &UnconfirmedCleanup,
    ) -> TargetDisposition {
        matched.map_or(TargetDisposition::Unknown, |matched| {
            matched
                .server
                .observe_disposition(cleanup.permits_absent_endpoint_as_removal())
        })
    }
}

struct TemporaryClaimConfig {
    path: PathBuf,
}

impl TemporaryClaimConfig {
    fn create(token: &str) -> Result<Self, TargetClaimFailure> {
        let path = std::env::temp_dir().join(format!("tmux-rescue-claim-{token}.conf"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| {
                TargetClaimFailure::new(format!("create ownership config failed: {error}"))
            })?;
        if let Err(error) = writeln!(file, "set-option -s exit-empty off")
            .and_then(|_| writeln!(file, "set-option -s @tmux_rescue_owner {token}"))
            .and_then(|_| file.sync_all())
        {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(TargetClaimFailure::new(format!(
                "write ownership config failed: {error}"
            )));
        }
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryClaimConfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Clone)]
struct OwnedServer {
    destination: RestoreDestination,
    token: String,
    process: OwnedProcessIdentity,
    start_time: u64,
}

struct TmuxOwnedTarget<P> {
    server: OwnedServer,
    shell: TargetShell,
    panes: HashMap<SourcePaneCoordinate, RestoredPane>,
    process_probe: P,
}

struct RestoredPane {
    target_id: TmuxPaneId,
    process_id: u32,
    tty: LosslessOsString,
    working_directory: RecordedAbsolutePath,
}

#[derive(Clone, Debug)]
struct CreatedPane {
    session_id: String,
    session_directory: RecordedAbsolutePath,
    window_id: String,
    window_index: u32,
    window_name: String,
    pane_id: TmuxPaneId,
    process_id: u32,
    tty: LosslessOsString,
    current_directory: RecordedAbsolutePath,
}

impl<P: PaneProcessProbe + 'static> OwnedRestoreTarget for TmuxOwnedTarget<P> {
    fn create_topology(&mut self, plan: &RestorePlan) -> Result<(), TopologyFailure> {
        if plan.target_shell() != &self.shell || plan.destination() != &self.server.destination {
            return Err(TopologyFailure::new(
                "restore plan does not match the claimed target capability",
            ));
        }
        if !self.shell.matches_current_file() {
            return Err(TopologyFailure::new(
                "planned target shell changed before topology creation",
            ));
        }
        self.server
            .run_guarded(
                &[
                    os("set-option"),
                    os("-g"),
                    os("default-shell"),
                    self.shell.executable().as_os_str().to_owned(),
                ],
                "set target default shell",
            )
            .map_err(TopologyFailure::new)?;

        let directories = plan
            .panes()
            .iter()
            .map(|pane| (pane.coordinate().clone(), pane.action().directory().clone()))
            .collect::<HashMap<_, _>>();

        for session in plan.sessions() {
            let first_window = session
                .windows()
                .first()
                .ok_or_else(|| TopologyFailure::new("planned session has no windows"))?;
            let first_coordinate = first_window
                .pane_coordinates()
                .first()
                .ok_or_else(|| TopologyFailure::new("planned window has no panes"))?;
            let first_directory = directories
                .get(first_coordinate)
                .ok_or_else(|| TopologyFailure::new("planned pane directory is missing"))?;

            let mut command = vec![
                os("new-session"),
                os("-d"),
                os("-P"),
                os("-F"),
                os(CREATED_PANE_FORMAT),
                os("-s"),
                os(session.name()),
                os("-c"),
                session.directory().path().as_os_str().to_owned(),
                os("-n"),
                os(first_window.name()),
            ];
            command.extend(topology_placeholder_argv(&self.shell));
            let output = self
                .server
                .run_guarded(&command, "create session")
                .map_err(TopologyFailure::new)?;
            let created = parse_created_pane(&output).map_err(TopologyFailure::new)?;

            if first_window.source_index() != 0 {
                self.server
                    .run_guarded(
                        &[
                            os("move-window"),
                            os("-s"),
                            os(&created.window_id),
                            os("-t"),
                            os(format!(
                                "{}:{}",
                                created.session_id,
                                first_window.source_index()
                            )),
                        ],
                        "move first window to its recorded index",
                    )
                    .map_err(TopologyFailure::new)?;
            }
            let mut respawn = vec![
                os("respawn-pane"),
                os("-k"),
                os("-t"),
                os(created.pane_id.as_str()),
                os("-c"),
                first_directory.as_os_str().to_owned(),
            ];
            respawn.extend(self.shell.interactive_argv());
            self.server
                .run_guarded(&respawn, "start first pane in its recorded directory")
                .map_err(TopologyFailure::new)?;
            let refreshed = self
                .read_pane(&created.pane_id)
                .map_err(TopologyFailure::new)?;
            self.remember_pane(
                first_coordinate,
                session.directory().path(),
                first_window.source_index(),
                first_window.name(),
                first_directory,
                refreshed,
            )
            .map_err(TopologyFailure::new)?;

            for coordinate in &first_window.pane_coordinates()[1..] {
                let directory = directories
                    .get(coordinate)
                    .ok_or_else(|| TopologyFailure::new("planned pane directory is missing"))?;
                let created = self
                    .create_split(&created.window_id, directory)
                    .map_err(TopologyFailure::new)?;
                self.remember_pane(
                    coordinate,
                    session.directory().path(),
                    first_window.source_index(),
                    first_window.name(),
                    directory,
                    created,
                )
                .map_err(TopologyFailure::new)?;
            }

            for window in &session.windows()[1..] {
                let first_coordinate = window
                    .pane_coordinates()
                    .first()
                    .ok_or_else(|| TopologyFailure::new("planned window has no panes"))?;
                let first_directory = directories
                    .get(first_coordinate)
                    .ok_or_else(|| TopologyFailure::new("planned pane directory is missing"))?;
                let mut command = vec![
                    os("new-window"),
                    os("-d"),
                    os("-P"),
                    os("-F"),
                    os(CREATED_PANE_FORMAT),
                    os("-t"),
                    os(format!("{}:{}", created.session_id, window.source_index())),
                    os("-n"),
                    os(window.name()),
                    os("-c"),
                    first_directory.as_os_str().to_owned(),
                ];
                command.extend(self.shell.interactive_argv());
                let output = self
                    .server
                    .run_guarded(&command, "create window")
                    .map_err(TopologyFailure::new)?;
                let created_window = parse_created_pane(&output).map_err(TopologyFailure::new)?;
                self.remember_pane(
                    first_coordinate,
                    session.directory().path(),
                    window.source_index(),
                    window.name(),
                    first_directory,
                    created_window.clone(),
                )
                .map_err(TopologyFailure::new)?;
                for coordinate in &window.pane_coordinates()[1..] {
                    let directory = directories
                        .get(coordinate)
                        .ok_or_else(|| TopologyFailure::new("planned pane directory is missing"))?;
                    let created_pane = self
                        .create_split(&created_window.window_id, directory)
                        .map_err(TopologyFailure::new)?;
                    self.remember_pane(
                        coordinate,
                        session.directory().path(),
                        window.source_index(),
                        window.name(),
                        directory,
                        created_pane,
                    )
                    .map_err(TopologyFailure::new)?;
                }
            }
        }

        self.server
            .run_guarded(
                &[os("set-option"), os("-s"), os("exit-empty"), os("on")],
                "enable exit-empty after topology creation",
            )
            .map_err(TopologyFailure::new)?;
        Ok(())
    }

    fn rollback(self: Box<Self>) -> RollbackOutcome {
        self.server.rollback()
    }

    fn begin_recovery(self: Box<Self>) -> Box<dyn RecoveryRestoreTarget> {
        self
    }
}

impl<P: PaneProcessProbe> TmuxOwnedTarget<P> {
    fn read_pane(&self, pane_id: &TmuxPaneId) -> Result<CreatedPane, String> {
        let output = self.server.run_guarded(
            &[
                os("display-message"),
                os("-p"),
                os("-t"),
                os(pane_id.as_str()),
                os(CREATED_PANE_FORMAT),
            ],
            "read restored pane identity",
        )?;
        parse_created_pane(&output)
    }

    fn create_split(
        &self,
        window_id: &str,
        directory: &RecordedAbsolutePath,
    ) -> Result<CreatedPane, String> {
        let mut command = vec![
            os("split-window"),
            os("-d"),
            os("-P"),
            os("-F"),
            os(CREATED_PANE_FORMAT),
            os("-t"),
            os(window_id),
            os("-c"),
            directory.as_os_str().to_owned(),
        ];
        command.extend(self.shell.interactive_argv());
        let output = self.server.run_guarded(&command, "create pane")?;
        parse_created_pane(&output)
    }

    fn remember_pane(
        &mut self,
        coordinate: &SourcePaneCoordinate,
        session_directory: &RecordedAbsolutePath,
        window_index: u32,
        window_name: &str,
        pane_directory: &RecordedAbsolutePath,
        pane: CreatedPane,
    ) -> Result<(), String> {
        if &pane.session_directory != session_directory
            || pane.window_index != window_index
            || pane.window_name != window_name
            || &pane.current_directory != pane_directory
        {
            return Err(format!(
                "tmux created pane {} with topology or directory state that differs from the plan",
                pane.pane_id.as_str()
            ));
        }
        self.panes.insert(
            coordinate.clone(),
            RestoredPane {
                target_id: pane.pane_id,
                process_id: pane.process_id,
                tty: pane.tty,
                working_directory: pane_directory.clone(),
            },
        );
        Ok(())
    }

    fn observe_pane(&self, pane: &RestoredPane) -> Result<PaneProcessObservation, String> {
        let executable =
            LosslessOsString::try_from_bytes(self.shell.executable_identity().as_bytes().to_vec())
                .map_err(|error| error.to_string())?;
        let topology = TopologyPane::new(
            0,
            pane.target_id.clone(),
            pane.working_directory.clone(),
            PaneProcessAnchor::try_new(
                pane.process_id,
                pane.tty.clone(),
                PaneInitialProcess::DefaultShell { executable },
            )
            .map_err(|error| error.to_string())?,
        );
        self.process_probe
            .observe(&topology)
            .map_err(|error| error.to_string())
    }

    fn live_pane_condition(&self, pane: &RestoredPane) -> String {
        let live = "#{==:#{pane_dead},0}";
        let process = ["#{==:#{pane_pid},", &pane.process_id.to_string(), "}"].concat();
        let pane_condition = ["#{&&:", live, ",", &process, "}"].concat();
        ["#{&&:", &self.server.condition(), ",", &pane_condition, "}"].concat()
    }

    fn running_pane_condition(&self, pane: &RestoredPane) -> String {
        [
            "#{&&:",
            &self.live_pane_condition(pane),
            ",#{==:#{pane_input_off},0}}",
        ]
        .concat()
    }

    fn shell_pane_condition(&self, pane: &RestoredPane) -> String {
        let shell_name = Path::new(self.shell.executable().as_os_str())
            .file_name()
            .and_then(OsStr::to_str)
            .expect("validated target shell basenames are ASCII");
        let command = ["#{==:#{pane_current_command},", shell_name, "}"].concat();
        [
            "#{&&:",
            &self.running_pane_condition(pane),
            ",",
            &command,
            "}",
        ]
        .concat()
    }

    fn pane_still_exists(&self, pane: &RestoredPane) -> bool {
        let Ok(output) = run_target_stdout(
            &self.server.destination,
            &[
                os("display-message"),
                os("-p"),
                os("-t"),
                os(pane.target_id.as_str()),
                os("#{pane_id}"),
            ],
            "probe restored pane",
        ) else {
            return false;
        };
        let Some(pane_id) = output.strip_suffix(b"\n") else {
            return false;
        };
        TmuxPaneId::try_from_bytes(pane_id.to_vec()).is_ok_and(|pane_id| pane_id == pane.target_id)
    }

    fn blocked_prompt_input_failure(&self, pane: &RestoredPane) -> CodexPromptPasteFailure {
        if !self.pane_still_exists(pane) {
            return CodexPromptPasteFailure::PaneMissing;
        }
        match self.server.run_conditional_stdout(
            pane.target_id.as_str(),
            &self.live_pane_condition(pane),
            &[
                os("display-message"),
                os("-p"),
                os("-t"),
                os(pane.target_id.as_str()),
                os("#{pane_input_off}"),
            ],
            "classify blocked Codex prompt input",
        ) {
            Ok(output) if output == b"1\n" => CodexPromptPasteFailure::InputDisabled,
            _ => CodexPromptPasteFailure::PasteFailed,
        }
    }
}

impl<P: PaneProcessProbe + 'static> RecoveryRestoreTarget for TmuxOwnedTarget<P> {
    fn guarded_pane_operation(
        &mut self,
        coordinate: &SourcePaneCoordinate,
        shell: &TargetShell,
        operation: GuardedPaneOperation<'_>,
    ) -> GuardedPaneResult {
        let Some(pane) = self.panes.get(coordinate) else {
            return Err(GuardedPaneFailure::PaneMissing);
        };
        if shell != &self.shell {
            return Err(GuardedPaneFailure::Failed(
                "guarded input shell does not match the claimed target shell".to_owned(),
            ));
        }
        if !shell.matches_current_file() {
            return Err(GuardedPaneFailure::Failed(
                "planned target shell changed before guarded pane input".to_owned(),
            ));
        }
        if let GuardedPaneOperation::LaunchAutomatic { input } = operation
            && !input.executable().matches_current_file()
        {
            return Err(GuardedPaneFailure::Failed(
                "planned automatic executable changed before launch".to_owned(),
            ));
        }
        match self.observe_pane(pane) {
            Ok(PaneProcessObservation::Idle) => {}
            Ok(PaneProcessObservation::Foreground(_)) => {
                return Err(GuardedPaneFailure::ShellNotForeground);
            }
            Ok(PaneProcessObservation::Unavailable(failure)) => {
                return Err(GuardedPaneFailure::Failed(failure.message().to_owned()));
            }
            Err(reason) => {
                if !self.pane_still_exists(pane) {
                    return Err(GuardedPaneFailure::PaneMissing);
                }
                return Err(GuardedPaneFailure::Failed(reason));
            }
        }

        let condition = self.shell_pane_condition(pane);
        let result = match operation {
            GuardedPaneOperation::VerifyShell => self.server.run_conditional(
                pane.target_id.as_str(),
                &condition,
                &[
                    os("display-message"),
                    os("-p"),
                    os("TMUX_RESCUE_SHELL_VERIFIED"),
                ],
                "verify restored pane shell",
            ),
            GuardedPaneOperation::PasteLiteral { input } => {
                let buffer_name = format!("tmux-rescue-{}", self.server.token);
                let commands =
                    literal_paste_commands(pane.target_id.as_str(), input.as_bytes(), &buffer_name);
                self.server.run_conditional_commands(
                    pane.target_id.as_str(),
                    &condition,
                    &commands,
                    "send guarded pane input",
                )
            }
            GuardedPaneOperation::LaunchAutomatic { input } => {
                let buffer_name = format!("tmux-rescue-{}", self.server.token);
                let commands = automatic_launch_commands(
                    pane.target_id.as_str(),
                    input.rendered().as_bytes(),
                    &buffer_name,
                );
                self.server.run_conditional_commands(
                    pane.target_id.as_str(),
                    &condition,
                    &commands,
                    "send guarded pane input",
                )
            }
        };
        match result {
            Ok(()) => Ok(()),
            Err(ConditionalFailure::Blocked) => Err(GuardedPaneFailure::ShellNotForeground),
            Err(ConditionalFailure::NotDispatched(reason) | ConditionalFailure::Failed(reason)) => {
                Err(GuardedPaneFailure::Failed(reason))
            }
        }
    }

    fn observe_automatic(
        &mut self,
        coordinate: &SourcePaneCoordinate,
        expected: &AutomaticRecoveryExpectation,
    ) -> AutomaticPaneObservation {
        let Some(pane) = self.panes.get(coordinate) else {
            return AutomaticPaneObservation::PaneMissing;
        };
        let mut latest =
            AutomaticPaneObservation::Failed("automatic recovery could not be observed".to_owned());
        for attempt in 0..MAX_AUTOMATIC_SETTLE_ATTEMPTS {
            if attempt != 0 {
                thread::sleep(AUTOMATIC_SETTLE_INTERVAL);
            }
            latest = match self.observe_pane(pane) {
                Ok(PaneProcessObservation::Idle) => AutomaticPaneObservation::ShellForeground,
                Ok(PaneProcessObservation::Foreground(evidence)) => {
                    let classification = classify_pane(*evidence);
                    match classification.recovery() {
                        PaneRecovery::Automatic(actual) if expected.matches(actual) => {
                            return AutomaticPaneObservation::Recovered;
                        }
                        _ => AutomaticPaneObservation::UnexpectedForeground,
                    }
                }
                Ok(PaneProcessObservation::Unavailable(failure)) => {
                    AutomaticPaneObservation::Failed(failure.message().to_owned())
                }
                Err(reason) => {
                    if !self.pane_still_exists(pane) {
                        return AutomaticPaneObservation::PaneMissing;
                    }
                    AutomaticPaneObservation::Failed(reason)
                }
            };
        }
        latest
    }

    fn paste_codex_prompt_area(
        &mut self,
        coordinate: &SourcePaneCoordinate,
        expected: &CodexSessionId,
        input: &CapturedCodexPromptArea,
    ) -> CodexPromptPasteResult {
        let Some(pane) = self.panes.get(coordinate) else {
            return Err(CodexPromptPasteFailure::PaneMissing);
        };
        let observation = match self.observe_pane(pane) {
            Ok(observation) => observation,
            Err(_) => {
                if !self.pane_still_exists(pane) {
                    return Err(CodexPromptPasteFailure::PaneMissing);
                }
                return Err(CodexPromptPasteFailure::PasteFailed);
            }
        };
        let PaneProcessObservation::Foreground(evidence) = observation else {
            return match observation {
                PaneProcessObservation::Idle => Err(CodexPromptPasteFailure::SessionMismatch),
                PaneProcessObservation::Unavailable(_) => Err(CodexPromptPasteFailure::PasteFailed),
                PaneProcessObservation::Foreground(_) => unreachable!(),
            };
        };
        let classification = classify_pane(*evidence);
        let PaneRecovery::Automatic(AutomaticRecovery::Codex { session_id, .. }) =
            classification.recovery()
        else {
            return Err(CodexPromptPasteFailure::SessionMismatch);
        };
        if session_id != expected {
            return Err(CodexPromptPasteFailure::SessionMismatch);
        }

        let buffer_name = format!(
            "tmux-rescue-{}-{}",
            self.server.token,
            uuid::Uuid::new_v4().simple()
        );
        let condition = self.running_pane_condition(pane);
        let BufferedPasteCommands {
            create_buffer,
            paste_buffer,
            cleanup_buffer,
        } = buffered_paste_commands(
            pane.target_id.as_str(),
            input.text().as_str().as_bytes(),
            &buffer_name,
        );
        let paste_commands = [create_buffer, paste_buffer];
        match self.server.run_conditional_commands(
            pane.target_id.as_str(),
            &condition,
            &paste_commands,
            "prepare Codex prompt input",
        ) {
            Ok(()) => return Ok(()),
            Err(ConditionalFailure::Blocked) => {
                return Err(self.blocked_prompt_input_failure(pane));
            }
            Err(ConditionalFailure::NotDispatched(_)) => {
                return Err(CodexPromptPasteFailure::PasteFailed);
            }
            Err(ConditionalFailure::Failed(_)) => {}
        }
        match self
            .server
            .run_guarded(&cleanup_buffer, "clean up Codex prompt buffer")
        {
            Ok(_) => Err(CodexPromptPasteFailure::PasteFailed),
            Err(_) => Err(CodexPromptPasteFailure::CleanupFailed),
        }
    }

    fn observe_disposition(&mut self) -> TargetDisposition {
        self.server.observe_disposition(false)
    }
}

struct BufferedPasteCommands {
    create_buffer: Vec<OsString>,
    paste_buffer: Vec<OsString>,
    cleanup_buffer: Vec<OsString>,
}

fn buffered_paste_commands(
    pane_id: &str,
    input: &[u8],
    buffer_name: &str,
) -> BufferedPasteCommands {
    BufferedPasteCommands {
        create_buffer: vec![
            os("set-buffer"),
            os("-b"),
            os(buffer_name),
            os("--"),
            OsString::from_vec(input.to_vec()),
        ],
        paste_buffer: vec![
            os("paste-buffer"),
            os("-d"),
            os("-p"),
            os("-r"),
            os("-b"),
            os(buffer_name),
            os("-t"),
            os(pane_id),
        ],
        cleanup_buffer: vec![os("delete-buffer"), os("-b"), os(buffer_name)],
    }
}

fn literal_paste_commands(pane_id: &str, input: &[u8], buffer_name: &str) -> Vec<Vec<OsString>> {
    let commands = buffered_paste_commands(pane_id, input, buffer_name);
    vec![commands.create_buffer, commands.paste_buffer]
}

fn topology_placeholder_argv(shell: &TargetShell) -> Vec<OsString> {
    vec![
        shell.executable().as_os_str().to_owned(),
        os("-c"),
        os("while :; do IFS= read -r _ || exit; done"),
    ]
}

fn automatic_launch_commands(pane_id: &str, input: &[u8], buffer_name: &str) -> Vec<Vec<OsString>> {
    let mut commands = literal_paste_commands(pane_id, input, buffer_name);
    commands.push(vec![os("send-keys"), os("-t"), os(pane_id), os("Enter")]);
    commands
}

impl OwnedServer {
    fn condition(&self) -> String {
        let token = ["#{==:#{@tmux_rescue_owner},", &self.token, "}"].concat();
        let process = ["#{==:#{pid},", &self.process.process_id.to_string(), "}"].concat();
        let start = ["#{==:#{start_time},", &self.start_time.to_string(), "}"].concat();
        ["#{&&:", &token, ",#{&&:", &process, ",", &start, "}}"].concat()
    }

    fn cleanup_condition(&self) -> String {
        ["#{&&:", &self.condition(), ",#{==:#{server_sessions},0}}"].concat()
    }

    fn run_guarded(&self, command: &[OsString], operation: &str) -> Result<Vec<u8>, String> {
        self.run_with_condition(&self.condition(), command, operation)
    }

    fn run_cleanup_guarded(
        &self,
        command: &[OsString],
        operation: &str,
    ) -> Result<Vec<u8>, String> {
        self.run_with_condition(&self.cleanup_condition(), command, operation)
    }

    fn run_with_condition(
        &self,
        condition: &str,
        command: &[OsString],
        operation: &str,
    ) -> Result<Vec<u8>, String> {
        self.process.ensure_same_live()?;
        let marker = format!("TMUX_RESCUE_OWNERSHIP_LOST_{}", self.token);
        let output = run_target_stdout(
            &self.destination,
            &[
                os("if-shell"),
                os("-F"),
                os(condition),
                tmux_command(command),
                tmux_command(&[os("display-message"), os("-p"), os(&marker)]),
            ],
            operation,
        )?;
        if output == format!("{marker}\n").as_bytes() {
            return Err("restore target ownership was lost".to_owned());
        }
        Ok(output)
    }

    fn run_conditional(
        &self,
        pane_id: &str,
        condition: &str,
        command: &[OsString],
        operation: &str,
    ) -> Result<(), ConditionalFailure> {
        self.run_conditional_stdout(pane_id, condition, command, operation)
            .map(|_| ())
    }

    fn run_conditional_stdout(
        &self,
        pane_id: &str,
        condition: &str,
        command: &[OsString],
        operation: &str,
    ) -> Result<Vec<u8>, ConditionalFailure> {
        self.run_conditional_commands_stdout(pane_id, condition, &[command.to_vec()], operation)
    }

    fn run_conditional_commands(
        &self,
        pane_id: &str,
        condition: &str,
        commands: &[Vec<OsString>],
        operation: &str,
    ) -> Result<(), ConditionalFailure> {
        self.run_conditional_commands_stdout(pane_id, condition, commands, operation)
            .map(|_| ())
    }

    fn run_conditional_commands_stdout(
        &self,
        pane_id: &str,
        condition: &str,
        commands: &[Vec<OsString>],
        operation: &str,
    ) -> Result<Vec<u8>, ConditionalFailure> {
        self.process
            .ensure_same_live()
            .map_err(ConditionalFailure::NotDispatched)?;
        let marker = format!("TMUX_RESCUE_INPUT_BLOCKED_{}", self.token);
        let output = run_target_stdout(
            &self.destination,
            &[
                os("if-shell"),
                os("-F"),
                os("-t"),
                os(pane_id),
                os(condition),
                tmux_command_list(commands),
                tmux_command(&[os("display-message"), os("-p"), os(&marker)]),
            ],
            operation,
        )
        .map_err(ConditionalFailure::Failed)?;
        if output == format!("{marker}\n").as_bytes() {
            return Err(ConditionalFailure::Blocked);
        }
        Ok(output)
    }

    fn rollback(self) -> RollbackOutcome {
        let operation_failure = self
            .run_guarded(&[os("kill-server")], "roll back restore target")
            .err();
        for _ in 0..ROLLBACK_OBSERVATION_ATTEMPTS {
            let disposition = self.observe_disposition(true);
            if disposition == TargetDisposition::Removed {
                return RollbackOutcome::Removed;
            }
            thread::sleep(ROLLBACK_OBSERVATION_INTERVAL);
        }
        let disposition = self.observe_disposition(true);
        if disposition == TargetDisposition::Removed {
            return RollbackOutcome::Removed;
        }
        let failure_disposition = if disposition == TargetDisposition::Retained {
            RollbackFailureDisposition::Retained
        } else {
            RollbackFailureDisposition::Unknown
        };
        let reason = operation_failure.unwrap_or_else(|| {
            format!("restore target was not observably removed; final disposition: {disposition:?}")
        });
        RollbackOutcome::Failed(RollbackFailure::new(failure_disposition, reason))
    }

    fn observe_disposition(&self, removed_if_absent: bool) -> TargetDisposition {
        let observation = match read_server_observation(&self.destination) {
            Ok(observation) => observation,
            Err(_) => {
                return self
                    .process
                    .disposition_when_endpoint_is_absent(removed_if_absent);
            }
        };
        if observation.owner_token == self.token.as_bytes()
            && observation.process_id == self.process.process_id
            && observation.start_time == self.start_time
            && matches!(
                observe_owned_process(self.process),
                OwnedProcessState::SameLiveProcess
            )
        {
            TargetDisposition::Retained
        } else {
            TargetDisposition::Unknown
        }
    }

    fn ensure_cleanup_identity(&self) -> Result<(), String> {
        let observation = read_server_observation(&self.destination)?;
        if observation.owner_token != self.token.as_bytes()
            || observation.process_id != self.process.process_id
            || observation.start_time != self.start_time
        {
            return Err("unconfirmed target identity changed before cleanup".to_owned());
        }
        if observation.sessions != 0 {
            return Err("unconfirmed target acquired sessions before cleanup".to_owned());
        }
        self.process.ensure_same_live()
    }
}

#[derive(Clone, Copy)]
struct OwnedProcessIdentity {
    process_id: u32,
    proc_start_time: u64,
}

impl OwnedProcessIdentity {
    fn capture(process_id: u32) -> Result<Self, String> {
        match read_process_stat(process_id)? {
            Some(stat) if !matches!(stat.state(), b'Z' | b'X' | b'x') => Ok(Self {
                process_id,
                proc_start_time: stat.start_time(),
            }),
            Some(_) => Err(format!("process {process_id} is no longer live")),
            None => Err(format!("process {process_id} no longer exists")),
        }
    }

    fn disposition_when_endpoint_is_absent(
        self,
        removed_if_process_is_gone: bool,
    ) -> TargetDisposition {
        match observe_owned_process(self) {
            OwnedProcessState::SameLiveProcess => TargetDisposition::Retained,
            OwnedProcessState::GoneOrReused if removed_if_process_is_gone => {
                TargetDisposition::Removed
            }
            OwnedProcessState::GoneOrReused => TargetDisposition::Missing,
            OwnedProcessState::Indeterminate => TargetDisposition::Unknown,
        }
    }

    fn ensure_same_live(self) -> Result<(), String> {
        match observe_owned_process(self) {
            OwnedProcessState::SameLiveProcess => Ok(()),
            OwnedProcessState::GoneOrReused => Err(format!(
                "owned server process {} is gone or has been replaced",
                self.process_id
            )),
            OwnedProcessState::Indeterminate => Err(format!(
                "owned server process {} could not be verified",
                self.process_id
            )),
        }
    }
}

enum OwnedProcessState {
    SameLiveProcess,
    GoneOrReused,
    Indeterminate,
}

fn observe_owned_process(identity: OwnedProcessIdentity) -> OwnedProcessState {
    observe_owned_process_with(identity, read_process_stat)
}

fn observe_owned_process_with(
    identity: OwnedProcessIdentity,
    read_stat: impl FnOnce(u32) -> Result<Option<crate::ProcessStat>, String>,
) -> OwnedProcessState {
    let stat = match read_stat(identity.process_id) {
        Ok(Some(stat)) => stat,
        Ok(None) => return OwnedProcessState::GoneOrReused,
        Err(_) => return OwnedProcessState::Indeterminate,
    };
    if stat.start_time() != identity.proc_start_time || matches!(stat.state(), b'Z' | b'X' | b'x') {
        OwnedProcessState::GoneOrReused
    } else {
        OwnedProcessState::SameLiveProcess
    }
}

fn read_process_stat(process_id: u32) -> Result<Option<crate::ProcessStat>, String> {
    let path = PathBuf::from("/proc")
        .join(process_id.to_string())
        .join("stat");
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error.to_string()),
    };
    parse_proc_stat(process_id, &bytes)
        .map(Some)
        .map_err(|error| error.to_string())
}

enum ConditionalFailure {
    Blocked,
    NotDispatched(String),
    Failed(String),
}

#[derive(Debug, Eq, PartialEq)]
struct ServerObservation {
    process_id: u32,
    start_time: u64,
    sessions: u32,
    owner_token: Vec<u8>,
}

fn read_server_observation(destination: &RestoreDestination) -> Result<ServerObservation, String> {
    let output = run_target_stdout(
        destination,
        &[os("display-message"), os("-p"), os(SERVER_FORMAT)],
        "read target server identity",
    )?;
    let mut records =
        parse_length_prefixed_records(&output, SERVER_FIELDS).map_err(|error| error.to_string())?;
    if records.len() != 1 {
        return Err("target identity did not contain exactly one record".to_owned());
    }
    let fields = records.remove(0);
    let [process_id, start_time, sessions, owner_token]: [Vec<u8>; SERVER_FIELDS] = fields
        .try_into()
        .map_err(|_| "target identity had the wrong field count".to_owned())?;
    Ok(ServerObservation {
        process_id: parse_u32(process_id, "target server process ID")
            .map_err(|error| error.to_string())?,
        start_time: parse_ascii_u64(start_time, "target server start time")?,
        sessions: parse_u32(sessions, "target server session count")
            .map_err(|error| error.to_string())?,
        owner_token,
    })
}

fn parse_created_pane(output: &[u8]) -> Result<CreatedPane, String> {
    let mut records = parse_length_prefixed_records(output, CREATED_PANE_FIELDS)
        .map_err(|error| error.to_string())?;
    if records.len() != 1 {
        return Err("tmux pane creation did not return exactly one record".to_owned());
    }
    let fields = records.remove(0);
    let [
        session_id,
        session_directory,
        window_id,
        window_index,
        window_name,
        pane_id,
        process_id,
        tty,
        current_directory,
    ]: [Vec<u8>; CREATED_PANE_FIELDS] = fields
        .try_into()
        .map_err(|_| "tmux pane identity had the wrong field count".to_owned())?;
    Ok(CreatedPane {
        session_id: parse_tmux_id(session_id, b'$', "session ID")?,
        session_directory: RecordedAbsolutePath::try_from_bytes(session_directory)
            .map_err(|error| error.to_string())?,
        window_id: parse_tmux_id(window_id, b'@', "window ID")?,
        window_index: parse_u32(window_index, "window index").map_err(|error| error.to_string())?,
        window_name: parse_utf8(window_name, "window name").map_err(|error| error.to_string())?,
        pane_id: TmuxPaneId::try_from_bytes(pane_id).map_err(|error| error.to_string())?,
        process_id: parse_u32(process_id, "pane process ID").map_err(|error| error.to_string())?,
        tty: LosslessOsString::try_from_bytes(tty).map_err(|error| error.to_string())?,
        current_directory: RecordedAbsolutePath::try_from_bytes(current_directory)
            .map_err(|error| error.to_string())?,
    })
}

fn parse_tmux_id(value: Vec<u8>, prefix: u8, field: &str) -> Result<String, String> {
    if value.first() != Some(&prefix)
        || value.len() < 2
        || !value[1..].iter().all(u8::is_ascii_digit)
    {
        return Err(format!("{field} is malformed"));
    }
    String::from_utf8(value).map_err(|_| format!("{field} is not valid UTF-8"))
}

fn parse_ascii_u64(value: Vec<u8>, field: &str) -> Result<u64, String> {
    std::str::from_utf8(&value)
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| format!("{field} is not a u64"))
}

fn random_owner_token() -> Result<String, TargetClaimFailure> {
    let mut bytes = [0_u8; 32];
    let mut offset = 0;
    while offset < bytes.len() {
        let result = unsafe {
            libc::getrandom(bytes[offset..].as_mut_ptr().cast(), bytes.len() - offset, 0)
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(TargetClaimFailure::new(format!(
                "generate ownership token failed: {error}"
            )));
        }
        if result == 0 {
            return Err(TargetClaimFailure::new(
                "generate ownership token returned no bytes",
            ));
        }
        offset += usize::try_from(result)
            .map_err(|_| TargetClaimFailure::new("ownership token length overflow"))?;
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn run_target_stdout(
    destination: &RestoreDestination,
    args: &[OsString],
    operation: &str,
) -> Result<Vec<u8>, String> {
    let output = no_start_target_command(destination)
        .args(args)
        .env_remove("TMUX")
        .output()
        .map_err(|error| format!("{operation}: tmux is unavailable: {error}"))?;
    require_success(operation, output).map_err(|error| error.to_string())
}

fn start_capable_target_command(destination: &RestoreDestination) -> Command {
    let mut command = Command::new("tmux");
    command.arg("-u");
    destination.selector().append_to(&mut command);
    command.env_remove("TMUX");
    command
}

fn no_start_target_command(destination: &RestoreDestination) -> Command {
    let mut command = Command::new("tmux");
    command.args(["-u", "-N"]);
    destination.selector().append_to(&mut command);
    command.env_remove("TMUX");
    command
}

fn tmux_command(args: &[OsString]) -> OsString {
    tmux_command_list(&[args.to_vec()])
}

fn tmux_command_list(commands: &[Vec<OsString>]) -> OsString {
    let mut output = Vec::new();
    for (command_index, command) in commands.iter().enumerate() {
        if command_index != 0 {
            output.extend_from_slice(b" ; ");
        }
        for (argument_index, argument) in command.iter().enumerate() {
            if argument_index != 0 {
                output.push(b' ');
            }
            output.extend_from_slice(&tmux_quote(argument.as_os_str()));
        }
    }
    OsString::from_vec(output)
}

fn tmux_quote(argument: &OsStr) -> Vec<u8> {
    let mut output = Vec::with_capacity(argument.as_bytes().len() * 4 + 2);
    output.push(b'"');
    for byte in argument.as_bytes() {
        output.extend_from_slice(format!("\\{byte:03o}").as_bytes());
    }
    output.push(b'"');
    output
}

fn os(value: impl AsRef<OsStr>) -> OsString {
    value.as_ref().to_owned()
}

fn capture_source_failure(error: TmuxAdapterError) -> CaptureSourceFailure {
    CaptureSourceFailure::try_new(safe_text(error.to_string().as_bytes()))
        .expect("escaped tmux diagnostics satisfy capture-source failure invariants")
}

fn safe_text(bytes: &[u8]) -> String {
    let mut escaped = String::new();
    for character in String::from_utf8_lossy(bytes).chars() {
        escaped.extend(character.escape_default());
        if escaped.len() >= 3_500 {
            escaped.truncate(3_500);
            break;
        }
    }
    if escaped.is_empty() {
        "external command failed without a diagnostic".to_owned()
    } else {
        escaped
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum TmuxAdapterError {
    #[error("tmux executable is unavailable: {0}")]
    CommandUnavailable(String),
    #[error("{operation} failed: {diagnostic}")]
    CommandFailed {
        operation: String,
        diagnostic: String,
    },
    #[error("tmux output is malformed: {0}")]
    MalformedOutput(String),
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use crate::{
        LinuxProcessInspector, RestoreDestination, RestoreTargetState, SnapshotSource,
        TargetDisposition, TmuxAdapter, TmuxSelector,
    };

    use super::{
        OwnedProcessIdentity, OwnedProcessState, OwnedServer, RestoredPane, TemporaryClaimConfig,
        TmuxOwnedTarget, UnconfirmedClaim, automatic_launch_commands, literal_paste_commands,
        observe_owned_process_with, os, read_server_observation,
    };

    struct TestProcessGuard(u32);

    struct TemporaryTmuxServer {
        socket: PathBuf,
    }

    impl TemporaryTmuxServer {
        fn start(socket: &Path, working_directory: &Path, command: &Path) -> Self {
            let output = Command::new("tmux")
                .args(["-u", "-S"])
                .arg(socket)
                .args([
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-s",
                    "work",
                    "-x",
                    "80",
                    "-y",
                    "1",
                    "-c",
                ])
                .arg(working_directory)
                .arg(command)
                .env_remove("TMUX")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "failed to start isolated tmux: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Self {
                socket: socket.to_owned(),
            }
        }
    }

    impl Drop for TemporaryTmuxServer {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .args(["-u", "-N", "-S"])
                .arg(&self.socket)
                .arg("kill-server")
                .env_remove("TMUX")
                .status();
        }
    }

    fn destination(socket: &std::path::Path) -> RestoreDestination {
        RestoreDestination::from_selector(TmuxSelector::SocketPath(socket.as_os_str().to_owned()))
    }

    impl Drop for TestProcessGuard {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0 as i32, libc::SIGTERM);
            }
        }
    }

    #[test]
    fn owned_process_identity_rejects_same_pid_with_changed_os_start_time() {
        let identity = OwnedProcessIdentity {
            process_id: 4242,
            proc_start_time: 11,
        };

        let state = observe_owned_process_with(identity, |process_id| {
            let stat = format!(
                "{process_id} (tmux: server) S 1 {process_id} {process_id} 0 {process_id} 0 0 0 0 0 1 2 0 0 20 0 1 0 22\n"
            );
            Ok(Some(
                crate::parse_proc_stat(process_id, stat.as_bytes()).unwrap(),
            ))
        });

        assert!(matches!(state, OwnedProcessState::GoneOrReused));
    }

    #[test]
    fn real_adapter_returns_the_faint_suffix_proof() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("faint-suffix.sock");
        let renderer = temp.path().join("render-faint-suffix");
        fs::write(
            &renderer,
            "#!/bin/sh\nprintf '› '\nprintf '\\033[2mImplement {feature}\\033[0m'\nexec /bin/sleep 30\n",
        )
        .unwrap();
        fs::set_permissions(&renderer, fs::Permissions::from_mode(0o700)).unwrap();
        let _server = TemporaryTmuxServer::start(&socket, temp.path(), &renderer);
        let source =
            SnapshotSource::try_from_bytes(socket.as_os_str().as_bytes().to_vec()).unwrap();
        let adapter = TmuxAdapter::new(source, LinuxProcessInspector::new());
        let topology = adapter.read_source_topology().unwrap();
        let pane = &topology.sessions()[0].windows()[0].panes()[0];
        let deadline = Instant::now() + Duration::from_secs(2);
        let grid = loop {
            let grid = adapter.read_source_visible_pane(pane).unwrap();
            if grid.rows()[0].as_str() == "› Implement {feature}" {
                break grid;
            }
            assert!(
                Instant::now() < deadline,
                "isolated pane never rendered the faint suffix: {:?}",
                grid.rows()
            );
            thread::sleep(Duration::from_millis(10));
        };

        let row = &grid.rows()[0];
        assert_eq!(row.as_str(), "› Implement {feature}");
        assert_eq!(
            row.faint_suffix_after_non_faint_prefix("› ")
                .unwrap()
                .as_str(),
            "Implement {feature}"
        );
    }

    #[test]
    fn codex_prompt_paste_is_set_buffer_then_bracketed_paste_without_enter() {
        let input = "draft line\nsecond line: \u{4f60}\u{597d}";
        let commands = literal_paste_commands("%7", input.as_bytes(), "rescue-buffer");

        assert_eq!(
            commands,
            vec![
                vec![
                    os("set-buffer"),
                    os("-b"),
                    os("rescue-buffer"),
                    os("--"),
                    os(input),
                ],
                vec![
                    os("paste-buffer"),
                    os("-d"),
                    os("-p"),
                    os("-r"),
                    os("-b"),
                    os("rescue-buffer"),
                    os("-t"),
                    os("%7"),
                ],
            ]
        );
        assert_eq!(commands.len(), 2);
        assert!(
            commands
                .iter()
                .flatten()
                .all(|argument| argument != "Enter")
        );
    }

    struct GuardTestProbe;

    impl crate::PaneProcessProbe for GuardTestProbe {
        fn observe(
            &self,
            _pane: &crate::TopologyPane,
        ) -> Result<crate::PaneProcessObservation, crate::ProcessInspectionFailure> {
            unreachable!("guard construction does not inspect processes")
        }
    }

    #[test]
    fn running_pane_guard_requires_input_and_omits_only_the_shell_current_command_clause() {
        let target = TmuxOwnedTarget {
            server: OwnedServer {
                destination: destination(std::path::Path::new("/tmp/guard-test.sock")),
                token: "owner-token".to_owned(),
                process: OwnedProcessIdentity {
                    process_id: 101,
                    proc_start_time: 22,
                },
                start_time: 33,
            },
            shell: crate::TargetShell::try_from_bytes(b"/bin/sh".to_vec()).unwrap(),
            panes: std::collections::HashMap::new(),
            process_probe: GuardTestProbe,
        };
        let pane = RestoredPane {
            target_id: crate::TmuxPaneId::try_from_bytes(b"%7".to_vec()).unwrap(),
            process_id: 202,
            tty: crate::LosslessOsString::try_from_bytes(b"/dev/pts/7".to_vec()).unwrap(),
            working_directory: crate::RecordedAbsolutePath::try_from_bytes(b"/tmp".to_vec())
                .unwrap(),
        };

        let running = target.running_pane_condition(&pane);
        let shell = target.shell_pane_condition(&pane);

        assert_eq!(
            running,
            "#{&&:#{&&:#{&&:#{==:#{@tmux_rescue_owner},owner-token},#{&&:#{==:#{pid},101},#{==:#{start_time},33}}},#{&&:#{==:#{pane_dead},0},#{==:#{pane_pid},202}}},#{==:#{pane_input_off},0}}"
        );
        assert_eq!(
            shell,
            format!("#{{&&:{running},#{{==:#{{pane_current_command}},sh}}}}")
        );
    }

    #[test]
    fn automatic_launch_is_a_bracketed_literal_paste_then_separate_enter() {
        let commands = automatic_launch_commands(
            "%7",
            b"'codex' 'resume' '1d6381bf-01c5-4c4a-b725-8e376e5ad295'",
            "rescue-buffer",
        );

        assert_eq!(
            commands,
            vec![
                vec![
                    os("set-buffer"),
                    os("-b"),
                    os("rescue-buffer"),
                    os("--"),
                    os("'codex' 'resume' '1d6381bf-01c5-4c4a-b725-8e376e5ad295'"),
                ],
                vec![
                    os("paste-buffer"),
                    os("-d"),
                    os("-p"),
                    os("-r"),
                    os("-b"),
                    os("rescue-buffer"),
                    os("-t"),
                    os("%7"),
                ],
                vec![os("send-keys"), os("-t"), os("%7"), os("Enter")],
            ]
        );
    }

    #[test]
    fn failed_claim_readback_removes_only_the_server_with_our_token() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("unconfirmed-claim.sock");
        let destination = destination(&socket);
        let token = "11".repeat(32);
        let config = TemporaryClaimConfig::create(&token).unwrap();
        let output = Command::new("tmux")
            .args(["-u", "-S"])
            .arg(&socket)
            .args(["-f"])
            .arg(config.path())
            .arg("start-server")
            .env_remove("TMUX")
            .output()
            .unwrap();
        assert!(output.status.success());

        let failure = UnconfirmedClaim {
            destination: destination.clone(),
            token,
        }
        .into_failure("forced ownership readback failure");

        assert_eq!(
            failure.target_state(),
            &RestoreTargetState::Observed(TargetDisposition::Removed)
        );
        assert!(
            failure
                .message()
                .contains("forced ownership readback failure")
        );
        assert!(read_server_observation(&destination).is_err());
    }

    #[test]
    fn missing_socket_is_not_removal_while_the_owned_server_process_is_live() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("unlinked-owned-server.sock");
        let destination = destination(&socket);
        let token = "22".repeat(32);
        let config = TemporaryClaimConfig::create(&token).unwrap();
        let output = Command::new("tmux")
            .args(["-u", "-S"])
            .arg(&socket)
            .args(["-f"])
            .arg(config.path())
            .arg("start-server")
            .env_remove("TMUX")
            .output()
            .unwrap();
        assert!(output.status.success());
        let observation = read_server_observation(&destination).unwrap();
        let _process = TestProcessGuard(observation.process_id);
        let server = OwnedServer {
            destination,
            token,
            process: super::OwnedProcessIdentity::capture(observation.process_id).unwrap(),
            start_time: observation.start_time,
        };
        fs::remove_file(&socket).unwrap();

        assert_eq!(
            server.observe_disposition(true),
            TargetDisposition::Retained
        );
    }

    #[test]
    fn failed_unconfirmed_cleanup_does_not_infer_removal_from_a_missing_socket() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("unlinked-unconfirmed-server.sock");
        let destination = destination(&socket);
        let token = "33".repeat(32);
        let config = TemporaryClaimConfig::create(&token).unwrap();
        let output = Command::new("tmux")
            .args(["-u", "-S"])
            .arg(&socket)
            .args(["-f"])
            .arg(config.path())
            .arg("start-server")
            .env_remove("TMUX")
            .output()
            .unwrap();
        assert!(output.status.success());
        let observation = read_server_observation(&destination).unwrap();
        let _process = TestProcessGuard(observation.process_id);
        fs::remove_file(&socket).unwrap();

        let failure = UnconfirmedClaim { destination, token }
            .into_failure("forced ownership readback failure");

        assert_eq!(
            failure.target_state(),
            &RestoreTargetState::Observed(TargetDisposition::Unknown)
        );
    }
}
