use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum, builder::TypedValueParser, error::ErrorKind};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use tmux_rescue::{
    AttentionReason, AutomaticFallbackReason, CaptureConsistency, CaptureEvent, CaptureTime,
    LatestDisposition, LinuxProcessInspector, PaneRestoreOutcome, RestoreExecutor, RestorePlan,
    RestoreRunResult, RestoreRunStatus, RestoreTargetState, SnapshotPublication,
    SourcePaneCoordinate, StateStore, SystemRestoreEnvironment, TargetDisposition, TmuxAdapter,
    TmuxRestoreAdapter, TmuxServerIdentity, TopologyReadPhase, capture_snapshot, plan_restore,
};

use crate::inspect::Palette;

pub const EXIT_SUCCESS: u8 = 0;
pub const EXIT_FAILURE: u8 = 1;
pub const EXIT_PARTIAL: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "tmux-rescue",
    version,
    about = "Snapshot and restore tmux programs"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorMode {
    fn enabled(self, automatic_support: bool) -> bool {
        match self {
            Self::Auto => automatic_support,
            Self::Always => true,
            Self::Never => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalColorSupport {
    stdout_auto: bool,
    stderr_auto: bool,
}

impl TerminalColorSupport {
    pub const fn new(stdout_auto: bool, stderr_auto: bool) -> Self {
        Self {
            stdout_auto,
            stderr_auto,
        }
    }

    fn stdout_palette(self, mode: ColorMode) -> Palette {
        Self::palette(mode.enabled(self.stdout_auto))
    }

    fn stderr_palette(self, mode: ColorMode) -> Palette {
        Self::palette(mode.enabled(self.stderr_auto))
    }

    fn palette(enabled: bool) -> Palette {
        if enabled {
            Palette::colored()
        } else {
            Palette::plain()
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Capture the tmux server selected by the invoking tmux context.
    Snapshot,
    /// Validate and display a captured tmux workspace.
    Inspect {
        /// Immutable snapshot path. The global latest snapshot is used when omitted.
        snapshot: Option<PathBuf>,
        /// When to use terminal color.
        #[arg(long, value_enum, default_value_t = ColorMode::Auto)]
        color: ColorMode,
    },
    /// Validate and print a restore plan, then optionally execute it.
    Restore {
        /// Immutable snapshot path. The global latest snapshot is used when omitted.
        snapshot: Option<PathBuf>,
        /// Absolute socket path of the absent target tmux server.
        #[arg(long, value_name = "server", value_parser = RestoreTargetParser)]
        target: Option<RestoreTarget>,
        /// Execute the printed plan.
        #[arg(long)]
        run: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreTarget(TmuxServerIdentity);

impl RestoreTarget {
    fn identity(&self) -> TmuxServerIdentity {
        self.0.clone()
    }
}

#[derive(Clone, Debug)]
struct RestoreTargetParser;

impl TypedValueParser for RestoreTargetParser {
    type Value = RestoreTarget;

    fn parse_ref(
        &self,
        command: &clap::Command,
        _argument: Option<&clap::Arg>,
        value: &OsStr,
    ) -> Result<Self::Value, clap::Error> {
        TmuxServerIdentity::try_from_bytes(value.as_bytes().to_vec())
            .map(RestoreTarget)
            .map_err(|error| {
                clap::Error::raw(
                    ErrorKind::ValueValidation,
                    format!("--target requires an absolute socket path: {error}"),
                )
                .with_cmd(command)
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreRequest {
    pub snapshot: Option<PathBuf>,
    pub target: Option<RestoreTarget>,
    pub run: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotSelection {
    Latest,
    Explicit(PathBuf),
}

impl From<Option<PathBuf>> for SnapshotSelection {
    fn from(path: Option<PathBuf>) -> Self {
        match path {
            Some(path) => Self::Explicit(path),
            None => Self::Latest,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectRequest {
    pub selection: SnapshotSelection,
    pub color: ColorMode,
}

pub trait CliRunner {
    fn snapshot(&mut self) -> u8;
    fn inspect(&mut self, request: InspectRequest) -> u8;
    fn restore(&mut self, request: RestoreRequest) -> u8;
}

pub fn dispatch(cli: Cli, runner: &mut impl CliRunner) -> u8 {
    match cli.command {
        Command::Snapshot => runner.snapshot(),
        Command::Inspect { snapshot, color } => runner.inspect(InspectRequest {
            selection: snapshot.into(),
            color,
        }),
        Command::Restore {
            snapshot,
            target,
            run,
        } => runner.restore(RestoreRequest {
            snapshot,
            target,
            run,
        }),
    }
}

pub struct SystemCliRunner<'a, W: Write, E: Write> {
    stdout: &'a mut W,
    stderr: &'a mut E,
    color_support: TerminalColorSupport,
}

impl<'a, W: Write, E: Write> SystemCliRunner<'a, W, E> {
    pub fn with_color_support(
        stdout: &'a mut W,
        stderr: &'a mut E,
        color_support: TerminalColorSupport,
    ) -> Self {
        Self {
            stdout,
            stderr,
            color_support,
        }
    }

    fn report_failure(&mut self, error: CliError) -> u8 {
        let _ = writeln!(self.stderr, "error: {}", safe_text(&error.to_string()));
        EXIT_FAILURE
    }
}

impl<W: Write, E: Write> CliRunner for SystemCliRunner<'_, W, E> {
    fn snapshot(&mut self) -> u8 {
        match run_snapshot(self.stdout, self.stderr) {
            Ok(code) => code,
            Err(error) => self.report_failure(error),
        }
    }

    fn inspect(&mut self, request: InspectRequest) -> u8 {
        let _palette = self.color_support.stdout_palette(request.color);
        EXIT_FAILURE
    }

    fn restore(&mut self, request: RestoreRequest) -> u8 {
        match run_restore(request, self.stdout, self.stderr) {
            Ok(code) => code,
            Err(error) => self.report_failure(error),
        }
    }
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct CliError {
    message: String,
}

impl CliError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: safe_text(&message.into()),
        }
    }
}

pub fn snapshot_exit_code(publication: &SnapshotPublication) -> u8 {
    match publication {
        SnapshotPublication::Published {
            latest:
                LatestDisposition::Updated
                | LatestDisposition::KeptNewer
                | LatestDisposition::ReplacedInvalid,
            ..
        } => EXIT_SUCCESS,
        SnapshotPublication::NotPublished(_)
        | SnapshotPublication::PublicationIndeterminate { .. }
        | SnapshotPublication::Published {
            latest: LatestDisposition::UpdateFailed(_),
            ..
        } => EXIT_FAILURE,
    }
}

pub fn restore_exit_code(status: RestoreRunStatus) -> u8 {
    match status {
        RestoreRunStatus::Complete => EXIT_SUCCESS,
        RestoreRunStatus::Fatal => EXIT_FAILURE,
        RestoreRunStatus::Partial => EXIT_PARTIAL,
    }
}

pub fn run_snapshot(stdout: &mut impl Write, stderr: &mut impl Write) -> Result<u8, CliError> {
    writeln!(stderr, "resolving source tmux server").map_err(io_failure)?;
    let source = TmuxAdapter::selected_source()
        .map_err(|error| CliError::new(format!("resolve source: {error}")))?;
    let captured_at = current_capture_time()?;
    let mut source = TmuxAdapter::new(source, LinuxProcessInspector::new());
    writeln!(stderr, "capturing tmux topology and foreground programs").map_err(io_failure)?;
    let capture = match capture_snapshot(&mut source, captured_at) {
        Ok(capture) => capture,
        Err(error) => {
            write_capture_events(stderr, error.events())?;
            return Err(CliError::new(format!("capture failed: {error}")));
        }
    };
    write_capture_events(stderr, capture.events())?;
    let store = StateStore::from_environment()
        .map_err(|error| CliError::new(format!("open state store: {error}")))?;
    let publication = store.publish(capture.snapshot());
    let exit_code = snapshot_exit_code(&publication);
    render_publication(stdout, stderr, &publication)?;
    Ok(exit_code)
}

pub fn run_restore(
    request: RestoreRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<u8, CliError> {
    let plan = match prepare_restore_plan(&request) {
        Ok(plan) => plan,
        Err(error) => {
            render_unestablished_failure(stdout)?;
            return Err(error);
        }
    };

    write!(stdout, "{plan}").map_err(io_failure)?;
    stdout.flush().map_err(io_failure)?;
    if !request.run {
        return Ok(EXIT_SUCCESS);
    }

    writeln!(stderr, "executing restore topology and pane recovery").map_err(io_failure)?;
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());
    let result = executor.execute(plan);
    render_restore_result(stdout, stderr, &result)?;
    Ok(restore_exit_code(result.status()))
}

fn prepare_restore_plan(request: &RestoreRequest) -> Result<RestorePlan, CliError> {
    let loaded = match &request.snapshot {
        Some(path) => StateStore::load_explicit_path(path),
        None => StateStore::from_environment()
            .map_err(|error| CliError::new(format!("open state store: {error}")))?
            .load_latest(),
    }
    .map_err(|error| CliError::new(format!("load snapshot: {error}")))?;
    let target = request.target.as_ref().map(RestoreTarget::identity);
    plan_restore(loaded.snapshot(), target, &SystemRestoreEnvironment)
        .map_err(|error| CliError::new(format!("plan restore: {error}")))
}

fn render_unestablished_failure(stdout: &mut impl Write) -> Result<(), CliError> {
    writeln!(stdout, "restore: fatal\ntarget state: not established").map_err(io_failure)
}

fn current_capture_time() -> Result<CaptureTime, CliError> {
    let encoded = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| CliError::new(format!("format capture time: {error}")))?;
    CaptureTime::parse_rfc3339(&encoded)
        .map_err(|error| CliError::new(format!("refine capture time: {error}")))
}

fn write_capture_events(stderr: &mut impl Write, events: &[CaptureEvent]) -> Result<(), CliError> {
    for event in events {
        match event {
            CaptureEvent::TopologyReadFailed {
                attempt,
                phase,
                failure,
            } => writeln!(
                stderr,
                "warning: capture attempt {attempt} {} topology read failed: {}",
                match phase {
                    TopologyReadPhase::Before => "before",
                    TopologyReadPhase::After => "after",
                },
                safe_text(failure.message())
            ),
            CaptureEvent::TopologyMismatch { attempt } => writeln!(
                stderr,
                "warning: capture attempt {attempt} topology changed; retrying"
            ),
            CaptureEvent::PaneRecoveryUnavailable {
                attempt,
                pane,
                failure,
            } => writeln!(
                stderr,
                "warning: capture attempt {attempt} pane {}:{}:{} recovery unavailable: {}",
                pane.session_name,
                pane.window_index,
                pane.pane_index,
                safe_text(failure.message())
            ),
            CaptureEvent::ResolverDowngraded {
                attempt,
                pane,
                outcome,
            } => writeln!(
                stderr,
                "warning: capture attempt {attempt} pane {}:{}:{} automatic recovery downgraded: {}",
                pane.session_name,
                pane.window_index,
                pane.pane_index,
                safe_text(&format!("{outcome:?}"))
            ),
            CaptureEvent::UnstableCandidateSaved { attempts } => writeln!(
                stderr,
                "warning: topology remained unstable after {attempts} attempts; publishing the latest complete candidate"
            ),
        }
        .map_err(io_failure)?;
    }
    Ok(())
}

fn render_publication(
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    publication: &SnapshotPublication,
) -> Result<(), CliError> {
    match publication {
        SnapshotPublication::NotPublished(failure) => {
            writeln!(
                stderr,
                "error: snapshot was not published: {}",
                safe_text(&failure.to_string())
            )
            .map_err(io_failure)?;
        }
        SnapshotPublication::PublicationIndeterminate {
            candidate_path,
            failure,
        } => {
            writeln!(
                stdout,
                "snapshot candidate: {}",
                display_path(candidate_path)
            )
            .map_err(io_failure)?;
            writeln!(
                stderr,
                "error: snapshot publication is indeterminate: {}",
                safe_text(&failure.to_string())
            )
            .map_err(io_failure)?;
        }
        SnapshotPublication::Published {
            snapshot_path,
            consistency,
            latest,
        } => {
            writeln!(stdout, "snapshot: {}", display_path(snapshot_path)).map_err(io_failure)?;
            writeln!(stdout, "consistency: {}", consistency_label(consistency))
                .map_err(io_failure)?;
            match latest {
                LatestDisposition::Updated => {
                    writeln!(stdout, "latest: updated").map_err(io_failure)?;
                }
                LatestDisposition::KeptNewer => {
                    writeln!(stdout, "latest: kept newer snapshot").map_err(io_failure)?;
                    writeln!(stderr, "warning: capture clock is not newer than latest")
                        .map_err(io_failure)?;
                }
                LatestDisposition::ReplacedInvalid => {
                    writeln!(stdout, "latest: replaced invalid pointer").map_err(io_failure)?;
                }
                LatestDisposition::UpdateFailed(error) => {
                    writeln!(stdout, "latest: update failed").map_err(io_failure)?;
                    writeln!(
                        stderr,
                        "error: immutable snapshot was published but latest update failed: {}",
                        safe_text(&error.to_string())
                    )
                    .map_err(io_failure)?;
                }
            }
        }
    }
    Ok(())
}

fn render_restore_result(
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    result: &RestoreRunResult,
) -> Result<(), CliError> {
    writeln!(
        stdout,
        "restore: {}\ntarget state: {}",
        match result.status() {
            RestoreRunStatus::Complete => "complete",
            RestoreRunStatus::Partial => "partial",
            RestoreRunStatus::Fatal => "fatal",
        },
        target_state_label(result.target_state())
    )
    .map_err(io_failure)?;
    for pane in result.panes() {
        let coordinate = pane.coordinate();
        writeln!(
            stdout,
            "pane {}:{}:{}: {}",
            coordinate.session_name,
            coordinate.window_index,
            coordinate.pane_index,
            pane_outcome_label(pane.outcome())
        )
        .map_err(io_failure)?;
        render_pane_warning(stderr, coordinate, pane.outcome())?;
    }
    if let Some(failure) = result.failure() {
        writeln!(stderr, "error: {}", safe_text(&failure.to_string())).map_err(io_failure)?;
    } else if result.status() == RestoreRunStatus::Partial {
        writeln!(stderr, "warning: restore completed partially").map_err(io_failure)?;
    }
    Ok(())
}

fn render_pane_warning(
    stderr: &mut impl Write,
    coordinate: &SourcePaneCoordinate,
    outcome: &PaneRestoreOutcome,
) -> Result<(), CliError> {
    if let PaneRestoreOutcome::NeedsAttention(reason) = outcome {
        writeln!(
            stderr,
            "warning: pane {}:{}:{} needs attention: {}",
            coordinate.session_name,
            coordinate.window_index,
            coordinate.pane_index,
            attention_label(reason)
        )
        .map_err(io_failure)?;
    } else if matches!(
        outcome,
        PaneRestoreOutcome::AutomaticLaunchFailedHintPrepared
    ) {
        writeln!(
            stderr,
            "warning: pane {}:{}:{} automatic launch failed; recovery hint prepared",
            coordinate.session_name, coordinate.window_index, coordinate.pane_index
        )
        .map_err(io_failure)?;
    } else if let PaneRestoreOutcome::PreparedAutomaticFallbackHint(reason) = outcome {
        writeln!(
            stderr,
            "warning: pane {}:{}:{} automatic recovery downgraded: {}",
            coordinate.session_name,
            coordinate.window_index,
            coordinate.pane_index,
            fallback_label(reason)
        )
        .map_err(io_failure)?;
    }
    Ok(())
}

fn consistency_label(consistency: &CaptureConsistency) -> String {
    match consistency {
        CaptureConsistency::Stable => "stable".to_owned(),
        CaptureConsistency::Unstable { attempts } => {
            format!("unstable after {} attempts", attempts.get())
        }
    }
}

fn target_state_label(state: &RestoreTargetState) -> &'static str {
    match state {
        RestoreTargetState::NotEstablished => "not established",
        RestoreTargetState::Observed(TargetDisposition::Removed) => "removed",
        RestoreTargetState::Observed(TargetDisposition::Retained) => "retained",
        RestoreTargetState::Observed(TargetDisposition::Missing) => "missing",
        RestoreTargetState::Observed(TargetDisposition::Unknown) => "unknown",
    }
}

fn pane_outcome_label(outcome: &PaneRestoreOutcome) -> String {
    match outcome {
        PaneRestoreOutcome::RestoredIdleShell => "restored idle shell".to_owned(),
        PaneRestoreOutcome::RecoveredAutomatically => "recovered automatically".to_owned(),
        PaneRestoreOutcome::PreparedManualHint => "prepared manual hint".to_owned(),
        PaneRestoreOutcome::PreparedAutomaticFallbackHint(reason) => {
            format!(
                "prepared automatic fallback hint ({})",
                fallback_label(reason)
            )
        }
        PaneRestoreOutcome::AutomaticLaunchFailedHintPrepared => {
            "automatic launch failed; prepared hint".to_owned()
        }
        PaneRestoreOutcome::NeedsAttention(reason) => {
            format!("needs attention ({})", attention_label(reason))
        }
    }
}

fn fallback_label(reason: &AutomaticFallbackReason) -> &'static str {
    match reason {
        AutomaticFallbackReason::RecordedDirectoryUnavailable => "recorded directory unavailable",
        AutomaticFallbackReason::ExecutableUnavailable => "executable unavailable",
    }
}

fn attention_label(reason: &AttentionReason) -> String {
    match reason {
        AttentionReason::ShellNotForeground => "shell is not foreground".to_owned(),
        AttentionReason::MissingPane => "target pane is missing".to_owned(),
        AttentionReason::UnexpectedForeground => "unexpected foreground process".to_owned(),
        AttentionReason::CapturedRecoveryUnavailable(failure) => {
            format!(
                "captured recovery unavailable: {}",
                safe_text(failure.message())
            )
        }
        AttentionReason::GuardedOperationFailed(reason) => {
            format!("guarded operation failed: {}", safe_text(reason))
        }
        AttentionReason::AutomaticObservationFailed(reason) => {
            format!("automatic observation failed: {}", safe_text(reason))
        }
    }
}

fn display_path(path: &Path) -> String {
    display_os(path.as_os_str())
}

fn display_os(value: &OsStr) -> String {
    match value.to_str() {
        Some(value) => safe_text(value),
        None => value
            .as_bytes()
            .iter()
            .map(|byte| {
                if matches!(byte, 0x20..=0x7e) {
                    char::from(*byte).to_string()
                } else {
                    format!("\\x{byte:02x}")
                }
            })
            .collect(),
    }
}

fn safe_text(value: &str) -> String {
    const MAX_CLI_DIAGNOSTIC_BYTES: usize = 4 * 1024;
    let mut output = String::new();
    for character in value.chars() {
        let fragment = if character.is_control() {
            character.escape_default().to_string()
        } else {
            character.to_string()
        };
        if output.len() + fragment.len() > MAX_CLI_DIAGNOSTIC_BYTES {
            break;
        }
        output.push_str(&fragment);
    }
    output
}

fn io_failure(error: std::io::Error) -> CliError {
    CliError::new(format!("write CLI output: {error}"))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use clap::Parser;
    use tmux_rescue::{CaptureConsistency, LatestDisposition, SnapshotPublication, StorageError};

    use super::*;

    #[derive(Default)]
    struct RecordingRunner {
        snapshot_calls: usize,
        inspect_requests: Vec<InspectRequest>,
        restore_requests: Vec<RestoreRequest>,
        snapshot_code: u8,
        inspect_code: u8,
        restore_code: u8,
    }

    impl CliRunner for RecordingRunner {
        fn snapshot(&mut self) -> u8 {
            self.snapshot_calls += 1;
            self.snapshot_code
        }

        fn inspect(&mut self, request: InspectRequest) -> u8 {
            self.inspect_requests.push(request);
            self.inspect_code
        }

        fn restore(&mut self, request: RestoreRequest) -> u8 {
            self.restore_requests.push(request);
            self.restore_code
        }
    }

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(args)
            .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"))
    }

    fn restore_command(cli: Cli) -> (Option<PathBuf>, Option<RestoreTarget>, bool) {
        match cli.command {
            Command::Restore {
                snapshot,
                target,
                run,
            } => (snapshot, target, run),
            Command::Snapshot | Command::Inspect { .. } => panic!("expected restore command"),
        }
    }

    fn inspect_command(cli: Cli) -> (Option<PathBuf>, ColorMode) {
        match cli.command {
            Command::Inspect { snapshot, color } => (snapshot, color),
            Command::Snapshot | Command::Restore { .. } => panic!("expected inspect command"),
        }
    }

    fn published(latest: LatestDisposition) -> SnapshotPublication {
        SnapshotPublication::Published {
            snapshot_path: PathBuf::from("/state/snapshots/snapshot.json"),
            consistency: CaptureConsistency::Stable,
            latest,
        }
    }

    #[test]
    fn parses_the_exact_command_surface() {
        assert!(matches!(
            parse(&["tmux-rescue", "snapshot"]).command,
            Command::Snapshot
        ));
        assert!(Cli::try_parse_from(["tmux-rescue", "snapshot", "unexpected"]).is_err());
        assert!(Cli::try_parse_from(["tmux-rescue", "snapshot", "--run"]).is_err());
        assert!(Cli::try_parse_from(["tmux-rescue", "restore", "--json"]).is_err());

        assert_eq!(
            inspect_command(parse(&["tmux-rescue", "inspect"])),
            (None, ColorMode::Auto)
        );
        assert_eq!(
            inspect_command(parse(&[
                "tmux-rescue",
                "inspect",
                "relative/snapshot.json",
                "--color",
                "always",
            ])),
            (
                Some(PathBuf::from("relative/snapshot.json")),
                ColorMode::Always,
            )
        );
        assert_eq!(
            inspect_command(parse(&[
                "tmux-rescue",
                "inspect",
                "--color",
                "never",
                "relative/snapshot.json",
            ])),
            (
                Some(PathBuf::from("relative/snapshot.json")),
                ColorMode::Never,
            )
        );
        assert!(Cli::try_parse_from(["tmux-rescue", "inspect", "--color", "sometimes"]).is_err());
        assert!(Cli::try_parse_from(["tmux-rescue", "inspect", "one.json", "two.json"]).is_err());

        let (snapshot, target, run) = restore_command(parse(&[
            "tmux-rescue",
            "restore",
            "/state/snapshot.json",
            "--target",
            "/tmp/target.sock",
            "--run",
        ]));
        assert_eq!(snapshot.as_deref(), Some(Path::new("/state/snapshot.json")));
        assert_eq!(
            target.unwrap().0.socket_path().as_bytes(),
            b"/tmp/target.sock"
        );
        assert!(run);

        let (snapshot, target, run) = restore_command(parse(&["tmux-rescue", "restore"]));
        assert_eq!((snapshot, target, run), (None, None, false));
    }

    #[test]
    fn dispatches_without_owning_orchestration() {
        let mut runner = RecordingRunner {
            snapshot_code: EXIT_FAILURE,
            restore_code: EXIT_PARTIAL,
            ..RecordingRunner::default()
        };
        assert_eq!(
            dispatch(parse(&["tmux-rescue", "snapshot"]), &mut runner),
            EXIT_FAILURE
        );
        assert_eq!(runner.snapshot_calls, 1);

        runner.inspect_code = EXIT_SUCCESS;
        assert_eq!(
            dispatch(
                parse(&[
                    "tmux-rescue",
                    "inspect",
                    "relative/snapshot.json",
                    "--color",
                    "never",
                ]),
                &mut runner,
            ),
            EXIT_SUCCESS
        );
        assert_eq!(
            runner.inspect_requests,
            [InspectRequest {
                selection: SnapshotSelection::Explicit(PathBuf::from("relative/snapshot.json")),
                color: ColorMode::Never,
            }]
        );

        assert_eq!(
            dispatch(
                parse(&[
                    "tmux-rescue",
                    "restore",
                    "/state/snapshot.json",
                    "--target",
                    "/tmp/target.sock",
                    "--run",
                ]),
                &mut runner,
            ),
            EXIT_PARTIAL
        );
        assert_eq!(runner.restore_requests.len(), 1);
        assert!(runner.restore_requests[0].run);
    }

    #[test]
    fn maps_typed_outcomes_to_documented_exit_codes() {
        for disposition in [
            LatestDisposition::Updated,
            LatestDisposition::ReplacedInvalid,
            LatestDisposition::KeptNewer,
        ] {
            assert_eq!(snapshot_exit_code(&published(disposition)), EXIT_SUCCESS);
        }
        assert_eq!(
            snapshot_exit_code(&published(LatestDisposition::UpdateFailed(
                StorageError::StateRootUnavailable("latest update failed".to_owned()),
            ))),
            EXIT_FAILURE
        );
        assert_eq!(restore_exit_code(RestoreRunStatus::Complete), EXIT_SUCCESS);
        assert_eq!(restore_exit_code(RestoreRunStatus::Fatal), EXIT_FAILURE);
        assert_eq!(restore_exit_code(RestoreRunStatus::Partial), EXIT_PARTIAL);
    }

    #[test]
    fn inspect_color_policy_resolves_per_stream() {
        assert!(!ColorMode::Auto.enabled(false));
        assert!(ColorMode::Auto.enabled(true));
        assert!(ColorMode::Always.enabled(false));
        assert!(ColorMode::Always.enabled(true));
        assert!(!ColorMode::Never.enabled(false));
        assert!(!ColorMode::Never.enabled(true));

        let support = TerminalColorSupport::new(true, false);
        assert_eq!(
            support.stdout_palette(ColorMode::Auto),
            crate::inspect::Palette::colored()
        );
        assert_eq!(
            support.stderr_palette(ColorMode::Auto),
            crate::inspect::Palette::plain()
        );
        assert_eq!(
            support.stderr_palette(ColorMode::Always).fatal_prefix(),
            "\x1b[31merror:\x1b[0m"
        );
        assert_eq!(
            support.stdout_palette(ColorMode::Never),
            crate::inspect::Palette::plain()
        );
    }

    #[test]
    fn automatic_failures_include_the_pane_coordinate_on_stderr() {
        let coordinate = SourcePaneCoordinate {
            session_name: "work".to_owned(),
            window_index: 3,
            pane_index: 7,
        };
        let mut stderr = Vec::new();

        render_pane_warning(
            &mut stderr,
            &coordinate,
            &PaneRestoreOutcome::AutomaticLaunchFailedHintPrepared,
        )
        .unwrap();

        let stderr = String::from_utf8(stderr).unwrap();
        assert!(stderr.contains("pane work:3:7"));
        assert!(stderr.contains("automatic launch failed"));
    }
}
