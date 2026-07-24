use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;
use uuid::Uuid;

use crate::{
    AutomaticRecovery, CapturedCommand, ClaudeSessionId, CodexSessionId, LosslessOsString,
    PaneRecovery, RecognizedBookshelfServeCommand, RecognizedMdBookServeCommand,
    RecordedAbsolutePath,
};

pub const MAX_TOOL_RECORD_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForegroundProcessMember {
    process_id: u32,
    parent_process_id: u32,
    process_group: u32,
    process_start_time: u64,
    process_tty: LosslessOsString,
    command: CapturedCommand,
    working_directory: RecordedAbsolutePath,
}

impl ForegroundProcessMember {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        process_id: u32,
        parent_process_id: u32,
        process_group: u32,
        process_start_time: u64,
        process_tty: LosslessOsString,
        command: CapturedCommand,
        working_directory: RecordedAbsolutePath,
    ) -> Result<Self, ForegroundEvidenceError> {
        if process_id == 0
            || parent_process_id == 0
            || process_group == 0
            || process_start_time == 0
        {
            return Err(ForegroundEvidenceError::ZeroProcessIdentity);
        }
        Ok(Self {
            process_id,
            parent_process_id,
            process_group,
            process_start_time,
            process_tty,
            command,
            working_directory,
        })
    }

    pub fn process_id(&self) -> u32 {
        self.process_id
    }

    pub fn parent_process_id(&self) -> u32 {
        self.parent_process_id
    }

    pub fn process_group(&self) -> u32 {
        self.process_group
    }

    pub fn process_start_time(&self) -> u64 {
        self.process_start_time
    }

    pub fn process_tty(&self) -> &LosslessOsString {
        &self.process_tty
    }

    pub fn command(&self) -> &CapturedCommand {
        &self.command
    }

    pub fn working_directory(&self) -> &RecordedAbsolutePath {
        &self.working_directory
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OpenedCodexSessionFile {
    holder_process_id: u32,
    device: u64,
    inode: u64,
    path: RecordedAbsolutePath,
    first_record: Vec<u8>,
}

impl OpenedCodexSessionFile {
    pub fn try_new(
        holder_process_id: u32,
        device: u64,
        inode: u64,
        path: RecordedAbsolutePath,
        first_record: Vec<u8>,
    ) -> Result<Self, ForegroundEvidenceError> {
        if holder_process_id == 0 {
            return Err(ForegroundEvidenceError::ZeroProcessIdentity);
        }
        if first_record.is_empty() || first_record.len() > MAX_TOOL_RECORD_BYTES {
            return Err(ForegroundEvidenceError::InvalidToolRecordSize {
                actual: first_record.len(),
                maximum: MAX_TOOL_RECORD_BYTES,
            });
        }
        Ok(Self {
            holder_process_id,
            device,
            inode,
            path,
            first_record,
        })
    }

    pub fn holder_process_id(&self) -> u32 {
        self.holder_process_id
    }

    pub fn path(&self) -> &RecordedAbsolutePath {
        &self.path
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CodexSessionEvidence {
    files: Vec<OpenedCodexSessionFile>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OpenedClaudeSessionFile {
    holder_process_id: u32,
    path: RecordedAbsolutePath,
    record: Vec<u8>,
}

impl OpenedClaudeSessionFile {
    pub fn try_new(
        holder_process_id: u32,
        path: RecordedAbsolutePath,
        record: Vec<u8>,
    ) -> Result<Self, ForegroundEvidenceError> {
        if holder_process_id == 0 {
            return Err(ForegroundEvidenceError::ZeroProcessIdentity);
        }
        if record.is_empty() || record.len() > MAX_TOOL_RECORD_BYTES {
            return Err(ForegroundEvidenceError::InvalidToolRecordSize {
                actual: record.len(),
                maximum: MAX_TOOL_RECORD_BYTES,
            });
        }
        Ok(Self {
            holder_process_id,
            path,
            record,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ClaudeSessionEvidence {
    files: Vec<OpenedClaudeSessionFile>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionTool {
    Codex,
    ClaudeCode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolAttributedTailSession {
    tool: SessionTool,
    session_id: Uuid,
}

impl ToolAttributedTailSession {
    pub fn try_new(tool: SessionTool, session_id: &str) -> Result<Self, ForegroundEvidenceError> {
        let session_id = Uuid::parse_str(session_id)
            .map_err(|_| ForegroundEvidenceError::InvalidTailSessionId)?;
        Ok(Self { tool, session_id })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneTiedForegroundEvidence {
    command: CapturedCommand,
    pane_working_directory: RecordedAbsolutePath,
    pane_tty: LosslessOsString,
    process_tty: LosslessOsString,
    foreground_process_group: u32,
    process_id: u32,
    process_group: u32,
    process_start_time: u64,
    members: Vec<ForegroundProcessMember>,
    codex_session_evidence: Option<CodexSessionEvidence>,
    claude_session_evidence: Option<ClaudeSessionEvidence>,
    tail_session: Option<ToolAttributedTailSession>,
}

impl PaneTiedForegroundEvidence {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        command: CapturedCommand,
        pane_working_directory: RecordedAbsolutePath,
        pane_tty: LosslessOsString,
        process_tty: LosslessOsString,
        foreground_process_group: u32,
        process_id: u32,
        process_group: u32,
        process_start_time: u64,
    ) -> Result<Self, ForegroundEvidenceError> {
        if foreground_process_group == 0
            || process_id == 0
            || process_group == 0
            || process_start_time == 0
        {
            return Err(ForegroundEvidenceError::ZeroProcessIdentity);
        }
        if pane_tty != process_tty {
            return Err(ForegroundEvidenceError::TtyMismatch);
        }
        if process_group != foreground_process_group {
            return Err(ForegroundEvidenceError::ProcessNotInForegroundGroup);
        }
        if process_id != process_group {
            return Err(ForegroundEvidenceError::ProcessIsNotGroupLeader);
        }

        Ok(Self {
            command,
            pane_working_directory,
            pane_tty,
            process_tty,
            foreground_process_group,
            process_id,
            process_group,
            process_start_time,
            members: Vec::new(),
            codex_session_evidence: None,
            claude_session_evidence: None,
            tail_session: None,
        })
    }

    pub fn with_foreground_members(
        mut self,
        members: Vec<ForegroundProcessMember>,
    ) -> Result<Self, ForegroundEvidenceError> {
        let mut parents = HashMap::with_capacity(members.len());
        for member in &members {
            if member.process_id == self.process_id
                || parents
                    .insert(member.process_id, member.parent_process_id)
                    .is_some()
            {
                return Err(ForegroundEvidenceError::DuplicateProcessIdentity(
                    member.process_id,
                ));
            }
            if member.process_group != self.foreground_process_group {
                return Err(ForegroundEvidenceError::ProcessNotInForegroundGroup);
            }
            if member.process_tty != self.process_tty {
                return Err(ForegroundEvidenceError::TtyMismatch);
            }
        }

        for member in &members {
            let mut current = member.process_id;
            let mut visited = HashSet::new();
            loop {
                if !visited.insert(current) {
                    return Err(ForegroundEvidenceError::ForegroundGroupIsNotRooted);
                }
                let parent = parents
                    .get(&current)
                    .copied()
                    .ok_or(ForegroundEvidenceError::ForegroundGroupIsNotRooted)?;
                if parent == self.process_id {
                    break;
                }
                if !parents.contains_key(&parent) {
                    return Err(ForegroundEvidenceError::ForegroundGroupIsNotRooted);
                }
                current = parent;
            }
        }

        self.members = members;
        Ok(self)
    }

    pub fn with_codex_session_evidence(
        mut self,
        store: RecordedAbsolutePath,
        files: Vec<OpenedCodexSessionFile>,
    ) -> Result<Self, ForegroundEvidenceError> {
        let group_processes = std::iter::once(self.process_id)
            .chain(self.members.iter().map(|member| member.process_id))
            .collect::<HashSet<_>>();
        for file in &files {
            if !group_processes.contains(&file.holder_process_id) {
                return Err(ForegroundEvidenceError::ToolRecordNotHeldByForegroundGroup);
            }
            let path = Path::new(file.path.as_os_str());
            let store_path = Path::new(store.as_os_str());
            if path == store_path
                || !path.starts_with(store_path)
                || path.extension() != Some(OsStr::new("jsonl"))
            {
                return Err(ForegroundEvidenceError::ToolRecordOutsideStore);
            }
        }
        self.codex_session_evidence = Some(CodexSessionEvidence { files });
        Ok(self)
    }

    pub fn with_claude_session_evidence(
        mut self,
        store: RecordedAbsolutePath,
        files: Vec<OpenedClaudeSessionFile>,
    ) -> Result<Self, ForegroundEvidenceError> {
        let group_processes = std::iter::once(self.process_id)
            .chain(self.members.iter().map(|member| member.process_id))
            .collect::<HashSet<_>>();
        for file in &files {
            if !group_processes.contains(&file.holder_process_id) {
                return Err(ForegroundEvidenceError::ToolRecordNotHeldByForegroundGroup);
            }
            let path = Path::new(file.path.as_os_str());
            let store_path = Path::new(store.as_os_str());
            let expected_name = format!("{}.json", file.holder_process_id);
            if path == store_path
                || !path.starts_with(store_path)
                || path.file_name().map(OsStrExt::as_bytes) != Some(expected_name.as_bytes())
            {
                return Err(ForegroundEvidenceError::ToolRecordOutsideStore);
            }
        }
        self.claude_session_evidence = Some(ClaudeSessionEvidence { files });
        Ok(self)
    }

    pub fn with_tail_session(mut self, tail_session: ToolAttributedTailSession) -> Self {
        self.tail_session = Some(tail_session);
        self
    }

    pub fn command(&self) -> &CapturedCommand {
        &self.command
    }

    pub fn pane_working_directory(&self) -> &RecordedAbsolutePath {
        &self.pane_working_directory
    }

    pub fn pane_tty(&self) -> &LosslessOsString {
        &self.pane_tty
    }

    pub fn process_tty(&self) -> &LosslessOsString {
        &self.process_tty
    }

    pub fn foreground_process_group(&self) -> u32 {
        self.foreground_process_group
    }

    pub fn process_id(&self) -> u32 {
        self.process_id
    }

    pub fn process_group(&self) -> u32 {
        self.process_group
    }

    pub fn process_start_time(&self) -> u64 {
        self.process_start_time
    }

    pub fn members(&self) -> &[ForegroundProcessMember] {
        &self.members
    }

    fn process_commands(&self) -> impl Iterator<Item = (u32, &CapturedCommand)> {
        std::iter::once((self.process_id, &self.command)).chain(
            self.members
                .iter()
                .map(|member| (member.process_id, &member.command)),
        )
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ForegroundEvidenceError {
    #[error("process identity values must be nonzero")]
    ZeroProcessIdentity,
    #[error("process {0} appears more than once in the foreground group")]
    DuplicateProcessIdentity(u32),
    #[error("the process terminal does not match the pane terminal")]
    TtyMismatch,
    #[error("the process does not belong to the pane's foreground process group")]
    ProcessNotInForegroundGroup,
    #[error("the selected foreground process is not the process-group leader")]
    ProcessIsNotGroupLeader,
    #[error("the foreground process group is not one tree rooted at its leader")]
    ForegroundGroupIsNotRooted,
    #[error("tool record is {actual} bytes; expected 1..={maximum}")]
    InvalidToolRecordSize { actual: usize, maximum: usize },
    #[error("tool record is not held open by the pane's foreground process group")]
    ToolRecordNotHeldByForegroundGroup,
    #[error("tool record is outside the configured session store")]
    ToolRecordOutsideStore,
    #[error("tool-attributed tail session ID is not a UUID")]
    InvalidTailSessionId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverFailure {
    Insufficient(String),
    Conflicting(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolverOutcome {
    Automatic(AutomaticRecovery),
    NotRecognized,
    InsufficientEvidence(ResolverFailure),
    ConflictingEvidence(ResolverFailure),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneClassification {
    recovery: PaneRecovery,
    resolver_outcome: ResolverOutcome,
}

impl PaneClassification {
    pub fn recovery(&self) -> &PaneRecovery {
        &self.recovery
    }

    pub fn resolver_outcome(&self) -> &ResolverOutcome {
        &self.resolver_outcome
    }

    pub fn into_recovery(self) -> PaneRecovery {
        self.recovery
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryCommand {
    argv: Vec<LosslessOsString>,
}

impl RecoveryCommand {
    pub fn argv(&self) -> &[LosslessOsString] {
        &self.argv
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AutomaticRecoveryExpectation {
    Codex(CodexSessionId),
    ClaudeCode(ClaudeSessionId),
    MdBookServe(RecognizedMdBookServeCommand),
    BookshelfServe(RecognizedBookshelfServeCommand),
}

impl From<&AutomaticRecovery> for AutomaticRecoveryExpectation {
    fn from(recovery: &AutomaticRecovery) -> Self {
        match recovery {
            AutomaticRecovery::Codex { session_id } => Self::Codex(session_id.clone()),
            AutomaticRecovery::ClaudeCode { session_id } => Self::ClaudeCode(session_id.clone()),
            AutomaticRecovery::MdBookServe { command } => Self::MdBookServe(command.clone()),
            AutomaticRecovery::BookshelfServe { command } => Self::BookshelfServe(command.clone()),
        }
    }
}

impl AutomaticRecoveryExpectation {
    pub fn matches(&self, actual: &AutomaticRecovery) -> bool {
        match (self, actual) {
            (Self::Codex(expected), AutomaticRecovery::Codex { session_id }) => {
                expected == session_id
            }
            (Self::ClaudeCode(expected), AutomaticRecovery::ClaudeCode { session_id }) => {
                expected == session_id
            }
            (Self::MdBookServe(expected), AutomaticRecovery::MdBookServe { command }) => {
                same_serve_argv(expected.command().argv(), command.command().argv())
            }
            (Self::BookshelfServe(expected), AutomaticRecovery::BookshelfServe { command }) => {
                same_serve_argv(expected.command().argv(), command.command().argv())
            }
            _ => false,
        }
    }
}

fn same_serve_argv(expected: &[LosslessOsString], actual: &[LosslessOsString]) -> bool {
    expected.len() == actual.len()
        && basename(expected[0].as_bytes()) == basename(actual[0].as_bytes())
        && expected[1..] == actual[1..]
}

pub fn derive_automatic_command(recovery: &AutomaticRecovery) -> RecoveryCommand {
    let argv = match recovery {
        AutomaticRecovery::Codex { session_id } => vec![
            fixed_os_string(b"codex"),
            fixed_os_string(b"resume"),
            fixed_os_string(session_id.as_uuid().to_string().as_bytes()),
        ],
        AutomaticRecovery::ClaudeCode { session_id } => vec![
            fixed_os_string(b"claude"),
            fixed_os_string(b"--resume"),
            fixed_os_string(session_id.as_uuid().to_string().as_bytes()),
        ],
        AutomaticRecovery::MdBookServe { command } => std::iter::once(fixed_os_string(b"mdbook"))
            .chain(command.command().argv()[1..].iter().cloned())
            .collect(),
        AutomaticRecovery::BookshelfServe { command } => std::iter::once(fixed_os_string(b"book"))
            .chain(command.command().argv()[1..].iter().cloned())
            .collect(),
    };
    RecoveryCommand { argv }
}

fn fixed_os_string(bytes: &[u8]) -> LosslessOsString {
    LosslessOsString::try_from_bytes(bytes.to_vec())
        .expect("fixed automatic-recovery arguments satisfy OS-string invariants")
}

pub fn classify_pane(evidence: PaneTiedForegroundEvidence) -> PaneClassification {
    let mut outcomes = [
        resolve_codex(&evidence),
        resolve_claude(&evidence),
        resolve_serve(&evidence),
    ]
    .into_iter()
    .flatten();
    let outcome = match (outcomes.next(), outcomes.next()) {
        (None, _) => ResolverOutcome::NotRecognized,
        (Some(outcome), None) => outcome,
        (Some(_), Some(_)) => conflicting(
            "foreground process group matches multiple automatic-recovery tool families",
        ),
    };
    let outcome = apply_tail_conflict(outcome, evidence.tail_session.as_ref());
    let recovery = match &outcome {
        ResolverOutcome::Automatic(automatic) => PaneRecovery::Automatic(automatic.clone()),
        ResolverOutcome::NotRecognized
        | ResolverOutcome::InsufficientEvidence(_)
        | ResolverOutcome::ConflictingEvidence(_) => PaneRecovery::Manual(evidence.command.clone()),
    };
    PaneClassification {
        recovery,
        resolver_outcome: outcome,
    }
}

fn resolve_serve(evidence: &PaneTiedForegroundEvidence) -> Option<ResolverOutcome> {
    RecognizedMdBookServeCommand::recognize(evidence.command.clone())
        .map(|command| ResolverOutcome::Automatic(AutomaticRecovery::MdBookServe { command }))
        .or_else(|_| {
            RecognizedBookshelfServeCommand::recognize(evidence.command.clone()).map(|command| {
                ResolverOutcome::Automatic(AutomaticRecovery::BookshelfServe { command })
            })
        })
        .ok()
}

fn resolve_codex(evidence: &PaneTiedForegroundEvidence) -> Option<ResolverOutcome> {
    let codex_processes = evidence
        .process_commands()
        .filter_map(|(process_id, command)| is_codex_tui(command).then_some(process_id))
        .collect::<HashSet<_>>();
    if codex_processes.is_empty() {
        return None;
    }
    let Some(session_evidence) = &evidence.codex_session_evidence else {
        return Some(insufficient("Codex has no opened session-file evidence"));
    };

    let pane_cwd = std::str::from_utf8(evidence.pane_working_directory.as_bytes()).ok();
    let Some(pane_cwd) = pane_cwd else {
        return Some(insufficient(
            "Codex JSON metadata cannot match a non-UTF-8 pane directory",
        ));
    };

    let mut file_identities = HashSet::new();
    let mut session_ids = HashSet::new();
    let mut matching_candidates = 0;
    for file in &session_evidence.files {
        if !codex_processes.contains(&file.holder_process_id) {
            continue;
        }
        if !file_identities.insert((file.device, file.inode)) {
            continue;
        }
        let record: CodexSessionMeta = match serde_json::from_slice(&file.first_record) {
            Ok(record) => record,
            Err(error) => {
                return Some(insufficient(format!(
                    "Codex session metadata is unparseable: {error}"
                )));
            }
        };
        if record.kind != "session_meta"
            || record.payload.originator != "codex-tui"
            || record.payload.thread_source != "user"
            || record.payload.cwd != pane_cwd
            || record.payload.parent_thread_id.is_some()
        {
            continue;
        }
        let session_id = match Uuid::parse_str(&record.payload.id) {
            Ok(session_id) => session_id,
            Err(_) => return Some(insufficient("Codex payload.id is not a UUID")),
        };
        matching_candidates += 1;
        session_ids.insert(session_id);
    }

    match matching_candidates {
        0 => Some(insufficient(
            "Codex has no exact root session matching the pane directory",
        )),
        1 => {
            let session_id = *session_ids.iter().next().expect("set length is one");
            Some(ResolverOutcome::Automatic(AutomaticRecovery::Codex {
                session_id: CodexSessionId::from_uuid(session_id),
            }))
        }
        _ => Some(conflicting(
            "Codex has multiple exact root session candidates",
        )),
    }
}

#[derive(Deserialize)]
struct CodexSessionMeta {
    #[serde(rename = "type")]
    kind: String,
    payload: CodexSessionMetaPayload,
}

#[derive(Deserialize)]
struct CodexSessionMetaPayload {
    id: String,
    originator: String,
    thread_source: String,
    cwd: String,
    #[serde(default)]
    parent_thread_id: Option<String>,
}

fn is_codex_tui(command: &CapturedCommand) -> bool {
    let executable_matches = basename(command.executable().as_bytes()) == Some(b"codex");
    let argv_zero_matches = command
        .argv()
        .first()
        .and_then(|value| basename(value.as_bytes()))
        == Some(b"codex");
    let noninteractive = command
        .argv()
        .iter()
        .skip(1)
        .any(|argument| matches!(argument.as_bytes(), b"app-server" | b"exec" | b"mcp-server"));
    executable_matches && argv_zero_matches && !noninteractive
}

fn resolve_claude(evidence: &PaneTiedForegroundEvidence) -> Option<ResolverOutcome> {
    let commands = evidence
        .process_commands()
        .filter(|(_, command)| is_claude_tui(command))
        .collect::<Vec<_>>();
    if commands.is_empty() {
        return None;
    }

    let mut saw_identity_flag = false;
    let mut invalid_identity = false;
    let claude_processes = commands
        .iter()
        .map(|(process_id, _)| *process_id)
        .collect::<HashSet<_>>();
    let mut argv_candidates = Vec::new();
    for (_, command) in commands {
        let argv = command.argv();
        let mut index = 1;
        while index < argv.len() {
            let argument = argv[index].as_bytes();
            if argument == b"--" {
                break;
            }
            let separate = matches!(argument, b"--session-id" | b"--resume" | b"-r");
            if separate {
                saw_identity_flag = true;
                let parsed = argv.get(index + 1).and_then(|value| {
                    std::str::from_utf8(value.as_bytes())
                        .ok()
                        .and_then(|value| Uuid::parse_str(value).ok())
                });
                match parsed {
                    Some(session_id) => {
                        argv_candidates.push(session_id);
                    }
                    None => invalid_identity = true,
                }
                index += 2;
                continue;
            }

            let equals_value = argument
                .strip_prefix(b"--session-id=")
                .or_else(|| argument.strip_prefix(b"--resume="));
            if let Some(value) = equals_value {
                saw_identity_flag = true;
                match std::str::from_utf8(value)
                    .ok()
                    .and_then(|value| Uuid::parse_str(value).ok())
                {
                    Some(session_id) => {
                        argv_candidates.push(session_id);
                    }
                    None => invalid_identity = true,
                }
            }
            index += 1;
        }
    }

    let mut invalid_record = false;
    let mut record_candidates = Vec::new();
    if let Some(session_evidence) = &evidence.claude_session_evidence {
        for file in &session_evidence.files {
            if !claude_processes.contains(&file.holder_process_id) {
                continue;
            }
            let record: ClaudeSessionRecord = match serde_json::from_slice(&file.record) {
                Ok(record) => record,
                Err(_) => {
                    invalid_record = true;
                    continue;
                }
            };
            let expected_start = evidence.process_start_time_for(file.holder_process_id);
            let parsed_start = record.proc_start.parse::<u64>().ok();
            let parsed_session = Uuid::parse_str(&record.session_id).ok();
            if record.pid != file.holder_process_id
                || record.kind != "interactive"
                || record.cwd.as_bytes() != evidence.pane_working_directory.as_bytes()
                || parsed_start != expected_start
                || parsed_session.is_none()
                || !record.transport.matches_pane(evidence.pane_tty())
            {
                invalid_record = true;
                continue;
            }
            record_candidates.push(parsed_session.expect("checked as present"));
        }
    }

    if argv_candidates.len() > 1 || record_candidates.len() > 1 {
        return Some(conflicting(
            "Claude evidence contains multiple exact session candidates",
        ));
    }
    if invalid_identity || invalid_record {
        return Some(insufficient(
            "Claude evidence is incomplete, stale, or malformed",
        ));
    }
    let argv_candidate = argv_candidates.into_iter().next();
    let record_candidate = record_candidates.into_iter().next();
    if let (Some(argv), Some(record)) = (argv_candidate, record_candidate)
        && argv != record
    {
        return Some(conflicting(
            "Claude argv and process record identify different sessions",
        ));
    }
    if let Some(session_id) = argv_candidate.or(record_candidate) {
        return Some(ResolverOutcome::Automatic(AutomaticRecovery::ClaudeCode {
            session_id: ClaudeSessionId::from_uuid(session_id),
        }));
    }
    if saw_identity_flag {
        return Some(insufficient(
            "Claude identity flag does not contain an exact UUID",
        ));
    }
    Some(insufficient(
        "Claude has no exact argv or interactive process-record session UUID",
    ))
}

#[derive(Deserialize)]
struct ClaudeSessionRecord {
    pid: u32,
    #[serde(rename = "sessionId")]
    session_id: String,
    cwd: String,
    #[serde(rename = "procStart")]
    proc_start: String,
    kind: String,
    transport: ClaudeTransportRecord,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ClaudeTransportRecord {
    Pty { identity: String },
}

impl ClaudeTransportRecord {
    fn matches_pane(&self, pane_tty: &LosslessOsString) -> bool {
        match self {
            Self::Pty { identity } => identity.as_bytes() == pane_tty.as_bytes(),
        }
    }
}

impl PaneTiedForegroundEvidence {
    fn process_start_time_for(&self, process_id: u32) -> Option<u64> {
        if process_id == self.process_id {
            return Some(self.process_start_time);
        }
        self.members
            .iter()
            .find(|member| member.process_id == process_id)
            .map(|member| member.process_start_time)
    }
}

fn apply_tail_conflict(
    outcome: ResolverOutcome,
    tail: Option<&ToolAttributedTailSession>,
) -> ResolverOutcome {
    let Some(tail) = tail else {
        return outcome;
    };
    let resolved = match &outcome {
        ResolverOutcome::Automatic(AutomaticRecovery::Codex { session_id })
            if tail.tool == SessionTool::Codex =>
        {
            Some(session_id.as_uuid())
        }
        ResolverOutcome::Automatic(AutomaticRecovery::ClaudeCode { session_id })
            if tail.tool == SessionTool::ClaudeCode =>
        {
            Some(session_id.as_uuid())
        }
        _ => None,
    };
    match resolved {
        Some(resolved) if resolved != tail.session_id => {
            conflicting("same-tool pane tail contains a different exact session ID")
        }
        _ => outcome,
    }
}

fn is_claude_tui(command: &CapturedCommand) -> bool {
    let executable_matches = is_claude_executable(command.executable().as_bytes());
    let argv_zero_matches = command
        .argv()
        .first()
        .and_then(|value| basename(value.as_bytes()))
        == Some(b"claude");
    executable_matches && argv_zero_matches && supported_claude_tui_arguments(command.argv())
}

#[derive(Clone, Copy)]
enum ClaudeOptionArity {
    Flag,
    RequiredValue,
    OptionalValue,
    VariadicValue,
}

fn supported_claude_tui_arguments(argv: &[LosslessOsString]) -> bool {
    let mut index = 1;
    while index < argv.len() {
        let argument = argv[index].as_bytes();
        if argument == b"--" || !argument.starts_with(b"-") {
            return false;
        }
        let (name, inline_value) = argument
            .iter()
            .position(|byte| *byte == b'=')
            .map_or((argument, None), |separator| {
                (&argument[..separator], Some(&argument[separator + 1..]))
            });
        let Some(arity) = supported_claude_option(name) else {
            return false;
        };
        if let Some(value) = inline_value {
            if value.is_empty() || matches!(arity, ClaudeOptionArity::Flag) {
                return false;
            }
            index += 1;
            continue;
        }
        match arity {
            ClaudeOptionArity::Flag => index += 1,
            ClaudeOptionArity::RequiredValue => {
                if argv.get(index + 1).is_none() {
                    return false;
                }
                index += 2;
            }
            ClaudeOptionArity::OptionalValue => {
                index += 1;
                if argv
                    .get(index)
                    .is_some_and(|value| !value.as_bytes().starts_with(b"-"))
                {
                    index += 1;
                }
            }
            ClaudeOptionArity::VariadicValue => {
                index += 1;
                let first_value = index;
                while argv
                    .get(index)
                    .is_some_and(|value| !value.as_bytes().starts_with(b"-"))
                {
                    index += 1;
                }
                if index == first_value {
                    return false;
                }
            }
        }
    }
    true
}

fn supported_claude_option(name: &[u8]) -> Option<ClaudeOptionArity> {
    use ClaudeOptionArity::{Flag, OptionalValue, RequiredValue, VariadicValue};

    match name {
        b"--allow-dangerously-skip-permissions"
        | b"--ax-screen-reader"
        | b"--bare"
        | b"--brief"
        | b"--chrome"
        | b"-c"
        | b"--continue"
        | b"--dangerously-skip-permissions"
        | b"--disable-slash-commands"
        | b"--exclude-dynamic-system-prompt-sections"
        | b"--ide"
        | b"--no-chrome"
        | b"--safe-mode"
        | b"--strict-mcp-config"
        | b"--verbose" => Some(Flag),
        b"--agent"
        | b"--agents"
        | b"--append-system-prompt"
        | b"--debug-file"
        | b"--effort"
        | b"--fallback-model"
        | b"--input-format"
        | b"--json-schema"
        | b"--max-budget-usd"
        | b"--model"
        | b"-n"
        | b"--name"
        | b"--output-format"
        | b"--permission-mode"
        | b"--plugin-dir"
        | b"--plugin-url"
        | b"--remote-control-session-name-prefix"
        | b"--session-id"
        | b"--setting-sources"
        | b"--settings"
        | b"--system-prompt"
        | b"--tools" => Some(RequiredValue),
        b"-d"
        | b"--debug"
        | b"--from-pr"
        | b"--prompt-suggestions"
        | b"--remote-control"
        | b"-r"
        | b"--resume" => Some(OptionalValue),
        b"--add-dir"
        | b"--allowedTools"
        | b"--allowed-tools"
        | b"--betas"
        | b"--disallowedTools"
        | b"--disallowed-tools"
        | b"--file"
        | b"--mcp-config" => Some(VariadicValue),
        _ => None,
    }
}

fn is_claude_executable(value: &[u8]) -> bool {
    if basename(value) == Some(b"claude") {
        return true;
    }
    let components = Path::new(OsStr::from_bytes(value))
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.as_bytes()),
            _ => None,
        })
        .collect::<Vec<_>>();
    matches!(
        components.as_slice(),
        [.., claude, versions, version]
            if *claude == b"claude" && *versions == b"versions" && !version.is_empty()
    )
}

fn basename(value: &[u8]) -> Option<&[u8]> {
    Path::new(OsStr::from_bytes(value))
        .file_name()
        .map(OsStrExt::as_bytes)
}

fn insufficient(reason: impl Into<String>) -> ResolverOutcome {
    ResolverOutcome::InsufficientEvidence(ResolverFailure::Insufficient(reason.into()))
}

fn conflicting(reason: impl Into<String>) -> ResolverOutcome {
    ResolverOutcome::ConflictingEvidence(ResolverFailure::Conflicting(reason.into()))
}
