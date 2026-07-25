use std::ffi::{CStr, CString, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{
    AutomaticRecovery, AutomaticRecoveryExpectation, CaptureFailure, CapturedCodexPromptArea,
    CodexSessionId, LosslessOsString, PaneRecovery, RecordedAbsolutePath, RecoveryCommand,
    SourcePaneCoordinate, TmuxSelector, ValidatedSnapshot, derive_automatic_command,
};

pub const MAX_RENDERED_SHELL_INPUT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreDestination {
    selector: TmuxSelector,
}

impl RestoreDestination {
    pub(crate) fn from_selector(selector: TmuxSelector) -> Self {
        Self { selector }
    }

    pub fn selector(&self) -> &TmuxSelector {
        &self.selector
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShellDialect {
    PosixLike,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetShell {
    executable: RecordedAbsolutePath,
    executable_identity: RecordedAbsolutePath,
    file_identity: ExecutableFileIdentity,
    dialect: ShellDialect,
}

impl TargetShell {
    pub fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, TargetShellError> {
        let catalog = ShellRuntimeCatalog::from_system();
        Self::try_from_bytes_with_catalog(bytes, &catalog)
    }

    fn try_from_bytes_with_catalog(
        bytes: Vec<u8>,
        catalog: &ShellRuntimeCatalog,
    ) -> Result<Self, TargetShellError> {
        let executable = RecordedAbsolutePath::try_from_bytes(bytes)
            .map_err(|error| TargetShellError::InvalidPath(error.to_string()))?;
        let dialect = shell_dialect(Path::new(executable.as_os_str()))
            .ok_or(TargetShellError::UnsupportedShell)?;
        if !is_executable_file(Path::new(executable.as_os_str())) {
            return Err(TargetShellError::NotExecutable);
        }
        let executable_identity = fs::canonicalize(executable.as_os_str())
            .map_err(|_| TargetShellError::NotExecutable)?;
        let executable_identity =
            RecordedAbsolutePath::try_from_bytes(executable_identity.into_os_string().into_vec())
                .map_err(|error| TargetShellError::InvalidPath(error.to_string()))?;
        if shell_dialect(Path::new(executable_identity.as_os_str())) != Some(dialect) {
            return Err(TargetShellError::UnsupportedShell);
        }
        let runtime_path = Path::new(executable_identity.as_os_str());
        let runtime = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(runtime_path)
            .map_err(|_| TargetShellError::NotExecutable)?;
        let file_identity =
            ExecutableFileIdentity::read_file(&runtime).ok_or(TargetShellError::NotExecutable)?;
        if !is_native_linux_executable(&runtime) {
            return Err(TargetShellError::MalformedNativeExecutable);
        }
        if !catalog.authorizes(runtime_path) {
            return Err(TargetShellError::RuntimeNotAuthorized);
        }
        Ok(Self {
            executable,
            executable_identity,
            file_identity,
            dialect,
        })
    }

    pub fn executable(&self) -> &RecordedAbsolutePath {
        &self.executable
    }

    pub fn executable_identity(&self) -> &RecordedAbsolutePath {
        &self.executable_identity
    }

    pub fn interactive_argv(&self) -> Vec<OsString> {
        vec![self.executable.as_os_str().to_owned(), OsString::from("-i")]
    }

    pub fn matches_current_file(&self) -> bool {
        fs::canonicalize(self.executable.as_os_str())
            .map(|path| path.as_os_str() == self.executable_identity.as_os_str())
            .unwrap_or(false)
            && self
                .file_identity
                .matches(Path::new(self.executable_identity.as_os_str()))
            && OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(self.executable_identity.as_os_str())
                .map(|runtime| is_native_linux_executable(&runtime))
                .unwrap_or(false)
            && is_executable_file(Path::new(self.executable.as_os_str()))
    }
}

fn shell_dialect(path: &Path) -> Option<ShellDialect> {
    match path.file_name().map(OsStrExt::as_bytes)? {
        b"sh" | b"bash" | b"dash" | b"zsh" | b"ksh" | b"mksh" | b"ash" => {
            Some(ShellDialect::PosixLike)
        }
        _ => None,
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum TargetShellError {
    #[error("target shell path is invalid: {0}")]
    InvalidPath(String),
    #[error("target shell is not supported by the v1 renderer")]
    UnsupportedShell,
    #[error("target shell is not an executable file for the effective user")]
    NotExecutable,
    #[error(
        "target shell is not a structurally complete architecture-compatible Linux ELF executable"
    )]
    MalformedNativeExecutable,
    #[error("target shell runtime is not registered or conventional")]
    RuntimeNotAuthorized,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct RestoreEnvironmentFailure {
    message: String,
}

impl RestoreEnvironmentFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: terminal_safe_text(message.into().as_bytes()),
        }
    }
}

pub trait RestoreEnvironment {
    fn target_shell(&self) -> Result<TargetShell, RestoreEnvironmentFailure>;
    fn home_directory(&self) -> Result<RecordedAbsolutePath, RestoreEnvironmentFailure>;
    fn directory_exists(&self, directory: &RecordedAbsolutePath) -> bool;
    fn resolve_executable(
        &self,
        directory: &RecordedAbsolutePath,
        command_word: &LosslessOsString,
    ) -> Option<PlanningExecutable>;
}

#[derive(Clone, Debug, Default)]
pub struct SystemRestoreEnvironment;

impl RestoreEnvironment for SystemRestoreEnvironment {
    fn target_shell(&self) -> Result<TargetShell, RestoreEnvironmentFailure> {
        let passwd = effective_user_record();
        let catalog = ShellRuntimeCatalog::from_system();
        let mut candidates = Vec::new();
        if let Some(shell) = std::env::var_os("SHELL") {
            candidates.push(PathBuf::from(shell));
        }
        if let Ok((_, shell)) = &passwd {
            candidates.push(PathBuf::from(shell));
        }
        candidates.push(PathBuf::from("/bin/sh"));

        for candidate in candidates {
            if !candidate.is_absolute() {
                continue;
            }
            if let Ok(shell) = TargetShell::try_from_bytes_with_catalog(
                candidate.into_os_string().into_vec(),
                &catalog,
            ) {
                return Ok(shell);
            }
        }
        Err(RestoreEnvironmentFailure::new(
            "no suitable absolute interactive shell was found",
        ))
    }

    fn home_directory(&self) -> Result<RecordedAbsolutePath, RestoreEnvironmentFailure> {
        let home = effective_user_record().map(|(home, _)| home)?;
        RecordedAbsolutePath::try_from_bytes(home.into_vec())
            .map_err(|error| RestoreEnvironmentFailure::new(error.to_string()))
    }

    fn directory_exists(&self, directory: &RecordedAbsolutePath) -> bool {
        fs::metadata(directory.as_os_str())
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
    }

    fn resolve_executable(
        &self,
        directory: &RecordedAbsolutePath,
        command_word: &LosslessOsString,
    ) -> Option<PlanningExecutable> {
        let command = Path::new(command_word.as_os_str());
        if command.components().count() > 1 {
            let candidate = if command.is_absolute() {
                command.to_owned()
            } else {
                Path::new(directory.as_os_str()).join(command)
            };
            return PlanningExecutable::try_from_path(candidate).ok();
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|path_entry| {
                let base = if path_entry.is_absolute() {
                    path_entry
                } else {
                    Path::new(directory.as_os_str()).join(path_entry)
                };
                base.join(command_word.as_os_str())
            })
            .find_map(|candidate| PlanningExecutable::try_from_path(candidate).ok())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedDirectoryOrigin {
    Recorded,
    HomeFallback,
    SessionFallback,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDirectory {
    path: RecordedAbsolutePath,
    origin: ResolvedDirectoryOrigin,
}

impl ResolvedDirectory {
    pub fn path(&self) -> &RecordedAbsolutePath {
        &self.path
    }

    pub fn origin(&self) -> ResolvedDirectoryOrigin {
        self.origin
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExistingRecordedDirectory(RecordedAbsolutePath);

impl ExistingRecordedDirectory {
    pub fn path(&self) -> &RecordedAbsolutePath {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlanningExecutable {
    path: RecordedAbsolutePath,
    file_identity: ExecutableFileIdentity,
}

impl PlanningExecutable {
    pub fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, PlanningExecutableError> {
        let path = RecordedAbsolutePath::try_from_bytes(bytes)
            .map_err(|error| PlanningExecutableError::InvalidPath(error.to_string()))?;
        if !is_executable_file(Path::new(path.as_os_str())) {
            return Err(PlanningExecutableError::NotExecutable);
        }
        let file_identity = ExecutableFileIdentity::read(Path::new(path.as_os_str()))
            .ok_or(PlanningExecutableError::NotExecutable)?;
        Ok(Self {
            path,
            file_identity,
        })
    }

    fn try_from_path(path: PathBuf) -> Result<Self, PlanningExecutableError> {
        Self::try_from_bytes(path.into_os_string().into_vec())
    }

    pub fn path(&self) -> &RecordedAbsolutePath {
        &self.path
    }

    pub fn matches_current_file(&self) -> bool {
        self.file_identity.matches(Path::new(self.path.as_os_str()))
            && is_executable_file(Path::new(self.path.as_os_str()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExecutableFileIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl ExecutableFileIdentity {
    fn read(path: &Path) -> Option<Self> {
        Self::from_metadata(fs::metadata(path).ok()?)
    }

    fn read_file(file: &File) -> Option<Self> {
        Self::from_metadata(file.metadata().ok()?)
    }

    fn from_metadata(metadata: fs::Metadata) -> Option<Self> {
        metadata.is_file().then_some(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }

    fn matches(&self, path: &Path) -> bool {
        Self::read(path).as_ref() == Some(self)
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum PlanningExecutableError {
    #[error("resolved executable path is invalid: {0}")]
    InvalidPath(String),
    #[error("resolved executable is not executable by the effective user")]
    NotExecutable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedShellInput {
    bytes: Vec<u8>,
    shell: TargetShell,
}

impl RenderedShellInput {
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn shell(&self) -> &TargetShell {
        &self.shell
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchableShellInput {
    rendered: RenderedShellInput,
    executable: PlanningExecutable,
}

impl LaunchableShellInput {
    pub fn rendered(&self) -> &RenderedShellInput {
        &self.rendered
    }

    pub fn executable(&self) -> &PlanningExecutable {
        &self.executable
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AutomaticFallbackReason {
    RecordedDirectoryUnavailable,
    ExecutableUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedAutomaticLaunch {
    directory: ExistingRecordedDirectory,
    input: LaunchableShellInput,
    expected: AutomaticRecoveryExpectation,
    codex_prompt: Option<PlannedCodexPromptPaste>,
}

impl PlannedAutomaticLaunch {
    fn new(
        directory: ExistingRecordedDirectory,
        input: LaunchableShellInput,
        automatic: &AutomaticRecovery,
    ) -> Self {
        let (expected, codex_prompt) = match automatic {
            AutomaticRecovery::Codex {
                session_id,
                prompt_area,
            } => (
                AutomaticRecoveryExpectation::Codex(session_id.clone()),
                prompt_area
                    .clone()
                    .map(|input| PlannedCodexPromptPaste { input }),
            ),
            AutomaticRecovery::ClaudeCode { session_id } => (
                AutomaticRecoveryExpectation::ClaudeCode(session_id.clone()),
                None,
            ),
            AutomaticRecovery::MdBookServe { command } => (
                AutomaticRecoveryExpectation::MdBookServe(command.clone()),
                None,
            ),
            AutomaticRecovery::BookshelfServe { command } => (
                AutomaticRecoveryExpectation::BookshelfServe(command.clone()),
                None,
            ),
        };
        Self {
            directory,
            input,
            expected,
            codex_prompt,
        }
    }

    pub fn directory(&self) -> &ExistingRecordedDirectory {
        &self.directory
    }

    pub fn input(&self) -> &LaunchableShellInput {
        &self.input
    }

    pub fn expectation(&self) -> &AutomaticRecoveryExpectation {
        &self.expected
    }

    pub(crate) fn codex_prompt(&self) -> Option<(&CodexSessionId, &CapturedCodexPromptArea)> {
        match (&self.expected, &self.codex_prompt) {
            (
                AutomaticRecoveryExpectation::Codex(session_id),
                Some(PlannedCodexPromptPaste { input }),
            ) => Some((session_id, input)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedCodexPromptPaste {
    input: CapturedCodexPromptArea,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlannedPaneAction {
    LeaveIdle {
        directory: ResolvedDirectory,
    },
    LaunchAutomatic(PlannedAutomaticLaunch),
    PasteManualHint {
        directory: ResolvedDirectory,
        input: RenderedShellInput,
    },
    PasteAutomaticFallback {
        directory: ResolvedDirectory,
        input: RenderedShellInput,
        reason: AutomaticFallbackReason,
    },
    NoInput {
        directory: ResolvedDirectory,
        reason: CaptureFailure,
    },
}

impl PlannedPaneAction {
    pub fn directory(&self) -> &RecordedAbsolutePath {
        match self {
            Self::LeaveIdle { directory }
            | Self::PasteManualHint { directory, .. }
            | Self::PasteAutomaticFallback { directory, .. }
            | Self::NoInput { directory, .. } => directory.path(),
            Self::LaunchAutomatic(launch) => launch.directory().path(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedPane {
    coordinate: SourcePaneCoordinate,
    action: PlannedPaneAction,
}

impl PlannedPane {
    pub fn coordinate(&self) -> &SourcePaneCoordinate {
        &self.coordinate
    }

    pub fn action(&self) -> &PlannedPaneAction {
        &self.action
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedWindow {
    source_index: u32,
    name: String,
    pane_coordinates: Vec<SourcePaneCoordinate>,
}

impl PlannedWindow {
    pub fn source_index(&self) -> u32 {
        self.source_index
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn pane_coordinates(&self) -> &[SourcePaneCoordinate] {
        &self.pane_coordinates
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedSession {
    name: String,
    directory: ResolvedDirectory,
    windows: Vec<PlannedWindow>,
}

impl PlannedSession {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn directory(&self) -> &ResolvedDirectory {
        &self.directory
    }

    pub fn windows(&self) -> &[PlannedWindow] {
        &self.windows
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanDegradation {
    SessionDirectoryFallback {
        session_name: String,
    },
    PaneDirectoryFallback {
        pane: SourcePaneCoordinate,
    },
    AutomaticRecoveryFallback {
        pane: SourcePaneCoordinate,
        reason: AutomaticFallbackReason,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestorePlan {
    destination: RestoreDestination,
    target_shell: TargetShell,
    sessions: Vec<PlannedSession>,
    panes: Vec<PlannedPane>,
    degradations: Vec<PlanDegradation>,
}

impl RestorePlan {
    pub fn destination(&self) -> &RestoreDestination {
        &self.destination
    }

    pub fn target_shell(&self) -> &TargetShell {
        &self.target_shell
    }

    pub fn sessions(&self) -> &[PlannedSession] {
        &self.sessions
    }

    pub fn panes(&self) -> &[PlannedPane] {
        &self.panes
    }

    pub fn degradations(&self) -> &[PlanDegradation] {
        &self.degradations
    }

    pub fn render_human(&self) -> String {
        let mut output = format!(
            "target: {}\nshell: {}\n",
            display_selector(self.destination.selector()),
            display_os(self.target_shell.executable.as_bytes())
        );
        for session in &self.sessions {
            output.push_str(&format!(
                "session {:?} cwd {}\n",
                session.name,
                display_os(session.directory.path.as_bytes())
            ));
            for window in &session.windows {
                output.push_str(&format!(
                    "  window {} {:?}\n",
                    window.source_index, window.name
                ));
            }
        }
        for pane in &self.panes {
            output.push_str(&format!(
                "  pane {}:{}:{} cwd {} [{}] {}\n",
                pane.coordinate.session_name,
                pane.coordinate.window_index,
                pane.coordinate.pane_index,
                display_os(pane.action.directory().as_bytes()),
                pane.action.directory_origin_label(),
                pane.action.label()
            ));
            match &pane.action {
                PlannedPaneAction::LeaveIdle { .. } => {}
                PlannedPaneAction::LaunchAutomatic(launch) => {
                    output.push_str(&format!(
                        "    input {} [submit Enter separately]\n",
                        display_rendered_input(launch.input().rendered().as_bytes())
                    ));
                    output.push_str(&format!(
                        "    expected {}\n",
                        automatic_expectation_label(launch.expectation())
                    ));
                    if let Some((_session_id, prompt_area)) = launch.codex_prompt() {
                        let visible_rows = prompt_area.text().visible_row_count();
                        output.push_str(&format!(
                            "    after recovery  paste {} {} without Enter\n",
                            visible_rows,
                            count_noun(visible_rows, "visible row", "visible rows"),
                        ));
                    }
                }
                PlannedPaneAction::PasteManualHint { input, .. } => {
                    output.push_str(&format!(
                        "    input {} [no Enter]\n",
                        display_rendered_input(input.as_bytes())
                    ));
                }
                PlannedPaneAction::PasteAutomaticFallback { input, reason, .. } => {
                    output.push_str(&format!(
                        "    input {} [no Enter]\n    reason {}\n",
                        display_rendered_input(input.as_bytes()),
                        automatic_fallback_label(reason)
                    ));
                }
                PlannedPaneAction::NoInput { reason, .. } => {
                    output.push_str(&format!(
                        "    capture failure {}\n",
                        terminal_safe_text(reason.message().as_bytes())
                    ));
                }
            }
        }
        if !self.degradations.is_empty() {
            output.push_str("degradations:\n");
            for degradation in &self.degradations {
                match degradation {
                    PlanDegradation::SessionDirectoryFallback { session_name } => {
                        output
                            .push_str(&format!("  session {session_name:?} directory fallback\n"));
                    }
                    PlanDegradation::PaneDirectoryFallback { pane } => {
                        output.push_str(&format!(
                            "  pane {}:{}:{} directory fallback\n",
                            pane.session_name, pane.window_index, pane.pane_index
                        ));
                    }
                    PlanDegradation::AutomaticRecoveryFallback { pane, reason } => {
                        output.push_str(&format!(
                            "  pane {}:{}:{} automatic fallback: {}\n",
                            pane.session_name,
                            pane.window_index,
                            pane.pane_index,
                            automatic_fallback_label(reason)
                        ));
                    }
                }
            }
        }
        output
    }
}

impl PlannedPaneAction {
    fn label(&self) -> &'static str {
        match self {
            Self::LeaveIdle { .. } => "leave idle shell",
            Self::LaunchAutomatic(_) => "launch automatic recovery",
            Self::PasteManualHint { .. } => "paste manual hint",
            Self::PasteAutomaticFallback { .. } => "paste automatic fallback hint",
            Self::NoInput { .. } => "no input",
        }
    }

    fn directory_origin_label(&self) -> &'static str {
        match self {
            Self::LaunchAutomatic(_) => "recorded",
            Self::LeaveIdle { directory }
            | Self::PasteManualHint { directory, .. }
            | Self::PasteAutomaticFallback { directory, .. }
            | Self::NoInput { directory, .. } => match directory.origin {
                ResolvedDirectoryOrigin::Recorded => "recorded",
                ResolvedDirectoryOrigin::HomeFallback => "home fallback",
                ResolvedDirectoryOrigin::SessionFallback => "session fallback",
            },
        }
    }
}

impl fmt::Display for RestorePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render_human())
    }
}

pub fn plan_restore(
    snapshot: &ValidatedSnapshot,
    explicit_selector: Option<TmuxSelector>,
    environment: &impl RestoreEnvironment,
) -> Result<RestorePlan, RestorePlanningError> {
    let destination = RestoreDestination::from_selector(explicit_selector.unwrap_or_else(|| {
        TmuxSelector::SocketPath(snapshot.source().path().as_os_str().to_owned())
    }));
    let target_shell = environment
        .target_shell()
        .map_err(|error| RestorePlanningError::Environment(error.to_string()))?;
    let home = environment
        .home_directory()
        .map_err(|error| RestorePlanningError::Environment(error.to_string()))?;
    if !environment.directory_exists(&home) {
        return Err(RestorePlanningError::Environment(
            "effective user home directory does not exist".to_owned(),
        ));
    }

    let mut sessions = Vec::with_capacity(snapshot.sessions().len());
    let mut panes = Vec::new();
    let mut degradations = Vec::new();
    for session in snapshot.sessions() {
        let session_directory = if environment.directory_exists(session.working_directory()) {
            ResolvedDirectory {
                path: session.working_directory().clone(),
                origin: ResolvedDirectoryOrigin::Recorded,
            }
        } else {
            degradations.push(PlanDegradation::SessionDirectoryFallback {
                session_name: session.name().to_owned(),
            });
            ResolvedDirectory {
                path: home.clone(),
                origin: ResolvedDirectoryOrigin::HomeFallback,
            }
        };
        let mut planned_windows = Vec::with_capacity(session.windows().len());
        for window in session.windows() {
            let mut pane_coordinates = Vec::with_capacity(window.panes().len());
            for pane in window.panes() {
                let coordinate = SourcePaneCoordinate {
                    session_name: session.name().to_owned(),
                    window_index: window.source_index(),
                    pane_index: pane.source_index(),
                };
                let pane_directory = if environment.directory_exists(pane.working_directory()) {
                    ResolvedDirectory {
                        path: pane.working_directory().clone(),
                        origin: ResolvedDirectoryOrigin::Recorded,
                    }
                } else {
                    degradations.push(PlanDegradation::PaneDirectoryFallback {
                        pane: coordinate.clone(),
                    });
                    ResolvedDirectory {
                        path: session_directory.path.clone(),
                        origin: ResolvedDirectoryOrigin::SessionFallback,
                    }
                };
                let action = plan_pane_action(
                    pane.recovery(),
                    pane_directory,
                    &coordinate,
                    &target_shell,
                    environment,
                    &mut degradations,
                )?;
                pane_coordinates.push(coordinate.clone());
                panes.push(PlannedPane { coordinate, action });
            }
            planned_windows.push(PlannedWindow {
                source_index: window.source_index(),
                name: window.name().to_owned(),
                pane_coordinates,
            });
        }
        sessions.push(PlannedSession {
            name: session.name().to_owned(),
            directory: session_directory,
            windows: planned_windows,
        });
    }

    Ok(RestorePlan {
        destination,
        target_shell,
        sessions,
        panes,
        degradations,
    })
}

fn plan_pane_action(
    recovery: &PaneRecovery,
    directory: ResolvedDirectory,
    coordinate: &SourcePaneCoordinate,
    shell: &TargetShell,
    environment: &impl RestoreEnvironment,
    degradations: &mut Vec<PlanDegradation>,
) -> Result<PlannedPaneAction, RestorePlanningError> {
    match recovery {
        PaneRecovery::Idle => Ok(PlannedPaneAction::LeaveIdle { directory }),
        PaneRecovery::Unavailable(failure) => Ok(PlannedPaneAction::NoInput {
            directory,
            reason: failure.clone(),
        }),
        PaneRecovery::Manual(command) => Ok(PlannedPaneAction::PasteManualHint {
            directory,
            input: render_argv(shell, command.argv(), coordinate)?,
        }),
        PaneRecovery::Automatic(automatic) => {
            let command = derive_automatic_command(automatic);
            let executable = environment.resolve_executable(&directory.path, &command.argv()[0]);
            let fallback_reason = if directory.origin != ResolvedDirectoryOrigin::Recorded {
                Some(AutomaticFallbackReason::RecordedDirectoryUnavailable)
            } else if executable.is_none() {
                Some(AutomaticFallbackReason::ExecutableUnavailable)
            } else {
                None
            };
            if let Some(reason) = fallback_reason {
                let input = render_recovery_command(shell, &command, coordinate)?;
                degradations.push(PlanDegradation::AutomaticRecoveryFallback {
                    pane: coordinate.clone(),
                    reason: reason.clone(),
                });
                return Ok(PlannedPaneAction::PasteAutomaticFallback {
                    directory,
                    input,
                    reason,
                });
            }
            let executable = executable.expect("fallback handles missing executable");
            let input = render_launch_command(shell, &command, &executable, coordinate)?;
            Ok(PlannedPaneAction::LaunchAutomatic(
                PlannedAutomaticLaunch::new(
                    ExistingRecordedDirectory(directory.path),
                    LaunchableShellInput {
                        rendered: input,
                        executable,
                    },
                    automatic,
                ),
            ))
        }
    }
}

fn render_launch_command(
    shell: &TargetShell,
    command: &RecoveryCommand,
    executable: &PlanningExecutable,
    coordinate: &SourcePaneCoordinate,
) -> Result<RenderedShellInput, RestorePlanningError> {
    let mut argv = command.argv().to_vec();
    argv[0] = LosslessOsString::try_from_bytes(executable.path().as_bytes().to_vec())
        .expect("a planning executable path is a validated OS string");
    render_argv(shell, &argv, coordinate)
}

fn render_recovery_command(
    shell: &TargetShell,
    command: &RecoveryCommand,
    coordinate: &SourcePaneCoordinate,
) -> Result<RenderedShellInput, RestorePlanningError> {
    render_argv(shell, command.argv(), coordinate)
}

fn render_argv(
    shell: &TargetShell,
    argv: &[LosslessOsString],
    coordinate: &SourcePaneCoordinate,
) -> Result<RenderedShellInput, RestorePlanningError> {
    let mut output = Vec::new();
    for (index, argument) in argv.iter().enumerate() {
        if contains_terminal_control(argument.as_bytes()) {
            return Err(RestorePlanningError::UnsafeShellInput {
                pane: coordinate.clone(),
                argument_index: index,
            });
        }
        if index != 0 {
            output.push(b' ');
        }
        output.push(b'\'');
        for byte in argument.as_bytes() {
            if *byte == b'\'' {
                output.extend_from_slice(b"'\\''");
            } else {
                output.push(*byte);
            }
        }
        output.push(b'\'');
    }
    if output.len() > MAX_RENDERED_SHELL_INPUT_BYTES {
        return Err(RestorePlanningError::ShellInputTooLarge {
            pane: coordinate.clone(),
            actual: output.len(),
            maximum: MAX_RENDERED_SHELL_INPUT_BYTES,
        });
    }
    Ok(RenderedShellInput {
        bytes: output,
        shell: shell.clone(),
    })
}

fn contains_terminal_control(bytes: &[u8]) -> bool {
    let mut remaining = bytes;
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(value) => return value.chars().any(char::is_control),
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                if std::str::from_utf8(&remaining[..valid_up_to])
                    .is_ok_and(|value| value.chars().any(char::is_control))
                {
                    return true;
                }
                let invalid_end = error
                    .error_len()
                    .map_or(remaining.len(), |length| valid_up_to + length);
                if remaining[valid_up_to..invalid_end]
                    .iter()
                    .any(|byte| *byte < 0x20 || matches!(*byte, 0x7f..=0x9f))
                {
                    return true;
                }
                remaining = &remaining[invalid_end..];
            }
        }
    }
    false
}

fn effective_user_record() -> Result<(OsString, OsString), RestoreEnvironmentFailure> {
    let buffer_size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let buffer_size = if buffer_size <= 0 {
        16 * 1024
    } else {
        usize::try_from(buffer_size).unwrap_or(16 * 1024)
    };
    let mut buffer = vec![0_u8; buffer_size];
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() {
        return Err(RestoreEnvironmentFailure::new(format!(
            "getpwuid_r failed with status {status}"
        )));
    }
    let record = unsafe { record.assume_init() };
    let home = unsafe { CStr::from_ptr(record.pw_dir) };
    let shell = unsafe { CStr::from_ptr(record.pw_shell) };
    Ok((
        OsString::from_vec(home.to_bytes().to_vec()),
        OsString::from_vec(shell.to_bytes().to_vec()),
    ))
}

fn is_executable_file(path: &Path) -> bool {
    if !fs::metadata(path)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
    {
        return false;
    }
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0 }
}

const CONVENTIONAL_SHELL_PATHS: &[&str] = &[
    "/bin/sh",
    "/usr/bin/sh",
    "/bin/bash",
    "/usr/bin/bash",
    "/bin/dash",
    "/usr/bin/dash",
    "/bin/zsh",
    "/usr/bin/zsh",
    "/bin/ksh",
    "/usr/bin/ksh",
    "/bin/mksh",
    "/usr/bin/mksh",
    "/bin/ash",
    "/usr/bin/ash",
];

#[derive(Clone, Debug)]
struct ShellRuntimeCatalog {
    identities: Vec<PathBuf>,
}

impl ShellRuntimeCatalog {
    fn from_system() -> Self {
        let mut authority_paths = CONVENTIONAL_SHELL_PATHS
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        if let Ok(shells) = fs::read("/etc/shells") {
            authority_paths.extend(shell_authority_paths(&shells));
        }
        Self::from_authority_paths(authority_paths)
    }

    fn from_authority_paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut identities = paths
            .into_iter()
            .filter(|path| path.is_absolute() && shell_dialect(path).is_some())
            .filter_map(|path| fs::canonicalize(path).ok())
            .collect::<Vec<_>>();
        identities.sort_unstable();
        identities.dedup();
        Self { identities }
    }

    fn authorizes(&self, runtime: &Path) -> bool {
        self.identities
            .binary_search_by(|identity| identity.as_path().cmp(runtime))
            .is_ok()
    }
}

fn shell_authority_paths(shells: &[u8]) -> Vec<PathBuf> {
    shells
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let line = line
                .split(|byte| *byte == b'#')
                .next()
                .map(trim_ascii_whitespace)
                .unwrap_or_default();
            (!line.is_empty()).then(|| PathBuf::from(OsString::from_vec(line.to_vec())))
        })
        .collect()
}

#[derive(Clone, Copy)]
enum ElfByteOrder {
    Little,
    Big,
}

struct ElfLoadSegment {
    file_offset: u64,
    virtual_address: u64,
    file_size: u64,
    memory_size: u64,
    flags: u32,
    alignment: u64,
}

impl ElfLoadSegment {
    fn parse(program_header: &[u8], class: u8, byte_order: ElfByteOrder) -> Option<Self> {
        match class {
            1 => Some(Self {
                file_offset: u64::from(elf_u32(program_header, 4, byte_order)?),
                virtual_address: u64::from(elf_u32(program_header, 8, byte_order)?),
                file_size: u64::from(elf_u32(program_header, 16, byte_order)?),
                memory_size: u64::from(elf_u32(program_header, 20, byte_order)?),
                flags: elf_u32(program_header, 24, byte_order)?,
                alignment: u64::from(elf_u32(program_header, 28, byte_order)?),
            }),
            2 => Some(Self {
                flags: elf_u32(program_header, 4, byte_order)?,
                file_offset: elf_u64(program_header, 8, byte_order)?,
                virtual_address: elf_u64(program_header, 16, byte_order)?,
                file_size: elf_u64(program_header, 32, byte_order)?,
                memory_size: elf_u64(program_header, 40, byte_order)?,
                alignment: elf_u64(program_header, 48, byte_order)?,
            }),
            _ => None,
        }
    }

    fn is_well_formed(&self, file_length: u64) -> bool {
        if self.file_size > self.memory_size {
            return false;
        }
        let Some(file_end) = self.file_offset.checked_add(self.file_size) else {
            return false;
        };
        if self.virtual_address.checked_add(self.memory_size).is_none() {
            return false;
        }
        if self.file_offset > file_length || file_end > file_length {
            return false;
        }
        self.alignment <= 1
            || (self.alignment.is_power_of_two()
                && self.file_offset % self.alignment == self.virtual_address % self.alignment)
    }

    fn contains_file_backed_executable_entry(&self, entry_point: u64) -> bool {
        let Some(file_backed_end) = self.virtual_address.checked_add(self.file_size) else {
            return false;
        };
        self.flags & 1 != 0 && entry_point >= self.virtual_address && entry_point < file_backed_end
    }
}

fn is_native_linux_executable(file: &File) -> bool {
    const ELF_IDENT_SIZE: usize = 16;
    const MAX_PROGRAM_HEADERS: u16 = 4_096;

    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file() || metadata.mode() & 0o111 == 0 {
        return false;
    }

    let mut ident = [0_u8; ELF_IDENT_SIZE];
    if file.read_exact_at(&mut ident, 0).is_err() || ident[..4] != *b"\x7fELF" {
        return false;
    }
    let class = ident[4];
    let expected_class = if usize::BITS == 64 { 2 } else { 1 };
    if class != expected_class || ident[6] != 1 {
        return false;
    }
    let byte_order = match ident[5] {
        1 if cfg!(target_endian = "little") => ElfByteOrder::Little,
        2 if cfg!(target_endian = "big") => ElfByteOrder::Big,
        _ => return false,
    };
    let (
        header_size,
        program_offset_field,
        header_size_field,
        program_entry_size_field,
        program_count_field,
        expected_program_entry_size,
    ) = match class {
        1 => (52_usize, 28_usize, 40_usize, 42_usize, 44_usize, 32_u16),
        2 => (64_usize, 32_usize, 52_usize, 54_usize, 56_usize, 56_u16),
        _ => return false,
    };
    let mut header = vec![0_u8; header_size];
    if file.read_exact_at(&mut header, 0).is_err() {
        return false;
    }
    if !matches!(elf_u16(&header, 16, byte_order), Some(2 | 3))
        || elf_u16(&header, 18, byte_order) != native_elf_machine()
        || elf_u32(&header, 20, byte_order) != Some(1)
        || elf_u16(&header, header_size_field, byte_order) != Some(header_size as u16)
    {
        return false;
    }

    let program_offset = match class {
        1 => elf_u32(&header, program_offset_field, byte_order).map(u64::from),
        2 => elf_u64(&header, program_offset_field, byte_order),
        _ => None,
    };
    let Some(program_offset) = program_offset else {
        return false;
    };
    let entry_point = match class {
        1 => elf_u32(&header, 24, byte_order).map(u64::from),
        2 => elf_u64(&header, 24, byte_order),
        _ => None,
    };
    let Some(entry_point) = entry_point else {
        return false;
    };
    let Some(program_entry_size) = elf_u16(&header, program_entry_size_field, byte_order) else {
        return false;
    };
    let Some(program_count) = elf_u16(&header, program_count_field, byte_order) else {
        return false;
    };
    if program_offset < header_size as u64
        || program_entry_size != expected_program_entry_size
        || program_count == 0
        || program_count > MAX_PROGRAM_HEADERS
    {
        return false;
    }
    let Some(program_bytes) = u64::from(program_entry_size).checked_mul(u64::from(program_count))
    else {
        return false;
    };
    let Some(program_end) = program_offset.checked_add(program_bytes) else {
        return false;
    };
    if program_end > metadata.len() {
        return false;
    }

    let mut saw_load_segment = false;
    let mut entry_is_executable = false;
    for index in 0..program_count {
        let offset = program_offset + u64::from(index) * u64::from(program_entry_size);
        let mut encoded = vec![0_u8; usize::from(program_entry_size)];
        if file.read_exact_at(&mut encoded, offset).is_err() {
            return false;
        }
        if elf_u32(&encoded, 0, byte_order) != Some(1) {
            continue;
        }
        let Some(segment) = ElfLoadSegment::parse(&encoded, class, byte_order) else {
            return false;
        };
        if !segment.is_well_formed(metadata.len()) {
            return false;
        }
        saw_load_segment = true;
        entry_is_executable |= segment.contains_file_backed_executable_entry(entry_point);
    }
    saw_load_segment && entry_is_executable
}

fn native_elf_machine() -> Option<u16> {
    match std::env::consts::ARCH {
        "x86" => Some(3),
        "mips" | "mips64" => Some(8),
        "powerpc" => Some(20),
        "powerpc64" => Some(21),
        "s390x" => Some(22),
        "arm" => Some(40),
        "sparc64" => Some(43),
        "x86_64" => Some(62),
        "aarch64" => Some(183),
        "riscv32" | "riscv64" => Some(243),
        "loongarch64" => Some(258),
        _ => None,
    }
}

fn elf_u16(bytes: &[u8], offset: usize, byte_order: ElfByteOrder) -> Option<u16> {
    let encoded = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(match byte_order {
        ElfByteOrder::Little => u16::from_le_bytes(encoded),
        ElfByteOrder::Big => u16::from_be_bytes(encoded),
    })
}

fn elf_u32(bytes: &[u8], offset: usize, byte_order: ElfByteOrder) -> Option<u32> {
    let encoded = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(match byte_order {
        ElfByteOrder::Little => u32::from_le_bytes(encoded),
        ElfByteOrder::Big => u32::from_be_bytes(encoded),
    })
}

fn elf_u64(bytes: &[u8], offset: usize, byte_order: ElfByteOrder) -> Option<u64> {
    let encoded = bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(match byte_order {
        ElfByteOrder::Little => u64::from_le_bytes(encoded),
        ElfByteOrder::Big => u64::from_be_bytes(encoded),
    })
}

fn trim_ascii_whitespace(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn terminal_safe_text(bytes: &[u8]) -> String {
    let mut output = String::new();
    for character in String::from_utf8_lossy(bytes).chars() {
        output.extend(character.escape_default());
        if output.len() >= 4_000 {
            output.truncate(4_000);
            break;
        }
    }
    if output.is_empty() {
        "environment operation failed".to_owned()
    } else {
        output
    }
}

fn display_os(bytes: &[u8]) -> String {
    let mut output = String::new();
    for byte in bytes {
        let encoded = if *byte == b'\\' {
            "\\\\".to_owned()
        } else if matches!(byte, 0x20..=0x7e) {
            char::from(*byte).to_string()
        } else {
            format!("\\x{byte:02x}")
        };
        if output.len() + encoded.len() > 4_000 {
            break;
        }
        output.push_str(&encoded);
    }
    if output.is_empty() {
        "<empty>".to_owned()
    } else {
        output
    }
}

fn display_selector(selector: &TmuxSelector) -> String {
    match selector {
        TmuxSelector::SocketName(value) => format!("-L {}", display_os(value.as_bytes())),
        TmuxSelector::SocketPath(value) => format!("-S {}", display_os(value.as_bytes())),
    }
}

fn display_rendered_input(bytes: &[u8]) -> String {
    let mut output = String::new();
    for byte in bytes {
        if *byte == b'\\' {
            output.push_str("\\\\");
        } else if matches!(byte, 0x20..=0x7e) {
            output.push(char::from(*byte));
        } else {
            output.push_str(&format!("\\x{byte:02x}"));
        }
    }
    output
}

fn automatic_fallback_label(reason: &AutomaticFallbackReason) -> &'static str {
    match reason {
        AutomaticFallbackReason::RecordedDirectoryUnavailable => "recorded directory unavailable",
        AutomaticFallbackReason::ExecutableUnavailable => "executable unavailable",
    }
}

fn count_noun(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}

fn automatic_expectation_label(expected: &AutomaticRecoveryExpectation) -> String {
    match expected {
        AutomaticRecoveryExpectation::Codex(session_id) => {
            format!("Codex session {}", session_id.as_uuid())
        }
        AutomaticRecoveryExpectation::ClaudeCode(session_id) => {
            format!("Claude Code session {}", session_id.as_uuid())
        }
        AutomaticRecoveryExpectation::MdBookServe(_) => "mdBook serve command".to_owned(),
        AutomaticRecoveryExpectation::BookshelfServe(_) => {
            "mdbook-bookshelf serve command".to_owned()
        }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct TargetClaimFailure {
    message: String,
    target_state: RestoreTargetState,
}

impl TargetClaimFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self::with_target_state(message, RestoreTargetState::NotEstablished)
    }

    pub fn with_target_state(message: impl Into<String>, target_state: RestoreTargetState) -> Self {
        Self {
            message: terminal_safe_text(message.into().as_bytes()),
            target_state,
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn target_state(&self) -> &RestoreTargetState {
        &self.target_state
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct TopologyFailure {
    message: String,
}

impl TopologyFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: terminal_safe_text(message.into().as_bytes()),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetDisposition {
    Removed,
    Retained,
    Missing,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RollbackFailureDisposition {
    Retained,
    Unknown,
}

impl RollbackFailureDisposition {
    fn target_disposition(self) -> TargetDisposition {
        match self {
            Self::Retained => TargetDisposition::Retained,
            Self::Unknown => TargetDisposition::Unknown,
        }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct RollbackFailure {
    disposition: RollbackFailureDisposition,
    message: String,
}

impl RollbackFailure {
    pub fn new(disposition: RollbackFailureDisposition, message: impl Into<String>) -> Self {
        Self {
            disposition,
            message: terminal_safe_text(message.into().as_bytes()),
        }
    }

    pub fn disposition(&self) -> RollbackFailureDisposition {
        self.disposition
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RollbackOutcome {
    Removed,
    Failed(RollbackFailure),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RestoreTargetState {
    NotEstablished,
    Observed(TargetDisposition),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GuardedPaneFailure {
    ShellNotForeground,
    PaneMissing,
    Failed(String),
}

#[derive(Clone, Copy, Debug)]
pub enum GuardedPaneOperation<'a> {
    VerifyShell,
    PasteLiteral { input: &'a RenderedShellInput },
    LaunchAutomatic { input: &'a LaunchableShellInput },
}

pub type GuardedPaneResult = Result<(), GuardedPaneFailure>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AutomaticPaneObservation {
    Recovered,
    ShellForeground,
    UnexpectedForeground,
    PaneMissing,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexPromptPasteFailure {
    SessionMismatch,
    PaneMissing,
    Failed(String),
}

pub type CodexPromptPasteResult = Result<(), CodexPromptPasteFailure>;

pub trait RestoreTargetCapability {
    fn claim(
        &mut self,
        destination: &RestoreDestination,
        shell: &TargetShell,
    ) -> Result<Box<dyn OwnedRestoreTarget>, TargetClaimFailure>;
}

pub trait OwnedRestoreTarget {
    fn create_topology(&mut self, plan: &RestorePlan) -> Result<(), TopologyFailure>;
    fn rollback(self: Box<Self>) -> RollbackOutcome;
    fn begin_recovery(self: Box<Self>) -> Box<dyn RecoveryRestoreTarget>;
}

pub trait RecoveryRestoreTarget {
    fn guarded_pane_operation(
        &mut self,
        pane: &SourcePaneCoordinate,
        shell: &TargetShell,
        operation: GuardedPaneOperation<'_>,
    ) -> GuardedPaneResult;

    fn observe_automatic(
        &mut self,
        pane: &SourcePaneCoordinate,
        expected: &AutomaticRecoveryExpectation,
    ) -> AutomaticPaneObservation;

    fn paste_codex_prompt_area(
        &mut self,
        pane: &SourcePaneCoordinate,
        expected: &CodexSessionId,
        input: &CapturedCodexPromptArea,
    ) -> CodexPromptPasteResult;

    fn observe_disposition(&mut self) -> TargetDisposition;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttentionReason {
    ShellNotForeground,
    MissingPane,
    UnexpectedForeground,
    CapturedRecoveryUnavailable(CaptureFailure),
    GuardedOperationFailed(String),
    AutomaticObservationFailed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PaneRestoreOutcome {
    RestoredIdleShell,
    RecoveredAutomatically,
    RecoveredAutomaticallyWithPromptPrepared,
    RecoveredAutomaticallyWithPromptNeedsAttention(CodexPromptPasteFailure),
    PreparedManualHint,
    PreparedAutomaticFallbackHint(AutomaticFallbackReason),
    AutomaticLaunchFailedHintPrepared,
    NeedsAttention(AttentionReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneRestoreResult {
    coordinate: SourcePaneCoordinate,
    outcome: PaneRestoreOutcome,
}

impl PaneRestoreResult {
    pub fn coordinate(&self) -> &SourcePaneCoordinate {
        &self.coordinate
    }

    pub fn outcome(&self) -> &PaneRestoreOutcome {
        &self.outcome
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestoreRunStatus {
    Complete,
    Partial,
    Fatal,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RestoreExecutionFailure {
    #[error("restore target claim failed: {failure}")]
    TargetClaimFailed { failure: TargetClaimFailure },
    #[error("restore topology failed: {failure}")]
    TopologyFailed { failure: TopologyFailure },
    #[error("restore topology failed: {topology_failure}; cleanup failed: {cleanup_failure}")]
    TopologyAndCleanupFailed {
        topology_failure: TopologyFailure,
        cleanup_failure: RollbackFailure,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreRunResult {
    status: RestoreRunStatus,
    target_state: RestoreTargetState,
    failure: Option<RestoreExecutionFailure>,
    panes: Vec<PaneRestoreResult>,
}

impl RestoreRunResult {
    pub fn status(&self) -> RestoreRunStatus {
        self.status
    }

    pub fn target_state(&self) -> &RestoreTargetState {
        &self.target_state
    }

    pub fn failure(&self) -> Option<&RestoreExecutionFailure> {
        self.failure.as_ref()
    }

    pub fn panes(&self) -> &[PaneRestoreResult] {
        &self.panes
    }

    fn fatal(failure: RestoreExecutionFailure, target_state: RestoreTargetState) -> Self {
        Self {
            status: RestoreRunStatus::Fatal,
            target_state,
            failure: Some(failure),
            panes: Vec::new(),
        }
    }
}

pub struct RestoreExecutor<T> {
    target: T,
}

impl<T: RestoreTargetCapability> RestoreExecutor<T> {
    pub fn new(target: T) -> Self {
        Self { target }
    }

    pub fn execute(&mut self, plan: RestorePlan) -> RestoreRunResult {
        let mut owned = match self.target.claim(plan.destination(), plan.target_shell()) {
            Ok(owned) => owned,
            Err(failure) => {
                let target_state = failure.target_state().clone();
                return RestoreRunResult::fatal(
                    RestoreExecutionFailure::TargetClaimFailed { failure },
                    target_state,
                );
            }
        };
        if let Err(failure) = owned.create_topology(&plan) {
            return match owned.rollback() {
                RollbackOutcome::Removed => RestoreRunResult::fatal(
                    RestoreExecutionFailure::TopologyFailed { failure },
                    RestoreTargetState::Observed(TargetDisposition::Removed),
                ),
                RollbackOutcome::Failed(cleanup_failure) => {
                    let disposition = cleanup_failure.disposition().target_disposition();
                    RestoreRunResult::fatal(
                        RestoreExecutionFailure::TopologyAndCleanupFailed {
                            topology_failure: failure,
                            cleanup_failure,
                        },
                        RestoreTargetState::Observed(disposition),
                    )
                }
            };
        }

        let mut recovery = owned.begin_recovery();
        let mut partial = !plan.degradations().is_empty();
        let panes = plan
            .panes()
            .iter()
            .map(|pane| {
                let outcome = execute_pane(&mut *recovery, plan.target_shell(), pane);
                partial |= pane_outcome_is_partial(&outcome);
                PaneRestoreResult {
                    coordinate: pane.coordinate.clone(),
                    outcome,
                }
            })
            .collect();
        let disposition = recovery.observe_disposition();
        partial |= disposition != TargetDisposition::Retained;

        RestoreRunResult {
            status: if partial {
                RestoreRunStatus::Partial
            } else {
                RestoreRunStatus::Complete
            },
            target_state: RestoreTargetState::Observed(disposition),
            failure: None,
            panes,
        }
    }
}

fn execute_pane(
    target: &mut dyn RecoveryRestoreTarget,
    shell: &TargetShell,
    pane: &PlannedPane,
) -> PaneRestoreOutcome {
    match &pane.action {
        PlannedPaneAction::LeaveIdle { .. } => map_guarded_result(
            target.guarded_pane_operation(
                &pane.coordinate,
                shell,
                GuardedPaneOperation::VerifyShell,
            ),
            PaneRestoreOutcome::RestoredIdleShell,
        ),
        PlannedPaneAction::PasteManualHint { input, .. } => map_guarded_result(
            target.guarded_pane_operation(
                &pane.coordinate,
                shell,
                GuardedPaneOperation::PasteLiteral { input },
            ),
            PaneRestoreOutcome::PreparedManualHint,
        ),
        PlannedPaneAction::PasteAutomaticFallback { input, reason, .. } => map_guarded_result(
            target.guarded_pane_operation(
                &pane.coordinate,
                shell,
                GuardedPaneOperation::PasteLiteral { input },
            ),
            PaneRestoreOutcome::PreparedAutomaticFallbackHint(reason.clone()),
        ),
        PlannedPaneAction::NoInput { reason, .. } => PaneRestoreOutcome::NeedsAttention(
            AttentionReason::CapturedRecoveryUnavailable(reason.clone()),
        ),
        PlannedPaneAction::LaunchAutomatic(launch) => {
            execute_automatic(target, shell, pane, launch)
        }
    }
}

fn execute_automatic(
    target: &mut dyn RecoveryRestoreTarget,
    shell: &TargetShell,
    pane: &PlannedPane,
    launch: &PlannedAutomaticLaunch,
) -> PaneRestoreOutcome {
    if let Err(failure) = target.guarded_pane_operation(
        &pane.coordinate,
        shell,
        GuardedPaneOperation::LaunchAutomatic {
            input: launch.input(),
        },
    ) {
        return guard_failure(failure);
    }

    match target.observe_automatic(&pane.coordinate, launch.expectation()) {
        AutomaticPaneObservation::Recovered => match launch.codex_prompt() {
            None => PaneRestoreOutcome::RecoveredAutomatically,
            Some((expected, input)) => {
                match target.paste_codex_prompt_area(&pane.coordinate, expected, input) {
                    Ok(()) => PaneRestoreOutcome::RecoveredAutomaticallyWithPromptPrepared,
                    Err(failure) => {
                        PaneRestoreOutcome::RecoveredAutomaticallyWithPromptNeedsAttention(failure)
                    }
                }
            }
        },
        AutomaticPaneObservation::ShellForeground => {
            let fallback = target.guarded_pane_operation(
                &pane.coordinate,
                shell,
                GuardedPaneOperation::PasteLiteral {
                    input: launch.input().rendered(),
                },
            );
            map_guarded_result(
                fallback,
                PaneRestoreOutcome::AutomaticLaunchFailedHintPrepared,
            )
        }
        AutomaticPaneObservation::UnexpectedForeground => {
            PaneRestoreOutcome::NeedsAttention(AttentionReason::UnexpectedForeground)
        }
        AutomaticPaneObservation::PaneMissing => {
            PaneRestoreOutcome::NeedsAttention(AttentionReason::MissingPane)
        }
        AutomaticPaneObservation::Failed(reason) => {
            PaneRestoreOutcome::NeedsAttention(AttentionReason::AutomaticObservationFailed(reason))
        }
    }
}

fn map_guarded_result(
    result: Result<(), GuardedPaneFailure>,
    success: PaneRestoreOutcome,
) -> PaneRestoreOutcome {
    match result {
        Ok(()) => success,
        Err(failure) => guard_failure(failure),
    }
}

fn guard_failure(failure: GuardedPaneFailure) -> PaneRestoreOutcome {
    let reason = match failure {
        GuardedPaneFailure::ShellNotForeground => AttentionReason::ShellNotForeground,
        GuardedPaneFailure::PaneMissing => AttentionReason::MissingPane,
        GuardedPaneFailure::Failed(reason) => AttentionReason::GuardedOperationFailed(reason),
    };
    PaneRestoreOutcome::NeedsAttention(reason)
}

fn pane_outcome_is_partial(outcome: &PaneRestoreOutcome) -> bool {
    matches!(
        outcome,
        PaneRestoreOutcome::PreparedAutomaticFallbackHint(_)
            | PaneRestoreOutcome::AutomaticLaunchFailedHintPrepared
            | PaneRestoreOutcome::RecoveredAutomaticallyWithPromptNeedsAttention(_)
            | PaneRestoreOutcome::NeedsAttention(_)
    )
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RestorePlanningError {
    #[error("restore environment failed: {0}")]
    Environment(String),
    #[error("pane {pane:?} argument {argument_index} contains terminal control bytes")]
    UnsafeShellInput {
        pane: SourcePaneCoordinate,
        argument_index: usize,
    },
    #[error("pane {pane:?} rendered shell input is {actual} bytes; the maximum is {maximum}")]
    ShellInputTooLarge {
        pane: SourcePaneCoordinate,
        actual: usize,
        maximum: usize,
    },
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn rejects_a_truncated_elf_even_when_its_path_is_registered() {
        let temp = tempfile::tempdir().unwrap();
        let shell = temp.path().join("bash");
        fs::write(&shell, b"\x7fELF").unwrap();
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
        let catalog = ShellRuntimeCatalog::from_authority_paths([shell.clone()]);

        let error =
            TargetShell::try_from_bytes_with_catalog(shell.into_os_string().into_vec(), &catalog)
                .unwrap_err();

        assert_eq!(error, TargetShellError::MalformedNativeExecutable);
    }

    #[test]
    fn rejects_a_registered_elf_with_an_out_of_bounds_load_segment() {
        let temp = tempfile::tempdir().unwrap();
        let shell = temp.path().join("bash");
        let mut elf = fs::read("/bin/sh").unwrap();
        corrupt_first_load_segment_offset(&mut elf);
        fs::write(&shell, elf).unwrap();
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
        let catalog = ShellRuntimeCatalog::from_authority_paths([shell.clone()]);

        let error =
            TargetShell::try_from_bytes_with_catalog(shell.into_os_string().into_vec(), &catalog)
                .unwrap_err();

        assert_eq!(error, TargetShellError::MalformedNativeExecutable);
    }

    #[test]
    fn accepts_a_registered_custom_supported_native_shell() {
        let temp = tempfile::tempdir().unwrap();
        let shell = temp.path().join("bash");
        fs::copy("/bin/sh", &shell).unwrap();
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
        let catalog = ShellRuntimeCatalog::from_authority_paths([shell.clone()]);

        let target_shell = TargetShell::try_from_bytes_with_catalog(
            shell.as_os_str().as_bytes().to_vec(),
            &catalog,
        )
        .unwrap();

        assert_eq!(
            target_shell.executable().as_bytes(),
            shell.as_os_str().as_bytes()
        );
    }

    #[test]
    fn conventional_shells_do_not_depend_on_an_etc_shells_registry() {
        let catalog = ShellRuntimeCatalog::from_authority_paths([PathBuf::from("/bin/sh")]);

        assert!(TargetShell::try_from_bytes_with_catalog(b"/bin/sh".to_vec(), &catalog).is_ok());
    }

    #[test]
    fn shell_registry_ignores_comments_relative_paths_and_unsupported_names() {
        let paths = shell_authority_paths(
            b"\n# comment\nrelative/bash\n/usr/bin/tmux\n /bin/sh # default\n",
        );
        let catalog = ShellRuntimeCatalog::from_authority_paths(paths);

        assert_eq!(catalog.identities, [fs::canonicalize("/bin/sh").unwrap()]);
    }

    fn corrupt_first_load_segment_offset(elf: &mut [u8]) {
        let byte_order = match elf[5] {
            1 => ElfByteOrder::Little,
            2 => ElfByteOrder::Big,
            _ => panic!("fixture is not an ELF with a supported byte order"),
        };
        let (program_offset, program_entry_size, program_count, load_offset_field) = match elf[4] {
            1 => (
                u64::from(elf_u32(elf, 28, byte_order).unwrap()),
                elf_u16(elf, 42, byte_order).unwrap(),
                elf_u16(elf, 44, byte_order).unwrap(),
                4_usize,
            ),
            2 => (
                elf_u64(elf, 32, byte_order).unwrap(),
                elf_u16(elf, 54, byte_order).unwrap(),
                elf_u16(elf, 56, byte_order).unwrap(),
                8_usize,
            ),
            _ => panic!("fixture is not a supported ELF class"),
        };
        for index in 0..program_count {
            let start =
                usize::try_from(program_offset + u64::from(index) * u64::from(program_entry_size))
                    .unwrap();
            if elf_u32(elf, start, byte_order) != Some(1) {
                continue;
            }
            let offset = start + load_offset_field;
            match elf[4] {
                1 => elf[offset..offset + 4].copy_from_slice(&match byte_order {
                    ElfByteOrder::Little => u32::MAX.to_le_bytes(),
                    ElfByteOrder::Big => u32::MAX.to_be_bytes(),
                }),
                2 => elf[offset..offset + 8].copy_from_slice(&match byte_order {
                    ElfByteOrder::Little => u64::MAX.to_le_bytes(),
                    ElfByteOrder::Big => u64::MAX.to_be_bytes(),
                }),
                _ => unreachable!(),
            }
            return;
        }
        panic!("fixture has no load segment");
    }
}
