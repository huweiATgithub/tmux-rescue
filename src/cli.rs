use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser, Subcommand, ValueEnum, error::ErrorKind};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use tmux_rescue::{
    AttentionReason, AutomaticFallbackReason, CaptureConsistency, CaptureEvent, CaptureTime,
    CodexPromptPasteFailure, LatestDisposition, PaneRestoreOutcome, RestoreEnvironment,
    RestoreExecutor, RestorePlan, RestoreRunResult, RestoreRunStatus, RestoreTargetCapability,
    RestoreTargetState, SnapshotPublication, SourcePaneCoordinate, StateStore,
    SystemRestoreEnvironment, TargetDisposition, TmuxAdapter, TmuxRestoreAdapter, TmuxSelector,
    TopologyReadPhase, capture_snapshot, plan_restore,
};

use crate::inspect::{Palette, is_unicode_display_control, render};

pub const EXIT_SUCCESS: u8 = 0;
pub const EXIT_FAILURE: u8 = 1;
pub const EXIT_PARTIAL: u8 = 2;

#[derive(Debug)]
pub struct Cli {
    pub command: Command,
}

#[derive(Debug, Parser)]
#[command(
    name = "tmux-rescue",
    version,
    about = "Snapshot and restore tmux programs"
)]
struct RawCli {
    #[arg(
        short = 'L',
        value_name = "SOCKET_NAME",
        conflicts_with = "socket_path"
    )]
    socket_name: Option<OsString>,
    #[arg(
        short = 'S',
        value_name = "SOCKET_PATH",
        conflicts_with = "socket_name"
    )]
    socket_path: Option<OsString>,
    #[command(subcommand)]
    command: RawCommand,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum IconMode {
    #[default]
    Unicode,
    Nerd,
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
enum RawCommand {
    /// Capture the ambient tmux server; root -L/-S before snapshot select one explicitly.
    Snapshot,
    /// Validate and display a captured tmux workspace.
    Inspect {
        /// Immutable snapshot path. The global latest snapshot is used when omitted.
        snapshot: Option<PathBuf>,
        /// When to use terminal color.
        #[arg(long, value_enum, default_value_t = ColorMode::Auto)]
        color: ColorMode,
        /// Which icon set to use.
        #[arg(long, value_enum, default_value_t = IconMode::Unicode)]
        icons: IconMode,
    },
    /// Validate and print a restore plan, then optionally execute it.
    Restore {
        /// Immutable snapshot path. The global latest snapshot is used when omitted.
        snapshot: Option<PathBuf>,
        /// Execute the printed plan.
        #[arg(long)]
        run: bool,
    },
}

#[derive(Debug)]
pub enum Command {
    Snapshot(SnapshotRequest),
    Inspect {
        snapshot: Option<PathBuf>,
        color: ColorMode,
        icons: IconMode,
    },
    Restore(RestoreRequest),
}

impl Cli {
    pub fn try_parse() -> Result<Self, clap::Error> {
        Self::try_parse_from(std::env::args_os())
    }

    pub fn try_parse_from<I, T>(arguments: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString> + Clone,
    {
        let raw = RawCli::try_parse_from(arguments)?;
        let selector = raw
            .socket_name
            .map(TmuxSelector::SocketName)
            .or_else(|| raw.socket_path.map(TmuxSelector::SocketPath));
        let command = match raw.command {
            RawCommand::Snapshot => Command::Snapshot(SnapshotRequest { selector }),
            RawCommand::Inspect {
                snapshot,
                color,
                icons,
            } => {
                if selector.is_some() {
                    return Err(clap::Error::raw(
                        ErrorKind::ArgumentConflict,
                        "tmux selectors cannot be used with inspect",
                    )
                    .with_cmd(&RawCli::command()));
                }
                Command::Inspect {
                    snapshot,
                    color,
                    icons,
                }
            }
            RawCommand::Restore { snapshot, run } => Command::Restore(RestoreRequest {
                snapshot,
                selector,
                run,
            }),
        };
        Ok(Self { command })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotRequest {
    pub selector: Option<TmuxSelector>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreRequest {
    pub snapshot: Option<PathBuf>,
    pub selector: Option<TmuxSelector>,
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
    pub icons: IconMode,
}

pub trait CliRunner {
    fn snapshot(&mut self, request: SnapshotRequest) -> u8;
    fn inspect(&mut self, request: InspectRequest) -> u8;
    fn restore(&mut self, request: RestoreRequest) -> u8;
}

pub fn dispatch(cli: Cli, runner: &mut impl CliRunner) -> u8 {
    match cli.command {
        Command::Snapshot(request) => runner.snapshot(request),
        Command::Inspect {
            snapshot,
            color,
            icons,
        } => runner.inspect(InspectRequest {
            selection: snapshot.into(),
            color,
            icons,
        }),
        Command::Restore(request) => runner.restore(request),
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

    fn report_inspect_failure(&mut self, error: CliError, color: ColorMode) -> u8 {
        let palette = self.color_support.stderr_palette(color);
        let _ = writeln!(
            self.stderr,
            "{} {}",
            palette.fatal_prefix(),
            safe_text(&error.to_string())
        );
        EXIT_FAILURE
    }
}

impl<W: Write, E: Write> CliRunner for SystemCliRunner<'_, W, E> {
    fn snapshot(&mut self, request: SnapshotRequest) -> u8 {
        match run_snapshot(request, self.stdout, self.stderr) {
            Ok(code) => code,
            Err(error) => self.report_failure(error),
        }
    }

    fn inspect(&mut self, request: InspectRequest) -> u8 {
        let palette = self.color_support.stdout_palette(request.color);
        match run_inspect(&request, self.stdout, palette) {
            Ok(code) => code,
            Err(error) => self.report_inspect_failure(error, request.color),
        }
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

pub fn run_snapshot(
    request: SnapshotRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<u8, CliError> {
    writeln!(stderr, "resolving source tmux server").map_err(io_failure)?;
    let mut source = TmuxAdapter::selected_source(request.selector)
        .map_err(|error| CliError::new(format!("resolve source: {error}")))?;
    let captured_at = current_capture_time()?;
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

pub fn run_inspect(
    request: &InspectRequest,
    stdout: &mut impl Write,
    palette: Palette,
) -> Result<u8, CliError> {
    let loaded = match &request.selection {
        SnapshotSelection::Latest => StateStore::from_environment()
            .map_err(|error| CliError::new(format!("open state store: {error}")))?
            .load_latest(),
        SnapshotSelection::Explicit(path) => StateStore::load_explicit_path(path),
    }
    .map_err(|error| CliError::new(format!("load snapshot: {error}")))?;
    let document = render(&loaded, &request.selection, palette, request.icons);
    stdout.write_all(document.as_bytes()).map_err(io_failure)?;
    stdout.flush().map_err(io_failure)?;
    Ok(EXIT_SUCCESS)
}

pub fn run_restore(
    request: RestoreRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<u8, CliError> {
    run_restore_with_factory(
        request,
        stdout,
        stderr,
        &SystemRestoreEnvironment,
        TmuxRestoreAdapter::new,
    )
}

fn run_restore_with_factory<T: RestoreTargetCapability>(
    request: RestoreRequest,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
    environment: &impl RestoreEnvironment,
    target_factory: impl FnOnce() -> T,
) -> Result<u8, CliError> {
    let plan = match prepare_and_render_restore(&request, environment, stdout) {
        Ok(plan) => plan,
        Err(error) => {
            render_unestablished_failure(stdout)?;
            return Err(error);
        }
    };

    if !request.run {
        return Ok(EXIT_SUCCESS);
    }

    writeln!(stderr, "executing restore topology and pane recovery").map_err(io_failure)?;
    let mut executor = RestoreExecutor::new(target_factory());
    let result = executor.execute(plan);
    render_restore_result(stdout, stderr, &result)?;
    Ok(restore_exit_code(result.status()))
}

fn prepare_and_render_restore(
    request: &RestoreRequest,
    environment: &impl RestoreEnvironment,
    stdout: &mut impl Write,
) -> Result<RestorePlan, CliError> {
    let loaded = match &request.snapshot {
        Some(path) => StateStore::load_explicit_path(path),
        None => StateStore::from_environment()
            .map_err(|error| CliError::new(format!("open state store: {error}")))?
            .load_latest(),
    }
    .map_err(|error| CliError::new(format!("load snapshot: {error}")))?;
    let plan = plan_restore(loaded.snapshot(), request.selector.clone(), environment)
        .map_err(|error| CliError::new(format!("plan restore: {error}")))?;
    write!(stdout, "{plan}").map_err(io_failure)?;
    stdout.flush().map_err(io_failure)?;
    Ok(plan)
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
            CaptureEvent::CodexPromptCaptureSkipped {
                attempt,
                pane,
                failure,
            } => writeln!(
                stderr,
                "warning: capture attempt {attempt} pane {}:{}:{} Codex prompt capture skipped: {}",
                pane.session_name,
                pane.window_index,
                pane.pane_index,
                safe_text(failure.message())
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
        PaneRestoreOutcome::RecoveredAutomaticallyWithPromptPrepared => {
            "recovered automatically; prepared pending input".to_owned()
        }
        PaneRestoreOutcome::RecoveredAutomaticallyWithPromptNeedsAttention(failure) => {
            format!(
                "recovered automatically; pending input needs attention ({})",
                codex_prompt_paste_failure_label(failure)
            )
        }
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

fn codex_prompt_paste_failure_label(failure: &CodexPromptPasteFailure) -> String {
    match failure {
        CodexPromptPasteFailure::SessionMismatch => "Codex session changed".to_owned(),
        CodexPromptPasteFailure::PaneMissing => "target pane is missing".to_owned(),
        CodexPromptPasteFailure::InputDisabled => "target pane input is disabled".to_owned(),
        CodexPromptPasteFailure::PasteFailed => "prompt paste failed".to_owned(),
        CodexPromptPasteFailure::CleanupFailed => "prompt buffer cleanup failed".to_owned(),
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
        let fragment = if character.is_control() || is_unicode_display_control(character) {
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
    use std::cell::Cell;
    use std::ffi::OsString;
    use std::io;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use tmux_rescue::{
        AutomaticPaneObservation, AutomaticRecoveryExpectation, CaptureConsistency,
        CapturedCodexPromptArea, CodexPromptPasteFailure, CodexPromptPasteResult, CodexSessionId,
        GuardedPaneOperation, GuardedPaneResult, LatestDisposition, LosslessOsString,
        OwnedRestoreTarget, PlanningExecutable, RecordedAbsolutePath, RecoveryRestoreTarget,
        RestoreDestination, RestoreEnvironment, RestoreEnvironmentFailure, RestorePlan,
        RestoreTargetCapability, RollbackOutcome, SnapshotPublication, StorageError,
        TargetClaimFailure, TargetDisposition, TargetShell, TmuxSelector,
    };

    use super::*;

    #[derive(Default)]
    struct RecordingRunner {
        snapshot_requests: Vec<SnapshotRequest>,
        inspect_requests: Vec<InspectRequest>,
        restore_requests: Vec<RestoreRequest>,
        snapshot_code: u8,
        inspect_code: u8,
        restore_code: u8,
    }

    impl CliRunner for RecordingRunner {
        fn snapshot(&mut self, request: SnapshotRequest) -> u8 {
            self.snapshot_requests.push(request);
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

    fn restore_command(cli: Cli) -> RestoreRequest {
        match cli.command {
            Command::Restore(request) => request,
            Command::Snapshot(_) | Command::Inspect { .. } => panic!("expected restore command"),
        }
    }

    fn inspect_command(cli: Cli) -> (Option<PathBuf>, ColorMode, IconMode) {
        match cli.command {
            Command::Inspect {
                snapshot,
                color,
                icons,
            } => (snapshot, color, icons),
            Command::Snapshot(_) | Command::Restore(_) => panic!("expected inspect command"),
        }
    }

    fn published(latest: LatestDisposition) -> SnapshotPublication {
        SnapshotPublication::Published {
            snapshot_path: PathBuf::from("/state/snapshots/snapshot.json"),
            consistency: CaptureConsistency::Stable,
            latest,
        }
    }

    struct TestRestoreEnvironment;

    impl RestoreEnvironment for TestRestoreEnvironment {
        fn target_shell(&self) -> Result<TargetShell, RestoreEnvironmentFailure> {
            TargetShell::try_from_bytes(b"/bin/sh".to_vec())
                .map_err(|error| RestoreEnvironmentFailure::new(error.to_string()))
        }

        fn home_directory(&self) -> Result<RecordedAbsolutePath, RestoreEnvironmentFailure> {
            RecordedAbsolutePath::try_from_bytes(b"/tmp".to_vec())
                .map_err(|error| RestoreEnvironmentFailure::new(error.to_string()))
        }

        fn directory_exists(&self, _directory: &RecordedAbsolutePath) -> bool {
            true
        }

        fn resolve_executable(
            &self,
            _directory: &RecordedAbsolutePath,
            _command_word: &LosslessOsString,
        ) -> Option<PlanningExecutable> {
            None
        }
    }

    struct ClaimFailureTarget;

    impl RestoreTargetCapability for ClaimFailureTarget {
        fn claim(
            &mut self,
            _destination: &RestoreDestination,
            _shell: &TargetShell,
        ) -> Result<Box<dyn OwnedRestoreTarget>, TargetClaimFailure> {
            Err(TargetClaimFailure::new("stop after construction"))
        }
    }

    struct PromptRestoreEnvironment;

    impl RestoreEnvironment for PromptRestoreEnvironment {
        fn target_shell(&self) -> Result<TargetShell, RestoreEnvironmentFailure> {
            TargetShell::try_from_bytes(b"/bin/sh".to_vec())
                .map_err(|error| RestoreEnvironmentFailure::new(error.to_string()))
        }

        fn home_directory(&self) -> Result<RecordedAbsolutePath, RestoreEnvironmentFailure> {
            RecordedAbsolutePath::try_from_bytes(b"/tmp".to_vec())
                .map_err(|error| RestoreEnvironmentFailure::new(error.to_string()))
        }

        fn directory_exists(&self, _directory: &RecordedAbsolutePath) -> bool {
            true
        }

        fn resolve_executable(
            &self,
            _directory: &RecordedAbsolutePath,
            _command_word: &LosslessOsString,
        ) -> Option<PlanningExecutable> {
            PlanningExecutable::try_from_bytes(b"/bin/sh".to_vec()).ok()
        }
    }

    struct PromptOutcomeTarget {
        paste_result: CodexPromptPasteResult,
    }

    impl RestoreTargetCapability for PromptOutcomeTarget {
        fn claim(
            &mut self,
            _destination: &RestoreDestination,
            _shell: &TargetShell,
        ) -> Result<Box<dyn OwnedRestoreTarget>, TargetClaimFailure> {
            Ok(Box::new(PromptOutcomeOwnedTarget {
                paste_result: Some(self.paste_result.clone()),
            }))
        }
    }

    struct PromptOutcomeOwnedTarget {
        paste_result: Option<CodexPromptPasteResult>,
    }

    impl OwnedRestoreTarget for PromptOutcomeOwnedTarget {
        fn create_topology(
            &mut self,
            _plan: &RestorePlan,
        ) -> Result<(), tmux_rescue::TopologyFailure> {
            Ok(())
        }

        fn rollback(self: Box<Self>) -> RollbackOutcome {
            RollbackOutcome::Removed
        }

        fn begin_recovery(self: Box<Self>) -> Box<dyn RecoveryRestoreTarget> {
            Box::new(PromptOutcomeRecoveryTarget {
                paste_result: self.paste_result,
            })
        }
    }

    struct PromptOutcomeRecoveryTarget {
        paste_result: Option<CodexPromptPasteResult>,
    }

    impl RecoveryRestoreTarget for PromptOutcomeRecoveryTarget {
        fn guarded_pane_operation(
            &mut self,
            _pane: &SourcePaneCoordinate,
            _shell: &TargetShell,
            _operation: GuardedPaneOperation<'_>,
        ) -> GuardedPaneResult {
            Ok(())
        }

        fn observe_automatic(
            &mut self,
            _pane: &SourcePaneCoordinate,
            _expected: &AutomaticRecoveryExpectation,
        ) -> AutomaticPaneObservation {
            AutomaticPaneObservation::Recovered
        }

        fn paste_codex_prompt_area(
            &mut self,
            _pane: &SourcePaneCoordinate,
            _expected: &CodexSessionId,
            _input: &CapturedCodexPromptArea,
        ) -> CodexPromptPasteResult {
            self.paste_result
                .take()
                .expect("prompt preparation is attempted exactly once")
        }

        fn observe_disposition(&mut self) -> TargetDisposition {
            TargetDisposition::Retained
        }
    }

    struct FlushTrackingWriter {
        bytes: Vec<u8>,
        flushed: Rc<Cell<bool>>,
    }

    impl io::Write for FlushTrackingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushed.set(true);
            Ok(())
        }
    }

    fn restore_fixture(path: &Path) {
        std::fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({
                "captured_at": "2026-07-24T00:00:00Z",
                "source": {"encoding": "utf8", "value": "/recorded/source.sock"},
                "consistency": {"kind": "stable"},
                "sessions": [{
                    "name": "work",
                    "working_directory": {"encoding": "utf8", "value": "/tmp"},
                    "windows": [{
                        "source_index": 0,
                        "name": "work",
                        "panes": [{
                            "source_index": 0,
                            "working_directory": {"encoding": "utf8", "value": "/tmp"},
                            "recovery": {"kind": "idle"}
                        }]
                    }]
                }]
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn prompt_restore_fixture(path: &Path, prompt: &str) {
        std::fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({
                "captured_at": "2026-07-24T00:00:00Z",
                "source": {"encoding": "utf8", "value": "/recorded/source.sock"},
                "consistency": {"kind": "stable"},
                "sessions": [{
                    "name": "work",
                    "working_directory": {"encoding": "utf8", "value": "/tmp"},
                    "windows": [{
                        "source_index": 4,
                        "name": "work",
                        "panes": [{
                            "source_index": 0,
                            "working_directory": {"encoding": "utf8", "value": "/tmp"},
                            "recovery": {
                                "kind": "automatic",
                                "recovery": {
                                    "kind": "codex",
                                    "session_id": "1d6381bf-01c5-4c4a-b725-8e376e5ad295",
                                    "prompt_area": {"text": prompt}
                                }
                            }
                        }]
                    }]
                }]
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn parses_the_exact_command_surface() {
        assert!(matches!(
            parse(&["tmux-rescue", "snapshot"]).command,
            Command::Snapshot(SnapshotRequest { selector: None })
        ));
        assert!(Cli::try_parse_from(["tmux-rescue", "snapshot", "unexpected"]).is_err());
        assert!(Cli::try_parse_from(["tmux-rescue", "snapshot", "--run"]).is_err());
        assert!(Cli::try_parse_from(["tmux-rescue", "restore", "--json"]).is_err());

        assert_eq!(
            inspect_command(parse(&["tmux-rescue", "inspect"])),
            (None, ColorMode::Auto, IconMode::Unicode),
        );
        assert_eq!(
            inspect_command(parse(&[
                "tmux-rescue",
                "inspect",
                "snapshot.json",
                "--icons",
                "nerd",
            ])),
            (
                Some(PathBuf::from("snapshot.json")),
                ColorMode::Auto,
                IconMode::Nerd,
            ),
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
                IconMode::Unicode,
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
                IconMode::Unicode,
            )
        );
        assert!(Cli::try_parse_from(["tmux-rescue", "inspect", "--color", "sometimes"]).is_err());
        assert!(Cli::try_parse_from(["tmux-rescue", "inspect", "--icons", "automatic"]).is_err());
        assert!(Cli::try_parse_from(["tmux-rescue", "inspect", "one.json", "two.json"]).is_err());

        let request = restore_command(parse(&[
            "tmux-rescue",
            "-L",
            "abc",
            "restore",
            "/state/snapshot.json",
            "--run",
        ]));
        assert_eq!(
            request.snapshot.as_deref(),
            Some(Path::new("/state/snapshot.json"))
        );
        assert_eq!(
            request.selector,
            Some(TmuxSelector::SocketName(OsString::from("abc")))
        );
        assert!(request.run);

        assert_eq!(
            restore_command(parse(&["tmux-rescue", "restore"])),
            RestoreRequest {
                snapshot: None,
                selector: None,
                run: false
            }
        );
    }

    #[test]
    fn snapshot_help_describes_ambient_and_explicit_selection() {
        let mut command = RawCli::command();
        let help = command.render_help().to_string();

        assert!(help.contains(
            "Capture the ambient tmux server; root -L/-S before snapshot select one explicitly"
        ));
    }

    #[test]
    fn parses_root_selectors_only_before_snapshot_and_restore() {
        for (flag, value, expected) in [
            (
                "-L",
                OsString::from("abc"),
                TmuxSelector::SocketName(OsString::from("abc")),
            ),
            (
                "-S",
                OsString::from("./rescue.sock"),
                TmuxSelector::SocketPath(OsString::from("./rescue.sock")),
            ),
        ] {
            let snapshot = Cli::try_parse_from([
                OsString::from("tmux-rescue"),
                OsString::from(flag),
                value.clone(),
                OsString::from("snapshot"),
            ])
            .unwrap();
            assert!(
                matches!(snapshot.command, Command::Snapshot(SnapshotRequest { selector: Some(selector) }) if selector == expected)
            );

            let restore = Cli::try_parse_from([
                OsString::from("tmux-rescue"),
                OsString::from(flag),
                value,
                OsString::from("restore"),
            ])
            .unwrap();
            assert_eq!(restore_command(restore).selector, Some(expected));
        }

        for args in [
            vec!["tmux-rescue", "-L", "one", "-S", "two", "snapshot"],
            vec!["tmux-rescue", "-S", "two", "-L", "one", "restore"],
            vec!["tmux-rescue", "-L", "one", "-L", "two", "snapshot"],
            vec!["tmux-rescue", "-S", "one", "-S", "two", "restore"],
            vec!["tmux-rescue", "snapshot", "-L", "one"],
            vec!["tmux-rescue", "snapshot", "-S", "one"],
            vec!["tmux-rescue", "restore", "-L", "one"],
            vec!["tmux-rescue", "restore", "-S", "one"],
            vec!["tmux-rescue", "-L", "one", "inspect"],
            vec!["tmux-rescue", "-S", "one", "inspect"],
            vec!["tmux-rescue", "-L"],
            vec!["tmux-rescue", "-S"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn parser_preserves_non_utf8_selector_bytes() {
        let bytes = vec![b'.', b'/', 0xff];
        let cli = Cli::try_parse_from([
            OsString::from("tmux-rescue"),
            OsString::from("-S"),
            OsString::from_vec(bytes.clone()),
            OsString::from("restore"),
        ])
        .unwrap();

        let selector = restore_command(cli).selector.unwrap();
        let TmuxSelector::SocketPath(path) = selector else {
            panic!("expected socket path")
        };
        assert_eq!(path.as_os_str().as_bytes(), bytes);
    }

    #[test]
    fn plan_only_restore_never_constructs_the_target_adapter() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = directory.path().join("snapshot.json");
        restore_fixture(&snapshot);
        let constructed = Rc::new(Cell::new(false));
        let observed = Rc::clone(&constructed);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let result = run_restore_with_factory(
            RestoreRequest {
                snapshot: Some(snapshot),
                selector: None,
                run: false,
            },
            &mut stdout,
            &mut stderr,
            &TestRestoreEnvironment,
            move || {
                observed.set(true);
                ClaimFailureTarget
            },
        );

        assert_eq!(result.unwrap(), EXIT_SUCCESS);
        assert!(!constructed.get());
        assert!(stdout.starts_with(b"target: -S /recorded/source.sock\n"));
    }

    #[test]
    fn run_flushes_the_plan_before_constructing_the_target_adapter() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = directory.path().join("snapshot.json");
        restore_fixture(&snapshot);
        let flushed = Rc::new(Cell::new(false));
        let observed = Rc::clone(&flushed);
        let mut stdout = FlushTrackingWriter {
            bytes: Vec::new(),
            flushed: Rc::clone(&flushed),
        };
        let mut stderr = Vec::new();

        let result = run_restore_with_factory(
            RestoreRequest {
                snapshot: Some(snapshot),
                selector: None,
                run: true,
            },
            &mut stdout,
            &mut stderr,
            &TestRestoreEnvironment,
            move || {
                assert!(observed.get());
                ClaimFailureTarget
            },
        );

        assert_eq!(result.unwrap(), EXIT_FAILURE);
        assert!(
            stdout
                .bytes
                .starts_with(b"target: -S /recorded/source.sock\n")
        );
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
        assert_eq!(
            runner.snapshot_requests,
            [SnapshotRequest { selector: None }]
        );

        runner.inspect_code = EXIT_SUCCESS;
        assert_eq!(
            dispatch(
                parse(&[
                    "tmux-rescue",
                    "inspect",
                    "relative/snapshot.json",
                    "--color",
                    "never",
                    "--icons",
                    "nerd",
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
                icons: IconMode::Nerd,
            }]
        );

        assert_eq!(
            dispatch(
                parse(&[
                    "tmux-rescue",
                    "-S",
                    "./target.sock",
                    "restore",
                    "/state/snapshot.json",
                    "--run",
                ]),
                &mut runner,
            ),
            EXIT_PARTIAL
        );
        assert_eq!(runner.restore_requests.len(), 1);
        assert!(runner.restore_requests[0].run);
        assert_eq!(
            runner.restore_requests[0].selector,
            Some(TmuxSelector::SocketPath(OsString::from("./target.sock")))
        );
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
    fn inspect_output_failure_is_fatal_and_reports_on_stderr() {
        struct FailingWriter;

        impl io::Write for FailingWriter {
            fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed stdout"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let snapshot = directory.path().join("snapshot.json");
        std::fs::write(
            &snapshot,
            serde_json::to_vec(&serde_json::json!({
                "captured_at": "2026-07-24T00:00:00Z",
                "source": {"encoding": "utf8", "value": "/tmp/source.sock"},
                "consistency": {"kind": "stable"},
                "sessions": [{
                    "name": "work",
                    "working_directory": {"encoding": "utf8", "value": "/workspace"},
                    "windows": [{
                        "source_index": 0,
                        "name": "shell",
                        "panes": [{
                            "source_index": 0,
                            "working_directory": {
                                "encoding": "utf8",
                                "value": "/workspace"
                            },
                            "recovery": {"kind": "idle"}
                        }]
                    }]
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let request = InspectRequest {
            selection: SnapshotSelection::Explicit(snapshot),
            color: ColorMode::Always,
            icons: IconMode::Unicode,
        };
        let mut stdout = FailingWriter;
        let mut stderr = Vec::new();
        let mut runner = SystemCliRunner::with_color_support(
            &mut stdout,
            &mut stderr,
            TerminalColorSupport::new(false, false),
        );

        assert_eq!(runner.inspect(request), EXIT_FAILURE);
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "\x1b[31merror:\x1b[0m write CLI output: closed stdout\n"
        );
    }

    #[test]
    fn inspect_failure_escapes_unicode_display_controls_on_stderr() {
        let directory = tempfile::tempdir().unwrap();
        let snapshot = directory
            .path()
            .join("before\u{202e}middle\u{2028}after\u{2029}.json");
        let request = InspectRequest {
            selection: SnapshotSelection::Explicit(snapshot),
            color: ColorMode::Never,
            icons: IconMode::Unicode,
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut runner = SystemCliRunner::with_color_support(
            &mut stdout,
            &mut stderr,
            TerminalColorSupport::new(false, false),
        );

        assert_eq!(runner.inspect(request), EXIT_FAILURE);
        assert!(stdout.is_empty());
        let stderr = String::from_utf8(stderr).unwrap();
        for character in ['\u{2028}', '\u{2029}', '\u{202e}'] {
            assert!(
                !stderr.contains(character),
                "raw {character:?} in fatal stderr: {stderr:?}"
            );
        }
        for escaped in ["\\u{2028}", "\\u{2029}", "\\u{202e}"] {
            assert!(
                stderr.contains(escaped),
                "missing {escaped:?} in fatal stderr: {stderr:?}"
            );
        }
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

    #[test]
    fn prepared_codex_prompt_outcome_is_count_free_and_prompt_free() {
        let label =
            pane_outcome_label(&PaneRestoreOutcome::RecoveredAutomaticallyWithPromptPrepared);

        assert_eq!(label, "recovered automatically; prepared pending input");
        assert!(!label.contains("row"));
        assert!(!label.contains("byte"));
        assert!(!label.contains("secret prompt"));
    }

    #[test]
    fn codex_prompt_attention_labels_are_safe_and_specific() {
        for (failure, expected) in [
            (
                CodexPromptPasteFailure::SessionMismatch,
                "recovered automatically; pending input needs attention (Codex session changed)",
            ),
            (
                CodexPromptPasteFailure::PaneMissing,
                "recovered automatically; pending input needs attention (target pane is missing)",
            ),
            (
                CodexPromptPasteFailure::InputDisabled,
                "recovered automatically; pending input needs attention (target pane input is disabled)",
            ),
            (
                CodexPromptPasteFailure::PasteFailed,
                "recovered automatically; pending input needs attention (prompt paste failed)",
            ),
            (
                CodexPromptPasteFailure::CleanupFailed,
                "recovered automatically; pending input needs attention (prompt buffer cleanup failed)",
            ),
        ] {
            assert_eq!(
                pane_outcome_label(
                    &PaneRestoreOutcome::RecoveredAutomaticallyWithPromptNeedsAttention(failure)
                ),
                expected
            );
        }
    }

    #[test]
    fn codex_prompt_attention_label_cannot_carry_prompt_text() {
        const SENSITIVE_PROMPT: &str = "release the unreleased signing key";
        for failure in [
            CodexPromptPasteFailure::SessionMismatch,
            CodexPromptPasteFailure::PaneMissing,
            CodexPromptPasteFailure::InputDisabled,
            CodexPromptPasteFailure::PasteFailed,
            CodexPromptPasteFailure::CleanupFailed,
        ] {
            let label = pane_outcome_label(
                &PaneRestoreOutcome::RecoveredAutomaticallyWithPromptNeedsAttention(failure),
            );
            assert!(!label.contains(SENSITIVE_PROMPT));
        }
    }

    #[test]
    fn codex_prompt_outcomes_render_exact_pane_lines_and_partial_warning() {
        const SENSITIVE_PROMPT: &str = "secret prompt that must not reach CLI output";
        let directory = tempfile::tempdir().unwrap();
        let snapshot = directory.path().join("snapshot.json");
        prompt_restore_fixture(&snapshot, SENSITIVE_PROMPT);
        let cases = [
            (
                Ok(()),
                EXIT_SUCCESS,
                concat!(
                    "restore: complete\n",
                    "target state: retained\n",
                    "pane work:4:0: recovered automatically; prepared pending input\n",
                ),
                "executing restore topology and pane recovery\n",
            ),
            (
                Err(CodexPromptPasteFailure::SessionMismatch),
                EXIT_PARTIAL,
                concat!(
                    "restore: partial\n",
                    "target state: retained\n",
                    "pane work:4:0: recovered automatically; pending input needs attention ",
                    "(Codex session changed)\n",
                ),
                concat!(
                    "executing restore topology and pane recovery\n",
                    "warning: restore completed partially\n",
                ),
            ),
        ];

        for (paste_result, expected_code, expected_suffix, expected_stderr) in cases {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();

            let code = run_restore_with_factory(
                RestoreRequest {
                    snapshot: Some(snapshot.clone()),
                    selector: None,
                    run: true,
                },
                &mut stdout,
                &mut stderr,
                &PromptRestoreEnvironment,
                move || PromptOutcomeTarget { paste_result },
            )
            .unwrap();

            let stdout = String::from_utf8(stdout).unwrap();
            let stderr = String::from_utf8(stderr).unwrap();
            assert_eq!(code, expected_code);
            assert!(
                stdout.ends_with(expected_suffix),
                "missing exact rendered restore result in {stdout:?}"
            );
            assert_eq!(stderr, expected_stderr);
            for output in [&stdout, &stderr] {
                assert!(!output.contains(SENSITIVE_PROMPT));
                assert!(!output.contains("prompt_area"));
                assert!(!output.contains("{\"text\":"));
            }
        }
    }

    #[test]
    fn prompt_capture_failures_are_coordinate_scoped_and_prompt_free() {
        let mut stderr = Vec::new();
        let event = CaptureEvent::CodexPromptCaptureSkipped {
            attempt: 1,
            pane: SourcePaneCoordinate {
                session_name: "work".to_owned(),
                window_index: 0,
                pane_index: 0,
            },
            failure: tmux_rescue::CodexPromptCaptureFailure::visible_pane_read_failed(),
        };

        write_capture_events(&mut stderr, &[event]).unwrap();

        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "warning: capture attempt 1 pane work:0:0 Codex prompt capture skipped: visible tmux pane could not be read\n"
        );
    }
}
