use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use thiserror::Error;

use crate::{
    AutomaticPaneObservation, AutomaticRecoveryExpectation, CaptureFailure, CaptureSource,
    CaptureSourceFailure, ExecutionCheckedTarget, GuardedPaneFailure, GuardedPaneOperation,
    GuardedPaneResult, LinuxProcessInspector, LosslessOsString, OwnedRestoreTarget,
    PaneInitialProcess, PaneProcessAnchor, PaneProcessObservation, PaneProcessProbe, PaneRecovery,
    RecordedAbsolutePath, RecoveryRestoreTarget, RestorePlan, RestoreTargetCapability,
    RestoreTargetState, RollbackFailure, RollbackFailureDisposition, RollbackOutcome,
    SnapshotSource, SourcePaneCoordinate, TargetClaimFailure, TargetDisposition, TargetProbe,
    TargetShell, TmuxServerIdentity, TopologyFailure, TopologyObservation, TopologyPane,
    TopologySession, TopologyWindow, classify_pane, parse_proc_stat, probe_tmux_target,
};

const SOURCE_FIELDS: usize = 10;
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
);

pub struct TmuxAdapter<P = LinuxProcessInspector> {
    source: SnapshotSource,
    process_probe: P,
}

impl<P> TmuxAdapter<P> {
    pub fn new(source: SnapshotSource, process_probe: P) -> Self {
        Self {
            source,
            process_probe,
        }
    }

    pub fn source(&self) -> &SnapshotSource {
        &self.source
    }
}

