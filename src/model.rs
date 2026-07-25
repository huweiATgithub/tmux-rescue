use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

pub const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SESSIONS: usize = 1_024;
pub const MAX_WINDOWS_PER_SESSION: usize = 1_024;
pub const MAX_PANES_PER_WINDOW: usize = 1_024;
pub const MAX_ARGUMENTS: usize = 4_096;
pub const MAX_OS_VALUE_BYTES: usize = 1024 * 1024;
pub const MAX_NAME_BYTES: usize = 4_096;
pub const MAX_DIAGNOSTIC_BYTES: usize = 4_096;
pub const MAX_CODEX_PROMPT_BYTES: usize = 16 * 1024;
pub const MAX_TOPOLOGY_VALIDATION_ATTEMPTS: usize = 3;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LosslessOsString(Vec<u8>);

impl LosslessOsString {
    pub fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, SnapshotValidationError> {
        Self::from_bytes(bytes, "operating-system value")
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn as_os_str(&self) -> &OsStr {
        OsStr::from_bytes(&self.0)
    }

    pub fn to_os_string(&self) -> OsString {
        OsString::from_vec(self.0.clone())
    }

    fn from_raw(
        raw: RawEncodedOsString,
        field: impl Into<String>,
    ) -> Result<Self, SnapshotValidationError> {
        let field = field.into();
        let (bytes, base64_encoded) = match raw.encoding.as_str() {
            "utf8" => (raw.value.into_bytes(), false),
            "base64" => {
                let decoded = BASE64.decode(raw.value.as_bytes()).map_err(|error| {
                    SnapshotValidationError::InvalidOsEncoding {
                        field: field.clone(),
                        reason: error.to_string(),
                    }
                })?;
                (decoded, true)
            }
            encoding => {
                return Err(SnapshotValidationError::InvalidOsEncoding {
                    field,
                    reason: format!("unsupported encoding {encoding:?}"),
                });
            }
        };

        let value = Self::from_bytes(bytes, field.clone())?;
        if base64_encoded && std::str::from_utf8(value.as_bytes()).is_ok() {
            return Err(SnapshotValidationError::InvalidOsEncoding {
                field,
                reason: "base64 is noncanonical for valid UTF-8 bytes".to_owned(),
            });
        }
        Ok(value)
    }

    pub(crate) fn from_bytes(
        bytes: Vec<u8>,
        field: impl Into<String>,
    ) -> Result<Self, SnapshotValidationError> {
        let field = field.into();
        if bytes.len() > MAX_OS_VALUE_BYTES {
            return Err(SnapshotValidationError::OsValueTooLong {
                field,
                actual: bytes.len(),
                maximum: MAX_OS_VALUE_BYTES,
            });
        }
        if bytes.contains(&0) {
            return Err(SnapshotValidationError::OsValueContainsNul { field });
        }
        Ok(Self(bytes))
    }

    fn to_raw(&self) -> RawEncodedOsString {
        match std::str::from_utf8(&self.0) {
            Ok(value) => RawEncodedOsString {
                encoding: "utf8".to_owned(),
                value: value.to_owned(),
            },
            Err(_) => RawEncodedOsString {
                encoding: "base64".to_owned(),
                value: BASE64.encode(&self.0),
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawEncodedOsString {
    pub encoding: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RawCaptureConsistency {
    Stable {},
    Unstable { attempts: usize },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawCapturedCommand {
    pub executable: RawEncodedOsString,
    pub argv: Vec<RawEncodedOsString>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawCapturedCodexPromptArea {
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RawAutomaticRecovery {
    Codex {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt_area: Option<RawCapturedCodexPromptArea>,
    },
    ClaudeCode {
        session_id: String,
    },
    MdBookServe {
        command: RawCapturedCommand,
    },
    BookshelfServe {
        command: RawCapturedCommand,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RawPaneRecovery {
    Idle {},
    Automatic { recovery: RawAutomaticRecovery },
    Manual { command: RawCapturedCommand },
    Unavailable { failure: String },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawPaneSnapshot {
    pub source_index: u32,
    pub working_directory: RawEncodedOsString,
    pub recovery: RawPaneRecovery,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawWindowSnapshot {
    pub source_index: u32,
    pub name: String,
    pub panes: Vec<RawPaneSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawSessionSnapshot {
    pub name: String,
    pub working_directory: RawEncodedOsString,
    pub windows: Vec<RawWindowSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RawSnapshot {
    pub captured_at: String,
    pub source: RawEncodedOsString,
    pub consistency: RawCaptureConsistency,
    pub sessions: Vec<RawSessionSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureTime {
    value: OffsetDateTime,
    encoded: String,
}

impl CaptureTime {
    pub fn value(&self) -> OffsetDateTime {
        self.value
    }

    pub fn encoded(&self) -> &str {
        &self.encoded
    }

    pub fn parse_rfc3339(encoded: &str) -> Result<Self, SnapshotValidationError> {
        Self::parse(encoded.to_owned())
    }

    fn parse(encoded: String) -> Result<Self, SnapshotValidationError> {
        let value = OffsetDateTime::parse(&encoded, &Rfc3339).map_err(|_| {
            SnapshotValidationError::InvalidCaptureTime {
                value: encoded.clone(),
            }
        })?;
        Ok(Self { value, encoded })
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RecordedAbsolutePath(LosslessOsString);

impl RecordedAbsolutePath {
    pub fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, SnapshotValidationError> {
        let value = LosslessOsString::from_bytes(bytes, "recorded path")?;
        if !Path::new(value.as_os_str()).is_absolute() {
            return Err(SnapshotValidationError::PathNotAbsolute {
                field: "recorded path".to_owned(),
            });
        }
        Ok(Self(value))
    }

    pub fn as_os_str(&self) -> &OsStr {
        self.0.as_os_str()
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    fn from_raw(
        raw: RawEncodedOsString,
        field: impl Into<String>,
    ) -> Result<Self, SnapshotValidationError> {
        let field = field.into();
        let value = LosslessOsString::from_raw(raw, field.clone())?;
        if !Path::new(value.as_os_str()).is_absolute() {
            return Err(SnapshotValidationError::PathNotAbsolute { field });
        }
        Ok(Self(value))
    }

    pub(crate) fn to_raw(&self) -> RawEncodedOsString {
        self.0.to_raw()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotSource {
    path: RecordedAbsolutePath,
}

impl SnapshotSource {
    pub fn try_from_bytes(bytes: Vec<u8>) -> Result<Self, SnapshotValidationError> {
        Ok(Self {
            path: RecordedAbsolutePath::try_from_bytes(bytes)?,
        })
    }

    pub fn path(&self) -> &RecordedAbsolutePath {
        &self.path
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExhaustedAttemptCount(usize);

impl ExhaustedAttemptCount {
    pub fn get(self) -> usize {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaptureConsistency {
    Stable,
    Unstable { attempts: ExhaustedAttemptCount },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedCommand {
    executable: LosslessOsString,
    argv: Vec<LosslessOsString>,
}

impl CapturedCommand {
    pub fn try_new(
        executable: LosslessOsString,
        argv: Vec<LosslessOsString>,
    ) -> Result<Self, SnapshotValidationError> {
        Self::from_raw(
            RawCapturedCommand {
                executable: executable.to_raw(),
                argv: argv.iter().map(LosslessOsString::to_raw).collect(),
            },
            "captured command",
        )
    }

    pub fn executable(&self) -> &LosslessOsString {
        &self.executable
    }

    pub fn argv(&self) -> &[LosslessOsString] {
        &self.argv
    }

    fn from_raw(
        raw: RawCapturedCommand,
        field: impl Into<String>,
    ) -> Result<Self, SnapshotValidationError> {
        let field = field.into();
        let executable = LosslessOsString::from_raw(raw.executable, format!("{field}.executable"))?;
        if executable.as_bytes().is_empty() {
            return Err(SnapshotValidationError::EmptyExecutable { field });
        }
        if raw.argv.is_empty() {
            return Err(SnapshotValidationError::EmptyArgv { field });
        }
        if raw.argv.len() > MAX_ARGUMENTS {
            return Err(SnapshotValidationError::TooManyArguments {
                field,
                actual: raw.argv.len(),
                maximum: MAX_ARGUMENTS,
            });
        }

        let argv = raw
            .argv
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                LosslessOsString::from_raw(value, format!("{field}.argv[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if argv[0].as_bytes().is_empty() {
            return Err(SnapshotValidationError::EmptyArgvZero { field });
        }

        Ok(Self { executable, argv })
    }

    fn to_raw(&self) -> RawCapturedCommand {
        RawCapturedCommand {
            executable: self.executable.to_raw(),
            argv: self.argv.iter().map(LosslessOsString::to_raw).collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexSessionId(Uuid);

impl CodexSessionId {
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    pub(crate) fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedPromptText(String);

impl CapturedPromptText {
    pub(crate) fn try_new(text: String) -> Result<Self, SnapshotValidationError> {
        if text.is_empty() {
            return Err(SnapshotValidationError::InvalidCodexPromptText {
                reason: "text is empty".to_owned(),
            });
        }
        if text.trim().is_empty() {
            return Err(SnapshotValidationError::InvalidCodexPromptText {
                reason: "text contains only whitespace".to_owned(),
            });
        }
        if text.len() > MAX_CODEX_PROMPT_BYTES {
            return Err(SnapshotValidationError::InvalidCodexPromptText {
                reason: format!(
                    "text is {} bytes; the maximum is {MAX_CODEX_PROMPT_BYTES}",
                    text.len()
                ),
            });
        }
        if text
            .chars()
            .any(|character| character != '\n' && character.is_control())
        {
            return Err(SnapshotValidationError::InvalidCodexPromptText {
                reason: "text contains terminal control characters".to_owned(),
            });
        }
        Ok(Self(text))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn visible_row_count(&self) -> usize {
        self.0.split('\n').count()
    }

    pub fn byte_count(&self) -> usize {
        self.0.len()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedCodexPromptArea {
    text: CapturedPromptText,
}

impl CapturedCodexPromptArea {
    pub(crate) fn try_new(text: String) -> Result<Self, SnapshotValidationError> {
        Ok(Self {
            text: CapturedPromptText::try_new(text)?,
        })
    }

    pub fn text(&self) -> &CapturedPromptText {
        &self.text
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeSessionId(Uuid);

impl ClaudeSessionId {
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    pub(crate) fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecognizedMdBookServeCommand(CapturedCommand);

impl RecognizedMdBookServeCommand {
    pub fn command(&self) -> &CapturedCommand {
        &self.0
    }

    pub(crate) fn recognize(command: CapturedCommand) -> Result<Self, SnapshotValidationError> {
        validate_serve_command("mdbook", &command)?;
        Ok(Self(command))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecognizedBookshelfServeCommand(CapturedCommand);

impl RecognizedBookshelfServeCommand {
    pub fn command(&self) -> &CapturedCommand {
        &self.0
    }

    pub(crate) fn recognize(command: CapturedCommand) -> Result<Self, SnapshotValidationError> {
        validate_serve_command("book", &command)?;
        Ok(Self(command))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AutomaticRecovery {
    Codex {
        session_id: CodexSessionId,
        prompt_area: Option<CapturedCodexPromptArea>,
    },
    ClaudeCode {
        session_id: ClaudeSessionId,
    },
    MdBookServe {
        command: RecognizedMdBookServeCommand,
    },
    BookshelfServe {
        command: RecognizedBookshelfServeCommand,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureFailure(String);

impl CaptureFailure {
    pub fn try_new(message: impl Into<String>) -> Result<Self, SnapshotValidationError> {
        Self::from_raw(message.into(), "capture failure")
    }

    pub fn message(&self) -> &str {
        &self.0
    }

    fn from_raw(
        message: String,
        field: impl Into<String>,
    ) -> Result<Self, SnapshotValidationError> {
        let field = field.into();
        if message.is_empty() {
            return Err(SnapshotValidationError::InvalidCaptureFailure {
                field,
                reason: "message is empty".to_owned(),
            });
        }
        if message.len() > MAX_DIAGNOSTIC_BYTES {
            return Err(SnapshotValidationError::InvalidCaptureFailure {
                field,
                reason: format!(
                    "message is {} bytes; the maximum is {MAX_DIAGNOSTIC_BYTES}",
                    message.len()
                ),
            });
        }
        if message.chars().any(char::is_control) {
            return Err(SnapshotValidationError::InvalidCaptureFailure {
                field,
                reason: "message contains terminal control characters".to_owned(),
            });
        }
        Ok(Self(message))
    }
}

impl fmt::Display for CaptureFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PaneRecovery {
    Idle,
    Automatic(AutomaticRecovery),
    Manual(CapturedCommand),
    Unavailable(CaptureFailure),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneSnapshot {
    source_index: u32,
    working_directory: RecordedAbsolutePath,
    recovery: PaneRecovery,
}

impl PaneSnapshot {
    pub fn source_index(&self) -> u32 {
        self.source_index
    }

    pub fn working_directory(&self) -> &RecordedAbsolutePath {
        &self.working_directory
    }

    pub fn recovery(&self) -> &PaneRecovery {
        &self.recovery
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowSnapshot {
    source_index: u32,
    name: String,
    panes: Vec<PaneSnapshot>,
}

impl WindowSnapshot {
    pub fn source_index(&self) -> u32 {
        self.source_index
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn panes(&self) -> &[PaneSnapshot] {
        &self.panes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSnapshot {
    name: String,
    working_directory: RecordedAbsolutePath,
    windows: Vec<WindowSnapshot>,
}

impl SessionSnapshot {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn working_directory(&self) -> &RecordedAbsolutePath {
        &self.working_directory
    }

    pub fn windows(&self) -> &[WindowSnapshot] {
        &self.windows
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedSnapshot {
    captured_at: CaptureTime,
    source: SnapshotSource,
    consistency: CaptureConsistency,
    sessions: Vec<SessionSnapshot>,
}

impl ValidatedSnapshot {
    pub fn from_json(input: &[u8]) -> Result<Self, SnapshotValidationError> {
        if input.len() > MAX_SNAPSHOT_BYTES {
            return Err(SnapshotValidationError::SnapshotTooLarge {
                actual: input.len(),
                maximum: MAX_SNAPSHOT_BYTES,
            });
        }

        let raw: RawSnapshot = serde_json::from_slice(input)
            .map_err(|error| SnapshotValidationError::InvalidJson(error.to_string()))?;
        Self::refine(raw)
    }

    pub(crate) fn from_capture_raw(raw: RawSnapshot) -> Result<Self, SnapshotValidationError> {
        let encoded = serde_json::to_vec(&raw)
            .map_err(|error| SnapshotValidationError::InvalidJson(error.to_string()))?;
        if encoded.len() > MAX_SNAPSHOT_BYTES {
            return Err(SnapshotValidationError::SnapshotTooLarge {
                actual: encoded.len(),
                maximum: MAX_SNAPSHOT_BYTES,
            });
        }
        Self::refine(raw)
    }

    pub fn to_json_pretty(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(&RawSnapshot::from(self))
    }

    pub fn captured_at(&self) -> &CaptureTime {
        &self.captured_at
    }

    pub fn source(&self) -> &SnapshotSource {
        &self.source
    }

    pub fn consistency(&self) -> &CaptureConsistency {
        &self.consistency
    }

    pub fn sessions(&self) -> &[SessionSnapshot] {
        &self.sessions
    }

    fn refine(raw: RawSnapshot) -> Result<Self, SnapshotValidationError> {
        let captured_at = CaptureTime::parse(raw.captured_at)?;
        let source = SnapshotSource {
            path: RecordedAbsolutePath::from_raw(raw.source, "source")?,
        };
        let consistency = match raw.consistency {
            RawCaptureConsistency::Stable {} => CaptureConsistency::Stable,
            RawCaptureConsistency::Unstable { attempts }
                if attempts == MAX_TOPOLOGY_VALIDATION_ATTEMPTS =>
            {
                CaptureConsistency::Unstable {
                    attempts: ExhaustedAttemptCount(attempts),
                }
            }
            RawCaptureConsistency::Unstable { attempts } => {
                return Err(SnapshotValidationError::InvalidUnstableAttemptCount {
                    actual: attempts,
                    expected: MAX_TOPOLOGY_VALIDATION_ATTEMPTS,
                });
            }
        };

        if raw.sessions.is_empty() {
            return Err(SnapshotValidationError::EmptySessions);
        }
        if raw.sessions.len() > MAX_SESSIONS {
            return Err(SnapshotValidationError::TooManySessions {
                actual: raw.sessions.len(),
                maximum: MAX_SESSIONS,
            });
        }

        let mut session_names = HashSet::with_capacity(raw.sessions.len());
        let mut sessions = Vec::with_capacity(raw.sessions.len());
        for (session_position, raw_session) in raw.sessions.into_iter().enumerate() {
            validate_session_name(&raw_session.name, session_position)?;
            if !session_names.insert(raw_session.name.clone()) {
                return Err(SnapshotValidationError::DuplicateSessionName {
                    name: raw_session.name,
                });
            }
            sessions.push(validate_session(raw_session, session_position)?);
        }

        Ok(Self {
            captured_at,
            source,
            consistency,
            sessions,
        })
    }
}

fn validate_session(
    raw: RawSessionSnapshot,
    session_position: usize,
) -> Result<SessionSnapshot, SnapshotValidationError> {
    let session_name = raw.name;
    let working_directory = RecordedAbsolutePath::from_raw(
        raw.working_directory,
        format!("sessions[{session_position}].working_directory"),
    )?;
    if raw.windows.is_empty() {
        return Err(SnapshotValidationError::EmptyWindows {
            session: session_name,
        });
    }
    if raw.windows.len() > MAX_WINDOWS_PER_SESSION {
        return Err(SnapshotValidationError::TooManyWindows {
            session: session_name,
            actual: raw.windows.len(),
            maximum: MAX_WINDOWS_PER_SESSION,
        });
    }

    let mut indexes = HashSet::with_capacity(raw.windows.len());
    let mut windows = Vec::with_capacity(raw.windows.len());
    for (window_position, raw_window) in raw.windows.into_iter().enumerate() {
        if !indexes.insert(raw_window.source_index) {
            return Err(SnapshotValidationError::DuplicateWindowIndex {
                session: session_name,
                index: raw_window.source_index,
            });
        }
        windows.push(validate_window(
            raw_window,
            &session_name,
            session_position,
            window_position,
        )?);
    }

    Ok(SessionSnapshot {
        name: session_name,
        working_directory,
        windows,
    })
}

fn validate_window(
    raw: RawWindowSnapshot,
    session_name: &str,
    session_position: usize,
    window_position: usize,
) -> Result<WindowSnapshot, SnapshotValidationError> {
    validate_window_name(&raw.name, session_position, window_position)?;
    if raw.panes.is_empty() {
        return Err(SnapshotValidationError::EmptyPanes {
            session: session_name.to_owned(),
            window_index: raw.source_index,
        });
    }
    if raw.panes.len() > MAX_PANES_PER_WINDOW {
        return Err(SnapshotValidationError::TooManyPanes {
            session: session_name.to_owned(),
            window_index: raw.source_index,
            actual: raw.panes.len(),
            maximum: MAX_PANES_PER_WINDOW,
        });
    }

    let window_index = raw.source_index;
    let mut indexes = HashSet::with_capacity(raw.panes.len());
    let mut panes = Vec::with_capacity(raw.panes.len());
    for (pane_position, raw_pane) in raw.panes.into_iter().enumerate() {
        if !indexes.insert(raw_pane.source_index) {
            return Err(SnapshotValidationError::DuplicatePaneIndex {
                session: session_name.to_owned(),
                window_index,
                index: raw_pane.source_index,
            });
        }
        let field = format!(
            "sessions[{session_position}].windows[{window_position}].panes[{pane_position}]"
        );
        panes.push(PaneSnapshot {
            source_index: raw_pane.source_index,
            working_directory: RecordedAbsolutePath::from_raw(
                raw_pane.working_directory,
                format!("{field}.working_directory"),
            )?,
            recovery: validate_recovery(raw_pane.recovery, &field)?,
        });
    }

    Ok(WindowSnapshot {
        source_index: window_index,
        name: raw.name,
        panes,
    })
}

fn validate_recovery(
    raw: RawPaneRecovery,
    field: &str,
) -> Result<PaneRecovery, SnapshotValidationError> {
    match raw {
        RawPaneRecovery::Idle {} => Ok(PaneRecovery::Idle),
        RawPaneRecovery::Manual { command } => Ok(PaneRecovery::Manual(CapturedCommand::from_raw(
            command,
            format!("{field}.recovery.command"),
        )?)),
        RawPaneRecovery::Unavailable { failure } => Ok(PaneRecovery::Unavailable(
            CaptureFailure::from_raw(failure, format!("{field}.recovery.failure"))?,
        )),
        RawPaneRecovery::Automatic { recovery } => Ok(PaneRecovery::Automatic(validate_automatic(
            recovery, field,
        )?)),
    }
}

fn validate_automatic(
    raw: RawAutomaticRecovery,
    field: &str,
) -> Result<AutomaticRecovery, SnapshotValidationError> {
    match raw {
        RawAutomaticRecovery::Codex {
            session_id,
            prompt_area,
        } => {
            let parsed = parse_session_id("codex", &session_id)?;
            Ok(AutomaticRecovery::Codex {
                session_id: CodexSessionId(parsed),
                prompt_area: prompt_area
                    .map(|prompt_area| CapturedCodexPromptArea::try_new(prompt_area.text))
                    .transpose()?,
            })
        }
        RawAutomaticRecovery::ClaudeCode { session_id } => {
            let parsed = parse_session_id("claude_code", &session_id)?;
            Ok(AutomaticRecovery::ClaudeCode {
                session_id: ClaudeSessionId(parsed),
            })
        }
        RawAutomaticRecovery::MdBookServe { command } => {
            let command =
                CapturedCommand::from_raw(command, format!("{field}.recovery.automatic.command"))?;
            validate_serve_command("mdbook", &command)?;
            Ok(AutomaticRecovery::MdBookServe {
                command: RecognizedMdBookServeCommand(command),
            })
        }
        RawAutomaticRecovery::BookshelfServe { command } => {
            let command =
                CapturedCommand::from_raw(command, format!("{field}.recovery.automatic.command"))?;
            validate_serve_command("book", &command)?;
            Ok(AutomaticRecovery::BookshelfServe {
                command: RecognizedBookshelfServeCommand(command),
            })
        }
    }
}

fn parse_session_id(tool: &str, session_id: &str) -> Result<Uuid, SnapshotValidationError> {
    Uuid::parse_str(session_id).map_err(|_| SnapshotValidationError::InvalidSessionId {
        tool: tool.to_owned(),
        value: session_id.to_owned(),
    })
}

fn validate_serve_command(
    tool: &str,
    command: &CapturedCommand,
) -> Result<(), SnapshotValidationError> {
    let executable_basename = Path::new(command.executable.as_os_str())
        .file_name()
        .map(OsStrExt::as_bytes);
    let argv_zero_basename = Path::new(command.argv[0].as_os_str())
        .file_name()
        .map(OsStrExt::as_bytes);
    let expected = tool.as_bytes();
    if executable_basename != Some(expected)
        || argv_zero_basename != Some(expected)
        || command.argv.get(1).map(LosslessOsString::as_bytes) != Some(b"serve")
    {
        return Err(SnapshotValidationError::InvalidRecognizedServeCommand {
            tool: tool.to_owned(),
            reason: "executable basename, argv[0] basename, and argv[1] must identify serve"
                .to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn validate_session_name(
    name: &str,
    position: usize,
) -> Result<(), SnapshotValidationError> {
    if name.is_empty() {
        return Err(SnapshotValidationError::InvalidTmuxName {
            field: format!("sessions[{position}].name"),
            reason: "session name is empty".to_owned(),
        });
    }
    validate_name(name, format!("sessions[{position}].name"))?;
    if name.contains([':', '.']) {
        return Err(SnapshotValidationError::InvalidTmuxName {
            field: format!("sessions[{position}].name"),
            reason: "session name contains ':' or '.'".to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn validate_window_name(
    name: &str,
    session_position: usize,
    window_position: usize,
) -> Result<(), SnapshotValidationError> {
    validate_name(
        name,
        format!("sessions[{session_position}].windows[{window_position}].name"),
    )
}

fn validate_name(name: &str, field: String) -> Result<(), SnapshotValidationError> {
    if name.len() > MAX_NAME_BYTES {
        return Err(SnapshotValidationError::InvalidTmuxName {
            field,
            reason: format!(
                "name is {} bytes; the maximum is {MAX_NAME_BYTES}",
                name.len()
            ),
        });
    }
    if name.chars().any(char::is_control) {
        return Err(SnapshotValidationError::InvalidTmuxName {
            field,
            reason: "name contains control characters".to_owned(),
        });
    }
    Ok(())
}

impl From<&ValidatedSnapshot> for RawSnapshot {
    fn from(snapshot: &ValidatedSnapshot) -> Self {
        Self {
            captured_at: snapshot.captured_at.encoded.clone(),
            source: snapshot.source.path.to_raw(),
            consistency: match snapshot.consistency {
                CaptureConsistency::Stable => RawCaptureConsistency::Stable {},
                CaptureConsistency::Unstable { attempts } => RawCaptureConsistency::Unstable {
                    attempts: attempts.get(),
                },
            },
            sessions: snapshot
                .sessions
                .iter()
                .map(RawSessionSnapshot::from)
                .collect(),
        }
    }
}

impl From<&SessionSnapshot> for RawSessionSnapshot {
    fn from(session: &SessionSnapshot) -> Self {
        Self {
            name: session.name.clone(),
            working_directory: session.working_directory.to_raw(),
            windows: session
                .windows
                .iter()
                .map(RawWindowSnapshot::from)
                .collect(),
        }
    }
}

impl From<&WindowSnapshot> for RawWindowSnapshot {
    fn from(window: &WindowSnapshot) -> Self {
        Self {
            source_index: window.source_index,
            name: window.name.clone(),
            panes: window.panes.iter().map(RawPaneSnapshot::from).collect(),
        }
    }
}

impl From<&PaneSnapshot> for RawPaneSnapshot {
    fn from(pane: &PaneSnapshot) -> Self {
        Self {
            source_index: pane.source_index,
            working_directory: pane.working_directory.to_raw(),
            recovery: RawPaneRecovery::from(&pane.recovery),
        }
    }
}

impl From<&PaneRecovery> for RawPaneRecovery {
    fn from(recovery: &PaneRecovery) -> Self {
        match recovery {
            PaneRecovery::Idle => Self::Idle {},
            PaneRecovery::Manual(command) => Self::Manual {
                command: command.to_raw(),
            },
            PaneRecovery::Unavailable(failure) => Self::Unavailable {
                failure: failure.0.clone(),
            },
            PaneRecovery::Automatic(recovery) => Self::Automatic {
                recovery: RawAutomaticRecovery::from(recovery),
            },
        }
    }
}

impl From<&AutomaticRecovery> for RawAutomaticRecovery {
    fn from(recovery: &AutomaticRecovery) -> Self {
        match recovery {
            AutomaticRecovery::Codex {
                session_id,
                prompt_area,
            } => Self::Codex {
                session_id: session_id.0.to_string(),
                prompt_area: prompt_area
                    .as_ref()
                    .map(|prompt_area| RawCapturedCodexPromptArea {
                        text: prompt_area.text.as_str().to_owned(),
                    }),
            },
            AutomaticRecovery::ClaudeCode { session_id } => Self::ClaudeCode {
                session_id: session_id.0.to_string(),
            },
            AutomaticRecovery::MdBookServe { command } => Self::MdBookServe {
                command: command.0.to_raw(),
            },
            AutomaticRecovery::BookshelfServe { command } => Self::BookshelfServe {
                command: command.0.to_raw(),
            },
        }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SnapshotValidationError {
    #[error("snapshot is {actual} bytes; the maximum is {maximum}")]
    SnapshotTooLarge { actual: usize, maximum: usize },
    #[error("snapshot JSON is invalid: {0}")]
    InvalidJson(String),
    #[error("capture time {value:?} is not RFC 3339")]
    InvalidCaptureTime { value: String },
    #[error("snapshot contains no sessions")]
    EmptySessions,
    #[error("snapshot contains {actual} sessions; the maximum is {maximum}")]
    TooManySessions { actual: usize, maximum: usize },
    #[error("session name {name:?} is duplicated")]
    DuplicateSessionName { name: String },
    #[error("session {session:?} contains no windows")]
    EmptyWindows { session: String },
    #[error("session {session:?} contains {actual} windows; the maximum is {maximum}")]
    TooManyWindows {
        session: String,
        actual: usize,
        maximum: usize,
    },
    #[error("window index {index} is duplicated in session {session:?}")]
    DuplicateWindowIndex { session: String, index: u32 },
    #[error("window {window_index} in session {session:?} contains no panes")]
    EmptyPanes { session: String, window_index: u32 },
    #[error(
        "window {window_index} in session {session:?} contains {actual} panes; the maximum is {maximum}"
    )]
    TooManyPanes {
        session: String,
        window_index: u32,
        actual: usize,
        maximum: usize,
    },
    #[error("pane index {index} is duplicated in window {window_index} of session {session:?}")]
    DuplicatePaneIndex {
        session: String,
        window_index: u32,
        index: u32,
    },
    #[error("{field} is not an absolute path")]
    PathNotAbsolute { field: String },
    #[error("{field} has an invalid operating-system string encoding: {reason}")]
    InvalidOsEncoding { field: String, reason: String },
    #[error("{field} contains a NUL byte")]
    OsValueContainsNul { field: String },
    #[error("{field} is {actual} bytes; the maximum is {maximum}")]
    OsValueTooLong {
        field: String,
        actual: usize,
        maximum: usize,
    },
    #[error("{field} has an empty executable")]
    EmptyExecutable { field: String },
    #[error("{field} has no argv elements")]
    EmptyArgv { field: String },
    #[error("{field} has {actual} argv elements; the maximum is {maximum}")]
    TooManyArguments {
        field: String,
        actual: usize,
        maximum: usize,
    },
    #[error("{field} has an empty argv[0]")]
    EmptyArgvZero { field: String },
    #[error("unstable attempt count is {actual}; exhaustion requires {expected}")]
    InvalidUnstableAttemptCount { actual: usize, expected: usize },
    #[error("{tool} session ID {value:?} is invalid")]
    InvalidSessionId { tool: String, value: String },
    #[error("Codex prompt text is invalid: {reason}")]
    InvalidCodexPromptText { reason: String },
    #[error("recognized {tool} serve command is invalid: {reason}")]
    InvalidRecognizedServeCommand { tool: String, reason: String },
    #[error("{field} is invalid: {reason}")]
    InvalidCaptureFailure { field: String, reason: String },
    #[error("{field} is not a valid tmux name: {reason}")]
    InvalidTmuxName { field: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(value: String) -> RawEncodedOsString {
        RawEncodedOsString {
            encoding: "utf8".to_owned(),
            value,
        }
    }

    #[test]
    fn capture_refinement_enforces_the_aggregate_snapshot_limit() {
        let large_argument = "x".repeat(MAX_OS_VALUE_BYTES);
        let raw = RawSnapshot {
            captured_at: "2026-07-23T00:00:00Z".to_owned(),
            source: encoded("/tmp/source.sock".to_owned()),
            consistency: RawCaptureConsistency::Stable {},
            sessions: vec![RawSessionSnapshot {
                name: "work".to_owned(),
                working_directory: encoded("/tmp".to_owned()),
                windows: vec![RawWindowSnapshot {
                    source_index: 0,
                    name: "work".to_owned(),
                    panes: vec![RawPaneSnapshot {
                        source_index: 0,
                        working_directory: encoded("/tmp".to_owned()),
                        recovery: RawPaneRecovery::Manual {
                            command: RawCapturedCommand {
                                executable: encoded("/usr/bin/tool".to_owned()),
                                argv: std::iter::once(encoded("tool".to_owned()))
                                    .chain((0..17).map(|_| encoded(large_argument.clone())))
                                    .collect(),
                            },
                        },
                    }],
                }],
            }],
        };

        assert!(matches!(
            ValidatedSnapshot::from_capture_raw(raw),
            Err(SnapshotValidationError::SnapshotTooLarge { .. })
        ));
    }
}