impl TmuxAdapter<LinuxProcessInspector> {
    pub fn selected_source() -> Result<SnapshotSource, TmuxAdapterError> {
        let output = Command::new("tmux")
            .args([
                "-u",
                "-N",
                "display-message",
                "-p",
                "#{n:socket_path}:#{socket_path}",
            ])
            .output()
            .map_err(|error| TmuxAdapterError::CommandUnavailable(error.to_string()))?;
        require_success("resolve selected tmux server", output).and_then(|stdout| {
            let mut records = parse_length_prefixed_records(&stdout, 1)?;
            if records.len() != 1 {
                return Err(TmuxAdapterError::MalformedOutput(
                    "selected server response did not contain exactly one record".to_owned(),
                ));
            }
            SnapshotSource::try_from_bytes(records.remove(0).remove(0))
                .map_err(|error| TmuxAdapterError::MalformedOutput(error.to_string()))
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
}

impl<P> TmuxAdapter<P> {
    fn read_source_topology(&self) -> Result<TopologyObservation, TmuxAdapterError> {
        let output = Command::new("tmux")
            .args(["-u", "-N", "-S"])
            .arg(self.source.path().as_os_str())
            .args(["list-panes", "-a", "-F", SOURCE_FORMAT])
            .env_remove("TMUX")
            .output()
            .map_err(|error| TmuxAdapterError::CommandUnavailable(error.to_string()))?;
        let stdout = require_success("read tmux source topology", output)?;
        let records = parse_length_prefixed_records(&stdout, SOURCE_FIELDS)?;
        topology_from_records(records)
    }
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
        ]: [Vec<u8>; SOURCE_FIELDS] = fields.try_into().map_err(|_| {
            TmuxAdapterError::MalformedOutput("wrong source field count".to_owned())
        })?;
        let session_name = parse_utf8(session_name, "session name")?;
        let window_index = parse_u32(window_index, "window index")?;
        let pane_index = parse_u32(pane_index, "pane index")?;
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
    "#{n:socket_path}:#{socket_path}",
    "#{n:pid}:#{pid}",
    "#{n:start_time}:#{start_time}",
    "#{n:server_sessions}:#{server_sessions}",
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
    fn recheck(&mut self, target: &TmuxServerIdentity) -> TargetProbe {
        probe_tmux_target(target)
    }

    fn claim(
        &mut self,
        checked: ExecutionCheckedTarget,
        shell: &TargetShell,
    ) -> Result<Box<dyn OwnedRestoreTarget>, TargetClaimFailure> {
        if self.process_probe.is_none() {
            return Err(TargetClaimFailure::new(
                "this restore adapter has already claimed a target",
            ));
        }
        let token = random_owner_token()?;
        let config = TemporaryClaimConfig::create(&token)?;
        let output = Command::new("tmux")
            .args(["-u", "-S"])
            .arg(checked.target().socket_path().as_os_str())
            .args(["-f"])
            .arg(config.path())
            .arg("start-server")
            .env_remove("TMUX")
            .output()
            .map_err(|error| {
                TargetClaimFailure::new(format!("tmux start-server is unavailable: {error}"))
            })?;
        let unconfirmed = UnconfirmedClaim {
            target: checked.target().clone(),
            token,
        };
        if !output.status.success() {
            return Err(unconfirmed.into_failure(format!(
                "tmux start-server failed: {}",
                safe_text(&output.stderr)
            )));
        }

        let server = unconfirmed.confirm()?;
        let process_probe = self
            .process_probe
            .take()
            .expect("adapter reuse was rejected before target creation");
        Ok(Box::new(TmuxOwnedTarget {
            server,
            shell: shell.clone(),
            panes: HashMap::new(),
            process_probe,
        }))
    }
}

struct UnconfirmedClaim {
    target: TmuxServerIdentity,
    token: String,
}

enum UnconfirmedCleanup {
    CommandSucceeded,
    CommandFailed(String),
}

impl UnconfirmedCleanup {
    fn permits_absent_endpoint_as_removal(&self) -> bool {
        matches!(self, Self::CommandSucceeded)
    }
}

impl UnconfirmedClaim {
    fn confirm(self) -> Result<OwnedServer, TargetClaimFailure> {
        let verification = read_server_observation(&self.target).and_then(|server| {
            let observed_token = read_owner_token(&self.target)?;
            if observed_token != self.token
                || server.socket_path.as_bytes() != self.target.socket_path().as_bytes()
                || server.sessions != 0
            {
                return Err("target ownership was not established after start-server".to_owned());
            }
            Ok(server)
        });
        match verification {
            Ok(server) => {
                let process = match OwnedProcessIdentity::capture(server.process_id) {
                    Ok(process) => process,
                    Err(reason) => {
                        return Err(self.into_failure(format!(
                            "target process identity was unavailable after start-server: {reason}"
                        )));
                    }
                };
                Ok(OwnedServer {
                    process,
                    target: self.target,
                    token: self.token,
                    start_time: server.start_time,
                })
            }
            Err(reason) => Err(self.into_failure(reason)),
        }
    }

    fn into_failure(self, reason: impl Into<String>) -> TargetClaimFailure {
        let owned_process = self.observed_owned_process();
        let cleanup_attempt = match self.remove_if_still_owned() {
            Ok(()) => UnconfirmedCleanup::CommandSucceeded,
            Err(failure) => UnconfirmedCleanup::CommandFailed(safe_text(failure.as_bytes())),
        };
        let disposition = self.observe_cleanup_disposition(owned_process, &cleanup_attempt);
        let cleanup = match (disposition, &cleanup_attempt) {
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

    fn observed_owned_process(&self) -> Option<OwnedProcessIdentity> {
        let server = read_server_observation(&self.target).ok()?;
        let token = read_owner_token(&self.target).ok()?;
        (token == self.token
            && server.socket_path.as_bytes() == self.target.socket_path().as_bytes())
        .then(|| OwnedProcessIdentity::capture(server.process_id).ok())
        .flatten()
    }

    fn remove_if_still_owned(&self) -> Result<(), String> {
        let marker = format!("TMUX_RESCUE_UNCONFIRMED_NOT_OWNED_{}", self.token);
        let condition = ["#{==:#{@tmux_rescue_owner},", &self.token, "}"].concat();
        let output = run_target_stdout(
            &self.target,
            &[
                os("if-shell"),
                os("-F"),
                os(condition),
                tmux_command(&[os("kill-server")]),
                tmux_command(&[os("display-message"), os("-p"), os(&marker)]),
            ],
            "remove unconfirmed restore target",
        )?;
        if output == format!("{marker}\n").as_bytes() {
            return Err("ownership token did not match; target was left untouched".to_owned());
        }
        Ok(())
    }

    fn observe_cleanup_disposition(
        &self,
        owned_process: Option<OwnedProcessIdentity>,
        cleanup: &UnconfirmedCleanup,
    ) -> TargetDisposition {
        for _ in 0..ROLLBACK_OBSERVATION_ATTEMPTS {
            let disposition = self.current_cleanup_disposition(owned_process, cleanup);
            if disposition == TargetDisposition::Removed {
                return disposition;
            }
            thread::sleep(ROLLBACK_OBSERVATION_INTERVAL);
        }
        self.current_cleanup_disposition(owned_process, cleanup)
    }

    fn current_cleanup_disposition(
        &self,
        owned_process: Option<OwnedProcessIdentity>,
        cleanup: &UnconfirmedCleanup,
    ) -> TargetDisposition {
        match probe_tmux_target(&self.target) {
            TargetProbe::MissingPath | TargetProbe::RefusedSocket => match owned_process {
                Some(identity) => identity.disposition_when_endpoint_is_absent(true),
                None if cleanup.permits_absent_endpoint_as_removal() => TargetDisposition::Removed,
                None => TargetDisposition::Unknown,
            },
            TargetProbe::Indeterminate(_) => TargetDisposition::Unknown,
            TargetProbe::Present => match read_owner_token(&self.target) {
                Ok(token) if token == self.token => TargetDisposition::Retained,
                Ok(_) | Err(_) => TargetDisposition::Unknown,
            },
        }
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
    target: TmuxServerIdentity,
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
    target_id: String,
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
    pane_id: String,
    process_id: u32,
    tty: LosslessOsString,
    current_directory: RecordedAbsolutePath,
}

impl<P: PaneProcessProbe + 'static> OwnedRestoreTarget for TmuxOwnedTarget<P> {
    fn create_topology(&mut self, plan: &RestorePlan) -> Result<(), TopologyFailure> {
        if plan.target_shell() != &self.shell || plan.target() != &self.server.target {
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
                os(&created.pane_id),
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
    fn read_pane(&self, pane_id: &str) -> Result<CreatedPane, String> {
        let output = self.server.run_guarded(
            &[
                os("display-message"),
                os("-p"),
                os("-t"),
                os(pane_id),
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
                pane.pane_id
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

    fn pane_condition(&self, pane: &RestoredPane) -> String {
        let shell_name = Path::new(self.shell.executable().as_os_str())
            .file_name()
            .and_then(OsStr::to_str)
            .expect("validated target shell basenames are ASCII");
        let live = "#{==:#{pane_dead},0}";
        let process = ["#{==:#{pane_pid},", &pane.process_id.to_string(), "}"].concat();
        let command = ["#{==:#{pane_current_command},", shell_name, "}"].concat();
        let shell_condition = ["#{&&:", live, ",#{&&:", &process, ",", &command, "}}"].concat();
        [
            "#{&&:",
            &self.server.condition(),
            ",",
            &shell_condition,
            "}",
        ]
        .concat()
    }

    fn pane_still_exists(&self, pane: &RestoredPane) -> bool {
        run_target_stdout(
            &self.server.target,
            &[
                os("display-message"),
                os("-p"),
                os("-t"),
                os(&pane.target_id),
                os("#{pane_id}"),
            ],
            "probe restored pane",
        )
        .is_ok()
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

        let condition = self.pane_condition(pane);
        let result = match operation {
            GuardedPaneOperation::VerifyShell => self.server.run_conditional(
                &pane.target_id,
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
                    literal_paste_commands(&pane.target_id, input.as_bytes(), &buffer_name);
                self.server.run_conditional_commands(
                    &pane.target_id,
                    &condition,
                    &commands,
                    "send guarded pane input",
                )
            }
            GuardedPaneOperation::LaunchAutomatic { input } => {
                let buffer_name = format!("tmux-rescue-{}", self.server.token);
                let commands = automatic_launch_commands(
                    &pane.target_id,
                    input.rendered().as_bytes(),
                    &buffer_name,
                );
                self.server.run_conditional_commands(
                    &pane.target_id,
                    &condition,
                    &commands,
                    "send guarded pane input",
                )
            }
        };
        match result {
            Ok(()) => Ok(()),
            Err(ConditionalFailure::Blocked) => Err(GuardedPaneFailure::ShellNotForeground),
            Err(ConditionalFailure::Failed(reason)) => Err(GuardedPaneFailure::Failed(reason)),
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

    fn observe_disposition(&mut self) -> TargetDisposition {
        self.server.observe_disposition(false)
    }
}

fn literal_paste_commands(pane_id: &str, input: &[u8], buffer_name: &str) -> Vec<Vec<OsString>> {
    vec![
        vec![
            os("set-buffer"),
            os("-b"),
            os(buffer_name),
            os("--"),
            OsString::from_vec(input.to_vec()),
        ],
        vec![
            os("paste-buffer"),
            os("-d"),
            os("-p"),
            os("-r"),
            os("-b"),
            os(buffer_name),
            os("-t"),
            os(pane_id),
        ],
    ]
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

    fn run_guarded(&self, command: &[OsString], operation: &str) -> Result<Vec<u8>, String> {
        let marker = format!("TMUX_RESCUE_OWNERSHIP_LOST_{}", self.token);
        let output = run_target_stdout(
            &self.target,
            &[
                os("if-shell"),
                os("-F"),
                os(self.condition()),
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
        self.run_conditional_commands(pane_id, condition, &[command.to_vec()], operation)
    }

    fn run_conditional_commands(
        &self,
        pane_id: &str,
        condition: &str,
        commands: &[Vec<OsString>],
        operation: &str,
    ) -> Result<(), ConditionalFailure> {
        let marker = format!("TMUX_RESCUE_INPUT_BLOCKED_{}", self.token);
        let output = run_target_stdout(
            &self.target,
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
        Ok(())
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
        match probe_tmux_target(&self.target) {
            TargetProbe::MissingPath | TargetProbe::RefusedSocket => self
                .process
                .disposition_when_endpoint_is_absent(removed_if_absent),
            TargetProbe::Indeterminate(_) => TargetDisposition::Unknown,
            TargetProbe::Present => match self.matches_live_server() {
                Ok(true) => TargetDisposition::Retained,
                Ok(false) | Err(_) => TargetDisposition::Unknown,
            },
        }
    }

    fn matches_live_server(&self) -> Result<bool, String> {
        let observation = read_server_observation(&self.target)?;
        let token = read_owner_token(&self.target)?;
        Ok(token == self.token
            && observation.socket_path.as_bytes() == self.target.socket_path().as_bytes()
            && observation.process_id == self.process.process_id
            && observation.start_time == self.start_time)
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
}

enum OwnedProcessState {
    SameLiveProcess,
    GoneOrReused,
    Indeterminate,
}

fn observe_owned_process(identity: OwnedProcessIdentity) -> OwnedProcessState {
    let stat = match read_process_stat(identity.process_id) {
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
    Failed(String),
}

struct ServerObservation {
    socket_path: RecordedAbsolutePath,
    process_id: u32,
    start_time: u64,
    sessions: u32,
}

fn read_server_observation(target: &TmuxServerIdentity) -> Result<ServerObservation, String> {
    let output = run_target_stdout(
        target,
        &[os("display-message"), os("-p"), os(SERVER_FORMAT)],
        "read target server identity",
    )?;
    let mut records =
        parse_length_prefixed_records(&output, SERVER_FIELDS).map_err(|error| error.to_string())?;
    if records.len() != 1 {
        return Err("target identity did not contain exactly one record".to_owned());
    }
    let fields = records.remove(0);
    let [socket_path, process_id, start_time, sessions]: [Vec<u8>; SERVER_FIELDS] = fields
        .try_into()
        .map_err(|_| "target identity had the wrong field count".to_owned())?;
    Ok(ServerObservation {
        socket_path: RecordedAbsolutePath::try_from_bytes(socket_path)
            .map_err(|error| error.to_string())?,
        process_id: parse_u32(process_id, "target server process ID")
            .map_err(|error| error.to_string())?,
        start_time: parse_ascii_u64(start_time, "target server start time")?,
        sessions: parse_u32(sessions, "target server session count")
            .map_err(|error| error.to_string())?,
    })
}

fn read_owner_token(target: &TmuxServerIdentity) -> Result<String, String> {
    let output = run_target_stdout(
        target,
        &[os("show-options"), os("-sv"), os("@tmux_rescue_owner")],
        "read target ownership token",
    )?;
    let token = output
        .strip_suffix(b"\n")
        .ok_or_else(|| "target ownership token lacks a terminating newline".to_owned())?;
    if token.is_empty() || token.contains(&b'\n') {
        return Err("target ownership token is malformed".to_owned());
    }
    String::from_utf8(token.to_vec())
        .map_err(|_| "target ownership token is not valid UTF-8".to_owned())
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
        pane_id: parse_tmux_id(pane_id, b'%', "pane ID")?,
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
    target: &TmuxServerIdentity,
    args: &[OsString],
    operation: &str,
) -> Result<Vec<u8>, String> {
    let output = Command::new("tmux")
        .args(["-u", "-N", "-S"])
        .arg(target.socket_path().as_os_str())
        .args(args)
        .env_remove("TMUX")
        .output()
        .map_err(|error| format!("{operation}: tmux is unavailable: {error}"))?;
    require_success(operation, output).map_err(|error| error.to_string())
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
    use std::process::Command;

    use crate::{RestoreTargetState, TargetDisposition, TmuxServerIdentity, probe_tmux_target};

    use super::{
        OwnedServer, TemporaryClaimConfig, UnconfirmedClaim, automatic_launch_commands,
        literal_paste_commands, os, read_server_observation,
    };

    struct TestProcessGuard(u32);

    impl Drop for TestProcessGuard {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0 as i32, libc::SIGTERM);
            }
        }
    }

    #[test]
    fn literal_hint_is_a_bracketed_tmux_paste_without_enter() {
        let commands = literal_paste_commands("%7", b"'touch' '/tmp/marker'", "rescue-buffer");

        assert_eq!(
            commands,
            vec![
                vec![
                    os("set-buffer"),
                    os("-b"),
                    os("rescue-buffer"),
                    os("--"),
                    os("'touch' '/tmp/marker'"),
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
        assert!(
            commands
                .iter()
                .flatten()
                .all(|argument| argument != "Enter")
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
        let target =
            TmuxServerIdentity::try_from_bytes(socket.as_os_str().as_bytes().to_vec()).unwrap();
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

        let failure =
            UnconfirmedClaim { target, token }.into_failure("forced ownership readback failure");

        assert_eq!(
            failure.target_state(),
            &RestoreTargetState::Observed(TargetDisposition::Removed)
        );
        assert!(
            failure
                .message()
                .contains("forced ownership readback failure")
        );
        assert!(matches!(
            probe_tmux_target(
                &TmuxServerIdentity::try_from_bytes(socket.as_os_str().as_bytes().to_vec())
                    .unwrap()
            ),
            crate::TargetProbe::MissingPath | crate::TargetProbe::RefusedSocket
        ));
    }

    #[test]
    fn missing_socket_is_not_removal_while_the_owned_server_process_is_live() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("unlinked-owned-server.sock");
        let target =
            TmuxServerIdentity::try_from_bytes(socket.as_os_str().as_bytes().to_vec()).unwrap();
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
        let observation = read_server_observation(&target).unwrap();
        let _process = TestProcessGuard(observation.process_id);
        let server = OwnedServer {
            target,
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
        let target =
            TmuxServerIdentity::try_from_bytes(socket.as_os_str().as_bytes().to_vec()).unwrap();
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
        let observation = read_server_observation(&target).unwrap();
        let _process = TestProcessGuard(observation.process_id);
        fs::remove_file(&socket).unwrap();

        let failure =
            UnconfirmedClaim { target, token }.into_failure("forced ownership readback failure");

        assert_eq!(
            failure.target_state(),
            &RestoreTargetState::Observed(TargetDisposition::Unknown)
        );
    }
}
