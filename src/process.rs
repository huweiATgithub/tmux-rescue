use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read};
use std::num::NonZeroU64;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{
    CapturedCommand, ForegroundProcessMember, LosslessOsString, MAX_OS_VALUE_BYTES,
    MAX_TOOL_RECORD_BYTES, OpenedClaudeSessionFile, OpenedCodexSessionFile, PaneInitialProcess,
    PaneProcessAnchor, PaneProcessObservation, PaneTiedForegroundEvidence, RecordedAbsolutePath,
    TopologyPane,
};

pub const MAX_CMDLINE_BYTES: usize = MAX_OS_VALUE_BYTES;
const MAX_PROC_STAT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ObservedProcessCommand {
    command: CapturedCommand,
    executable: ObservedExecutable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcessExecutableKey {
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ObservedExecutable {
    UnpinnedRaw,
    PinnedLinked {
        key: ProcessExecutableKey,
        link_count: NonZeroU64,
    },
    PinnedUnlinked {
        key: ProcessExecutableKey,
        identity_path: LosslessOsString,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PinnedExecutableObservation {
    key: ProcessExecutableKey,
    link_count: u64,
    raw_link: LosslessOsString,
}

impl ObservedProcessCommand {
    pub(crate) fn unpinned(command: CapturedCommand) -> Self {
        Self {
            command,
            executable: ObservedExecutable::UnpinnedRaw,
        }
    }

    fn from_pinned(
        executable: PinnedExecutableObservation,
        argv: Vec<LosslessOsString>,
    ) -> Result<Self, ProcessInspectionFailure> {
        let command = CapturedCommand::try_new(executable.raw_link.clone(), argv)
            .map_err(|error| ProcessInspectionFailure::InvalidCommandValue(error.to_string()))?;
        let executable = match NonZeroU64::new(executable.link_count) {
            Some(link_count) => ObservedExecutable::PinnedLinked {
                key: executable.key,
                link_count,
            },
            None => {
                let Some(identity_path) =
                    executable.raw_link.as_bytes().strip_suffix(b" (deleted)")
                else {
                    return Err(ProcessInspectionFailure::InvalidEvidence(
                        "unlinked executable is missing the kernel deletion decoration".to_owned(),
                    ));
                };
                if identity_path.is_empty() {
                    return Err(ProcessInspectionFailure::InvalidEvidence(
                        "unlinked executable identity is empty".to_owned(),
                    ));
                }
                let identity_path = LosslessOsString::try_from_bytes(identity_path.to_vec())
                    .map_err(|error| {
                        ProcessInspectionFailure::InvalidEvidence(error.to_string())
                    })?;
                ObservedExecutable::PinnedUnlinked {
                    key: executable.key,
                    identity_path,
                }
            }
        };
        Ok(Self {
            command,
            executable,
        })
    }

    pub(crate) fn command(&self) -> &CapturedCommand {
        &self.command
    }

    pub(crate) fn executable_identity_basename(&self) -> Option<&[u8]> {
        let identity_path = match &self.executable {
            ObservedExecutable::UnpinnedRaw | ObservedExecutable::PinnedLinked { .. } => {
                self.command.executable()
            }
            ObservedExecutable::PinnedUnlinked { identity_path, .. } => identity_path,
        };
        Path::new(identity_path.as_os_str())
            .file_name()
            .map(OsStrExt::as_bytes)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessStat {
    process_id: u32,
    state: u8,
    parent_process_id: u32,
    process_group: u32,
    session_id: u32,
    tty_device: i64,
    foreground_process_group: i32,
    start_time: u64,
}

impl ProcessStat {
    pub fn process_id(&self) -> u32 {
        self.process_id
    }

    pub fn state(&self) -> u8 {
        self.state
    }

    pub fn parent_process_id(&self) -> u32 {
        self.parent_process_id
    }

    pub fn process_group(&self) -> u32 {
        self.process_group
    }

    pub fn session_id(&self) -> u32 {
        self.session_id
    }

    pub fn tty_device(&self) -> i64 {
        self.tty_device
    }

    pub fn foreground_process_group(&self) -> i32 {
        self.foreground_process_group
    }

    pub fn start_time(&self) -> u64 {
        self.start_time
    }
}

pub fn parse_proc_stat(
    expected_process_id: u32,
    input: &[u8],
) -> Result<ProcessStat, ProcessInspectionFailure> {
    let open = input
        .iter()
        .position(|byte| *byte == b'(')
        .ok_or_else(|| malformed_stat(expected_process_id, "missing comm opening parenthesis"))?;
    let close = input
        .iter()
        .rposition(|byte| *byte == b')')
        .filter(|close| *close > open)
        .ok_or_else(|| malformed_stat(expected_process_id, "missing comm closing parenthesis"))?;

    let parsed_process_id = parse_number::<u32>(
        trim_ascii(&input[..open]),
        expected_process_id,
        "process ID",
    )?;
    if parsed_process_id != expected_process_id {
        return Err(ProcessInspectionFailure::ProcessIdMismatch {
            expected: expected_process_id,
            actual: parsed_process_id,
        });
    }

    let fields = input[close + 1..]
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    if fields.len() < 20 {
        return Err(malformed_stat(
            expected_process_id,
            "fewer than 22 process-stat fields",
        ));
    }
    if fields[0].len() != 1 {
        return Err(malformed_stat(expected_process_id, "invalid process state"));
    }

    Ok(ProcessStat {
        process_id: parsed_process_id,
        state: fields[0][0],
        parent_process_id: parse_number(fields[1], expected_process_id, "parent process ID")?,
        process_group: parse_number(fields[2], expected_process_id, "process group")?,
        session_id: parse_number(fields[3], expected_process_id, "session ID")?,
        tty_device: parse_number(fields[4], expected_process_id, "tty device")?,
        foreground_process_group: parse_number(
            fields[5],
            expected_process_id,
            "foreground process group",
        )?,
        start_time: parse_number(fields[19], expected_process_id, "process start time")?,
    })
}

pub fn parse_proc_cmdline(input: &[u8]) -> Result<Vec<LosslessOsString>, ProcessInspectionFailure> {
    if input.len() > MAX_CMDLINE_BYTES {
        return Err(ProcessInspectionFailure::CmdlineTooLarge {
            actual: input.len(),
            maximum: MAX_CMDLINE_BYTES,
        });
    }
    if input.last() != Some(&0) {
        return Err(ProcessInspectionFailure::CmdlineMissingFinalNul);
    }

    let argv = input[..input.len() - 1]
        .split(|byte| *byte == 0)
        .map(|argument| {
            LosslessOsString::try_from_bytes(argument.to_vec())
                .map_err(|error| ProcessInspectionFailure::InvalidCommandValue(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if argv
        .first()
        .is_none_or(|argument| argument.as_bytes().is_empty())
    {
        return Err(ProcessInspectionFailure::EmptyArgvZero);
    }
    Ok(argv)
}

pub fn select_foreground_processes(
    pane: &ProcessStat,
    processes: Vec<ProcessStat>,
) -> Result<Vec<ProcessStat>, ProcessInspectionFailure> {
    if pane.tty_device == 0 {
        return Err(ProcessInspectionFailure::PaneHasNoTty);
    }
    let foreground_group = u32::try_from(pane.foreground_process_group)
        .map_err(|_| ProcessInspectionFailure::NoForegroundProcessGroup)?;
    if foreground_group == 0 {
        return Err(ProcessInspectionFailure::NoForegroundProcessGroup);
    }

    let mut selected = processes
        .into_iter()
        .filter(|process| {
            process.process_group == foreground_group
                && process.session_id == pane.session_id
                && process.tty_device == pane.tty_device
                && !matches!(process.state, b'Z' | b'X' | b'x')
        })
        .collect::<Vec<_>>();
    let Some(leader) = selected
        .iter()
        .find(|process| process.process_id == foreground_group)
    else {
        return Err(ProcessInspectionFailure::ForegroundLeaderGone);
    };
    if leader.process_group != foreground_group {
        return Err(ProcessInspectionFailure::ForegroundLeaderGone);
    }

    let parents = selected
        .iter()
        .map(|process| (process.process_id, process.parent_process_id))
        .collect::<HashMap<_, _>>();
    for process in &selected {
        if process.process_id == foreground_group {
            continue;
        }
        let mut current = process.process_id;
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(current) {
                return Err(ProcessInspectionFailure::AmbiguousForegroundJob);
            }
            let Some(parent) = parents.get(&current).copied() else {
                return Err(ProcessInspectionFailure::AmbiguousForegroundJob);
            };
            if parent == foreground_group {
                break;
            }
            if !parents.contains_key(&parent) {
                return Err(ProcessInspectionFailure::AmbiguousForegroundJob);
            }
            current = parent;
        }
    }

    selected.sort_by_key(|process| process.process_id);
    Ok(selected)
}

pub trait PaneProcessProbe {
    fn observe(
        &self,
        pane: &TopologyPane,
    ) -> Result<PaneProcessObservation, ProcessInspectionFailure>;
}

#[derive(Clone, Debug)]
pub struct LinuxProcessInspector {
    proc_root: PathBuf,
    codex_sessions: Option<RecordedAbsolutePath>,
    claude_sessions: Option<RecordedAbsolutePath>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ToolRecordObservation<T> {
    Disabled,
    Unavailable,
    Available(Vec<T>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ToolEvidenceObservation {
    codex: ToolRecordObservation<OpenedCodexSessionFile>,
    claude: ToolRecordObservation<OpenedClaudeSessionFile>,
}

impl LinuxProcessInspector {
    pub fn new() -> Self {
        Self {
            proc_root: PathBuf::from("/proc"),
            codex_sessions: default_tool_store("CODEX_HOME", ".codex"),
            claude_sessions: default_tool_store("CLAUDE_CONFIG_DIR", ".claude"),
        }
    }

    pub fn with_proc_root(proc_root: PathBuf) -> Self {
        Self {
            proc_root,
            codex_sessions: None,
            claude_sessions: None,
        }
    }

    pub fn with_proc_root_and_tool_stores(
        proc_root: PathBuf,
        codex_sessions: Option<RecordedAbsolutePath>,
        claude_sessions: Option<RecordedAbsolutePath>,
    ) -> Self {
        Self {
            proc_root,
            codex_sessions,
            claude_sessions,
        }
    }

    fn read_stat(&self, process_id: u32) -> Result<ProcessStat, ProcessInspectionFailure> {
        let path = self.proc_root.join(process_id.to_string()).join("stat");
        let bytes = read_bounded(&path, MAX_PROC_STAT_BYTES, "read process stat", process_id)?;
        parse_proc_stat(process_id, &bytes)
    }

    fn read_pane_tty(&self, process_id: u32) -> Result<LosslessOsString, ProcessInspectionFailure> {
        let path = self.proc_root.join(process_id.to_string()).join("fd/0");
        let target = fs::read_link(path).map_err(|error| ProcessInspectionFailure::Io {
            operation: "read pane process terminal".to_owned(),
            process_id: Some(process_id),
            reason: error.to_string(),
        })?;
        LosslessOsString::try_from_bytes(target.into_os_string().into_vec())
            .map_err(|error| ProcessInspectionFailure::InvalidEvidence(error.to_string()))
    }

    fn enumerate_process_stats(&self) -> Result<Vec<ProcessStat>, ProcessInspectionFailure> {
        let entries =
            fs::read_dir(&self.proc_root).map_err(|error| ProcessInspectionFailure::Io {
                operation: "enumerate procfs".to_owned(),
                process_id: None,
                reason: error.to_string(),
            })?;
        let mut processes = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| ProcessInspectionFailure::Io {
                operation: "read procfs directory entry".to_owned(),
                process_id: None,
                reason: error.to_string(),
            })?;
            let name = entry.file_name().into_vec();
            let Ok(name) = std::str::from_utf8(&name) else {
                continue;
            };
            let Ok(process_id) = name.parse::<u32>() else {
                continue;
            };
            match self.read_stat(process_id) {
                Ok(stat) => processes.push(stat),
                Err(ProcessInspectionFailure::Io { .. }) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(processes)
    }

    fn read_process_stably(
        &self,
        process_id: u32,
    ) -> Result<InspectedProcess, ProcessInspectionFailure> {
        let first = self.read_process_once(process_id)?;
        let second = self.read_process_once(process_id)?;
        if !first.same_observation(&second) {
            return Err(ProcessInspectionFailure::ObservationRaced { process_id });
        }
        Ok(second)
    }

    fn read_process_once(
        &self,
        process_id: u32,
    ) -> Result<InspectedProcess, ProcessInspectionFailure> {
        let process_root = self.proc_root.join(process_id.to_string());
        let stat = self.read_stat(process_id)?;
        if matches!(stat.state, b'Z' | b'X' | b'x') {
            return Err(ProcessInspectionFailure::ProcessNotLive { process_id });
        }

        let executable_link = process_root.join("exe");
        let first_executable = read_pinned_executable(&executable_link, process_id)?;
        let cmdline = read_bounded(
            &process_root.join("cmdline"),
            MAX_CMDLINE_BYTES,
            "read process cmdline",
            process_id,
        )?;
        let argv = parse_proc_cmdline(&cmdline)?;
        let cwd = fs::read_link(process_root.join("cwd")).map_err(|error| {
            ProcessInspectionFailure::Io {
                operation: "read process working directory".to_owned(),
                process_id: Some(process_id),
                reason: error.to_string(),
            }
        })?;
        let working_directory = RecordedAbsolutePath::try_from_bytes(
            cwd.into_os_string().into_vec(),
        )
        .map_err(|error| ProcessInspectionFailure::InvalidCommandValue(error.to_string()))?;
        let executable = read_pinned_executable(&executable_link, process_id)?;
        if first_executable != executable {
            return Err(ProcessInspectionFailure::ObservationRaced { process_id });
        }
        let command = ObservedProcessCommand::from_pinned(executable, argv)?;
        let after = self.read_stat(process_id)?;
        if !same_process_identity_and_job(&stat, &after) {
            return Err(ProcessInspectionFailure::ObservationRaced { process_id });
        }

        Ok(InspectedProcess {
            stat: after,
            command,
            working_directory,
        })
    }

    fn collect_codex_session_files(
        &self,
        process_ids: &[u32],
        store: &RecordedAbsolutePath,
    ) -> Result<Vec<OpenedCodexSessionFile>, ProcessInspectionFailure> {
        let store_path = Path::new(store.as_os_str());
        let mut files = Vec::new();
        for process_id in process_ids {
            let fd_root = self.proc_root.join(process_id.to_string()).join("fd");
            let entries = fs::read_dir(&fd_root).map_err(|error| ProcessInspectionFailure::Io {
                operation: "enumerate process file descriptors".to_owned(),
                process_id: Some(*process_id),
                reason: error.to_string(),
            })?;
            for entry in entries {
                let entry = entry.map_err(|error| ProcessInspectionFailure::Io {
                    operation: "read process file descriptor".to_owned(),
                    process_id: Some(*process_id),
                    reason: error.to_string(),
                })?;
                let Ok(file) = open_process_fd(&entry.path()) else {
                    continue;
                };
                let Ok(target) = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
                else {
                    continue;
                };
                if !target.starts_with(store_path)
                    || target.extension().and_then(|value| value.to_str()) != Some("jsonl")
                {
                    continue;
                }
                let path =
                    RecordedAbsolutePath::try_from_bytes(target.as_os_str().as_bytes().to_vec())
                        .map_err(|error| {
                            ProcessInspectionFailure::InvalidEvidence(error.to_string())
                        })?;
                let metadata = file
                    .metadata()
                    .map_err(|error| ProcessInspectionFailure::Io {
                        operation: "stat opened Codex session file".to_owned(),
                        process_id: Some(*process_id),
                        reason: error.to_string(),
                    })?;
                let first_record = read_first_record(file, *process_id)?;
                files.push(
                    OpenedCodexSessionFile::try_new(
                        *process_id,
                        metadata.dev(),
                        metadata.ino(),
                        path,
                        first_record,
                    )
                    .map_err(|error| {
                        ProcessInspectionFailure::InvalidEvidence(error.to_string())
                    })?,
                );
            }
        }
        files.sort();
        Ok(files)
    }

    fn collect_claude_session_files(
        &self,
        process_ids: &[u32],
        store: &RecordedAbsolutePath,
    ) -> Result<Vec<OpenedClaudeSessionFile>, ProcessInspectionFailure> {
        let mut files = Vec::new();
        for process_id in process_ids {
            let path = Path::new(store.as_os_str()).join(format!("{process_id}.json"));
            let file = match open_tool_record(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(ProcessInspectionFailure::Io {
                        operation: "open Claude session record".to_owned(),
                        process_id: Some(*process_id),
                        reason: error.to_string(),
                    });
                }
            };
            let record = read_open_file_bounded(
                file,
                MAX_TOOL_RECORD_BYTES,
                "read Claude session record",
                *process_id,
            )?;
            let path = RecordedAbsolutePath::try_from_bytes(path.as_os_str().as_bytes().to_vec())
                .map_err(|error| {
                ProcessInspectionFailure::InvalidEvidence(error.to_string())
            })?;
            files.push(
                OpenedClaudeSessionFile::try_new(*process_id, path, record).map_err(|error| {
                    ProcessInspectionFailure::InvalidEvidence(error.to_string())
                })?,
            );
        }
        files.sort();
        Ok(files)
    }

    fn collect_tool_evidence(&self, process_ids: &[u32]) -> ToolEvidenceObservation {
        let codex = match &self.codex_sessions {
            None => ToolRecordObservation::Disabled,
            Some(store) => match self.collect_codex_session_files(process_ids, store) {
                Ok(files) => ToolRecordObservation::Available(files),
                Err(_) => ToolRecordObservation::Unavailable,
            },
        };
        let claude = match &self.claude_sessions {
            None => ToolRecordObservation::Disabled,
            Some(store) => match self.collect_claude_session_files(process_ids, store) {
                Ok(files) => ToolRecordObservation::Available(files),
                Err(_) => ToolRecordObservation::Unavailable,
            },
        };
        ToolEvidenceObservation { codex, claude }
    }

    fn reobserve_foreground_group(
        &self,
        anchor: &PaneProcessAnchor,
        baseline_pane: &ProcessStat,
    ) -> Result<Vec<InspectedProcess>, ProcessInspectionFailure> {
        if self.read_pane_tty(anchor.pane_pid())? != *anchor.pane_tty() {
            return Err(ProcessInspectionFailure::PaneTtyMismatch {
                process_id: anchor.pane_pid(),
            });
        }
        let pane_before = self.read_stat(anchor.pane_pid())?;
        if !same_process_identity_and_job(baseline_pane, &pane_before) {
            return Err(ProcessInspectionFailure::ObservationRaced {
                process_id: anchor.pane_pid(),
            });
        }
        let selected = select_foreground_processes(&pane_before, self.enumerate_process_stats()?)?;
        let observations = selected
            .iter()
            .map(|process| self.read_process_stably(process.process_id))
            .collect::<Result<Vec<_>, _>>()?;
        let pane_after = self.read_stat(anchor.pane_pid())?;
        if !same_process_identity_and_job(&pane_before, &pane_after) {
            return Err(ProcessInspectionFailure::ObservationRaced {
                process_id: anchor.pane_pid(),
            });
        }
        if self.read_pane_tty(anchor.pane_pid())? != *anchor.pane_tty() {
            return Err(ProcessInspectionFailure::PaneTtyMismatch {
                process_id: anchor.pane_pid(),
            });
        }
        let foreground_group = u32::try_from(pane_after.foreground_process_group)
            .map_err(|_| ProcessInspectionFailure::NoForegroundProcessGroup)?;
        for process in &observations {
            if process.stat.process_group != foreground_group
                || process.stat.session_id != pane_after.session_id
                || process.stat.tty_device != pane_after.tty_device
            {
                return Err(ProcessInspectionFailure::ObservationRaced {
                    process_id: process.stat.process_id,
                });
            }
        }
        Ok(observations)
    }
}

impl Default for LinuxProcessInspector {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneProcessProbe for LinuxProcessInspector {
    fn observe(
        &self,
        pane: &TopologyPane,
    ) -> Result<PaneProcessObservation, ProcessInspectionFailure> {
        let anchor = pane.process_anchor();
        if self.read_pane_tty(anchor.pane_pid())? != *anchor.pane_tty() {
            return Err(ProcessInspectionFailure::PaneTtyMismatch {
                process_id: anchor.pane_pid(),
            });
        }
        let pane_before = self.read_stat(anchor.pane_pid())?;
        let selected = select_foreground_processes(&pane_before, self.enumerate_process_stats()?)?;
        let mut inspected = selected
            .iter()
            .map(|process| self.read_process_stably(process.process_id))
            .collect::<Result<Vec<_>, _>>()?;
        let inspected_baseline = inspected.clone();
        let pane_after = self.read_stat(anchor.pane_pid())?;
        if !same_process_identity_and_job(&pane_before, &pane_after) {
            return Err(ProcessInspectionFailure::ObservationRaced {
                process_id: anchor.pane_pid(),
            });
        }
        if self.read_pane_tty(anchor.pane_pid())? != *anchor.pane_tty() {
            return Err(ProcessInspectionFailure::PaneTtyMismatch {
                process_id: anchor.pane_pid(),
            });
        }
        let foreground_group = u32::try_from(pane_after.foreground_process_group)
            .map_err(|_| ProcessInspectionFailure::NoForegroundProcessGroup)?;
        for process in &inspected {
            if process.stat.process_group != foreground_group
                || process.stat.session_id != pane_after.session_id
                || process.stat.tty_device != pane_after.tty_device
            {
                return Err(ProcessInspectionFailure::ObservationRaced {
                    process_id: process.stat.process_id,
                });
            }
        }

        let leader_position = inspected
            .iter()
            .position(|process| process.stat.process_id == foreground_group)
            .ok_or(ProcessInspectionFailure::ForegroundLeaderGone)?;
        let leader = inspected.remove(leader_position);
        if leader.working_directory != *pane.working_directory() {
            return Err(ProcessInspectionFailure::PaneWorkingDirectoryMismatch {
                process_id: leader.stat.process_id,
            });
        }
        let pane_process_is_idle = match anchor.initial_process() {
            PaneInitialProcess::DefaultShell { executable } => {
                executable == leader.command.command().executable()
            }
            PaneInitialProcess::ExplicitCommand => {
                is_conservatively_interactive_shell(leader.command.command())
            }
        };
        if inspected.is_empty()
            && leader.stat.process_id == anchor.pane_pid()
            && pane_process_is_idle
        {
            let final_observations = self.reobserve_foreground_group(anchor, &pane_after)?;
            if !same_process_observations(&inspected_baseline, &final_observations) {
                return Err(ProcessInspectionFailure::ObservationRaced {
                    process_id: leader.stat.process_id,
                });
            }
            return Ok(PaneProcessObservation::Idle);
        }

        let process_ids = std::iter::once(leader.stat.process_id)
            .chain(inspected.iter().map(|process| process.stat.process_id))
            .collect::<Vec<_>>();
        let tool_evidence = self.collect_tool_evidence(&process_ids);
        let mut evidence = PaneTiedForegroundEvidence::try_new_observed(
            leader.command,
            pane.working_directory().clone(),
            anchor.pane_tty().clone(),
            anchor.pane_tty().clone(),
            foreground_group,
            leader.stat.process_id,
            leader.stat.process_group,
            leader.stat.start_time,
        )
        .map_err(|error| ProcessInspectionFailure::InvalidEvidence(error.to_string()))?;
        let members = inspected
            .into_iter()
            .map(|process| {
                ForegroundProcessMember::try_new_observed(
                    process.stat.process_id,
                    process.stat.parent_process_id,
                    process.stat.process_group,
                    process.stat.start_time,
                    anchor.pane_tty().clone(),
                    process.command,
                    process.working_directory,
                )
                .map_err(|error| ProcessInspectionFailure::InvalidEvidence(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        evidence = evidence
            .with_foreground_members(members)
            .map_err(|error| ProcessInspectionFailure::InvalidEvidence(error.to_string()))?;
        if let Some(store) = &self.codex_sessions
            && let ToolRecordObservation::Available(files) = &tool_evidence.codex
            && !files.is_empty()
            && let Ok(refined) = evidence
                .clone()
                .with_codex_session_evidence(store.clone(), files.clone())
        {
            evidence = refined;
        }
        if let Some(store) = &self.claude_sessions
            && let ToolRecordObservation::Available(files) = &tool_evidence.claude
            && !files.is_empty()
            && let Ok(refined) = evidence
                .clone()
                .with_claude_session_evidence(store.clone(), files.clone())
        {
            evidence = refined;
        }
        let final_observations = self.reobserve_foreground_group(anchor, &pane_after)?;
        if !same_process_observations(&inspected_baseline, &final_observations) {
            let process_id = inspected_baseline
                .iter()
                .zip(&final_observations)
                .find(|(before, after)| !before.same_observation(after))
                .map(|(process, _)| process.stat.process_id)
                .unwrap_or(anchor.pane_pid());
            return Err(ProcessInspectionFailure::ObservationRaced { process_id });
        }
        ensure_tool_evidence_unchanged(&tool_evidence, &self.collect_tool_evidence(&process_ids))?;
        Ok(PaneProcessObservation::Foreground(Box::new(evidence)))
    }
}

fn read_pinned_executable(
    executable_link: &Path,
    process_id: u32,
) -> Result<PinnedExecutableObservation, ProcessInspectionFailure> {
    let executable = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
        .open(executable_link)
        .map_err(|error| ProcessInspectionFailure::Io {
            operation: "open process executable".to_owned(),
            process_id: Some(process_id),
            reason: error.to_string(),
        })?;
    let before = executable
        .metadata()
        .map_err(|error| ProcessInspectionFailure::Io {
            operation: "stat pinned process executable".to_owned(),
            process_id: Some(process_id),
            reason: error.to_string(),
        })?;
    let raw_link =
        fs::read_link(format!("/proc/self/fd/{}", executable.as_raw_fd())).map_err(|error| {
            ProcessInspectionFailure::Io {
                operation: "read pinned process executable".to_owned(),
                process_id: Some(process_id),
                reason: error.to_string(),
            }
        })?;
    let after = executable
        .metadata()
        .map_err(|error| ProcessInspectionFailure::Io {
            operation: "stat pinned process executable".to_owned(),
            process_id: Some(process_id),
            reason: error.to_string(),
        })?;
    if before.dev() != after.dev() || before.ino() != after.ino() || before.nlink() != after.nlink()
    {
        return Err(ProcessInspectionFailure::ObservationRaced { process_id });
    }
    let raw_link = LosslessOsString::try_from_bytes(raw_link.into_os_string().into_vec())
        .map_err(|error| ProcessInspectionFailure::InvalidEvidence(error.to_string()))?;
    Ok(PinnedExecutableObservation {
        key: ProcessExecutableKey {
            device: before.dev(),
            inode: before.ino(),
        },
        link_count: before.nlink(),
        raw_link,
    })
}

fn ensure_tool_evidence_unchanged(
    before: &ToolEvidenceObservation,
    after: &ToolEvidenceObservation,
) -> Result<(), ProcessInspectionFailure> {
    if before == after {
        Ok(())
    } else {
        Err(ProcessInspectionFailure::ToolEvidenceRaced)
    }
}

fn is_conservatively_interactive_shell(command: &CapturedCommand) -> bool {
    fn supported_shell(value: &LosslessOsString) -> bool {
        Path::new(value.as_os_str())
            .file_name()
            .map(OsStrExt::as_bytes)
            .map(|name| name.strip_prefix(b"-").unwrap_or(name))
            .is_some_and(|name| {
                matches!(
                    name,
                    b"sh" | b"bash" | b"dash" | b"zsh" | b"ksh" | b"mksh" | b"ash"
                )
            })
    }

    supported_shell(command.executable())
        && command.argv().first().is_some_and(supported_shell)
        && command.argv()[1..].iter().all(|argument| {
            matches!(
                argument.as_bytes(),
                b"-i" | b"-l" | b"-il" | b"-li" | b"--login"
            )
        })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InspectedProcess {
    stat: ProcessStat,
    command: ObservedProcessCommand,
    working_directory: RecordedAbsolutePath,
}

impl InspectedProcess {
    fn same_observation(&self, other: &Self) -> bool {
        same_process_identity_and_job(&self.stat, &other.stat)
            && self.command == other.command
            && self.working_directory == other.working_directory
    }
}

fn same_process_observations(left: &[InspectedProcess], right: &[InspectedProcess]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.same_observation(right))
}

fn same_process_identity_and_job(left: &ProcessStat, right: &ProcessStat) -> bool {
    left.process_id == right.process_id
        && left.parent_process_id == right.parent_process_id
        && left.process_group == right.process_group
        && left.session_id == right.session_id
        && left.tty_device == right.tty_device
        && left.foreground_process_group == right.foreground_process_group
        && left.start_time == right.start_time
        && !matches!(right.state, b'Z' | b'X' | b'x')
}

fn read_bounded(
    path: &Path,
    maximum: usize,
    operation: &str,
    process_id: u32,
) -> Result<Vec<u8>, ProcessInspectionFailure> {
    let file = File::open(path).map_err(|error| ProcessInspectionFailure::Io {
        operation: operation.to_owned(),
        process_id: Some(process_id),
        reason: error.to_string(),
    })?;
    read_open_file_bounded(file, maximum, operation, process_id)
}

fn read_open_file_bounded(
    file: File,
    maximum: usize,
    operation: &str,
    process_id: u32,
) -> Result<Vec<u8>, ProcessInspectionFailure> {
    let mut bytes = Vec::new();
    file.take((maximum + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| ProcessInspectionFailure::Io {
            operation: operation.to_owned(),
            process_id: Some(process_id),
            reason: error.to_string(),
        })?;
    if bytes.len() > maximum {
        return Err(ProcessInspectionFailure::ReadLimitExceeded {
            operation: operation.to_owned(),
            process_id,
            actual: bytes.len(),
            maximum,
        });
    }
    Ok(bytes)
}

fn read_first_record(file: File, process_id: u32) -> Result<Vec<u8>, ProcessInspectionFailure> {
    let mut reader = BufReader::new(file).take((MAX_TOOL_RECORD_BYTES + 1) as u64);
    let mut record = Vec::new();
    reader
        .read_until(b'\n', &mut record)
        .map_err(|error| ProcessInspectionFailure::Io {
            operation: "read Codex session metadata".to_owned(),
            process_id: Some(process_id),
            reason: error.to_string(),
        })?;
    if record.len() > MAX_TOOL_RECORD_BYTES {
        return Err(ProcessInspectionFailure::ReadLimitExceeded {
            operation: "read Codex session metadata".to_owned(),
            process_id,
            actual: record.len(),
            maximum: MAX_TOOL_RECORD_BYTES,
        });
    }
    Ok(record)
}

fn open_process_fd(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

fn open_tool_record(path: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "tool record is not a regular file",
        ));
    }
    Ok(file)
}

fn default_tool_store(environment: &str, default_directory: &str) -> Option<RecordedAbsolutePath> {
    let root = match std::env::var_os(environment) {
        Some(root) => PathBuf::from(root),
        None => PathBuf::from(std::env::var_os("HOME")?).join(default_directory),
    };
    if !root.is_absolute() {
        return None;
    }
    let sessions = root.join("sessions");
    RecordedAbsolutePath::try_from_bytes(sessions.into_os_string().into_vec()).ok()
}

#[cfg(test)]
mod observation_tests {
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    use super::{
        CapturedCommand, InspectedProcess, LosslessOsString, ObservedProcessCommand,
        OpenedClaudeSessionFile, PinnedExecutableObservation, ProcessExecutableKey,
        ProcessInspectionFailure, ProcessStat, RecordedAbsolutePath, ToolEvidenceObservation,
        ToolRecordObservation, ensure_tool_evidence_unchanged, read_pinned_executable,
        same_process_observations,
    };

    struct ChildGuard(Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn observed_command(
        raw_link: &[u8],
        device: u64,
        inode: u64,
        link_count: u64,
    ) -> Result<ObservedProcessCommand, ProcessInspectionFailure> {
        ObservedProcessCommand::from_pinned(
            PinnedExecutableObservation {
                key: ProcessExecutableKey { device, inode },
                link_count,
                raw_link: LosslessOsString::try_from_bytes(raw_link.to_vec()).unwrap(),
            },
            vec![LosslessOsString::try_from_bytes(b"codex".to_vec()).unwrap()],
        )
    }

    #[test]
    fn pinned_executable_identity_accepts_only_a_proved_kernel_deletion_decoration() {
        for (raw_link, link_count, expected_identity) in [
            (b"/tmp/codex".as_slice(), 1, Some(b"codex".as_slice())),
            (
                b"/tmp/codex (deleted)".as_slice(),
                1,
                Some(b"codex (deleted)".as_slice()),
            ),
            (
                b"/tmp/codex (deleted)".as_slice(),
                0,
                Some(b"codex".as_slice()),
            ),
            (
                b"/tmp/codex (deleted) (deleted)".as_slice(),
                0,
                Some(b"codex (deleted)".as_slice()),
            ),
            (b"/tmp/codex".as_slice(), 0, None),
            (b" (deleted)".as_slice(), 0, None),
        ] {
            let result = observed_command(raw_link, 8, 42, link_count);
            match expected_identity {
                Some(expected_identity) => {
                    let command = result.unwrap();
                    assert_eq!(command.command().executable().as_bytes(), raw_link);
                    assert_eq!(
                        command.executable_identity_basename(),
                        Some(expected_identity)
                    );
                }
                None => assert!(matches!(
                    result,
                    Err(ProcessInspectionFailure::InvalidEvidence(_))
                )),
            }
        }
    }

    #[test]
    fn production_acquisition_refines_a_running_unlinked_executable() {
        let temporary = tempfile::tempdir().unwrap();
        let executable = temporary.path().join("codex");
        fs::copy("/bin/sleep", &executable).unwrap();
        let child = ChildGuard(Command::new(&executable).arg("30").spawn().unwrap());
        let process_id = child.0.id();
        let process_executable = Path::new("/proc").join(process_id.to_string()).join("exe");
        let deadline = Instant::now() + Duration::from_secs(1);
        let exec_confirmation = loop {
            let observation = match fs::read_link(&process_executable) {
                Ok(link) if link == executable => break Ok(()),
                Ok(link) => format!("link {}", link.display()),
                Err(error) => format!("I/O error {error}"),
            };
            if Instant::now() >= deadline {
                break Err(observation);
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if let Err(last_observation) = exec_confirmation {
            panic!("child {process_id} did not exec {executable:?}: {last_observation}");
        }

        let before_unlink = fs::metadata(&executable).unwrap();
        fs::remove_file(&executable).unwrap();

        let acquired = read_pinned_executable(&process_executable, process_id).unwrap();
        assert!(acquired.raw_link.as_bytes().ends_with(b"/codex (deleted)"));
        assert_eq!(acquired.key.device, before_unlink.dev());
        assert_eq!(acquired.key.inode, before_unlink.ino());
        assert_eq!(acquired.link_count, 0);

        let command = ObservedProcessCommand::from_pinned(
            acquired,
            vec![LosslessOsString::try_from_bytes(b"codex".to_vec()).unwrap()],
        )
        .unwrap();
        assert_eq!(
            command.executable_identity_basename(),
            Some(b"codex".as_slice())
        );
    }

    fn observed(executable: &[u8]) -> InspectedProcess {
        InspectedProcess {
            stat: ProcessStat {
                process_id: 42,
                state: b'S',
                parent_process_id: 1,
                process_group: 42,
                session_id: 42,
                tty_device: 3,
                foreground_process_group: 42,
                start_time: 99,
            },
            command: ObservedProcessCommand::unpinned(
                CapturedCommand::try_new(
                    LosslessOsString::try_from_bytes(executable.to_vec()).unwrap(),
                    vec![LosslessOsString::try_from_bytes(executable.to_vec()).unwrap()],
                )
                .unwrap(),
            ),
            working_directory: RecordedAbsolutePath::try_from_bytes(b"/tmp".to_vec()).unwrap(),
        }
    }

    fn pinned_observed(
        raw_link: &[u8],
        device: u64,
        inode: u64,
        link_count: u64,
    ) -> InspectedProcess {
        let mut process = observed(b"/tmp/codex");
        process.command = observed_command(raw_link, device, inode, link_count).unwrap();
        process
    }

    #[test]
    fn final_process_fence_rejects_exec_with_the_same_pid_and_start_time() {
        assert!(!same_process_observations(
            &[observed(b"/usr/bin/claude")],
            &[observed(b"/usr/bin/unrelated")],
        ));
    }

    #[test]
    fn final_process_fence_rejects_a_new_foreground_member() {
        let original = observed(b"/usr/bin/claude");
        let mut joined = observed(b"/usr/bin/helper");
        joined.stat.process_id = 43;
        joined.stat.parent_process_id = 42;

        assert!(!same_process_observations(
            std::slice::from_ref(&original),
            &[original.clone(), joined],
        ));
    }

    #[test]
    fn final_process_fence_rejects_pinned_executable_observation_mutations() {
        let original = pinned_observed(b"/tmp/codex", 8, 42, 1);

        for replacement in [
            pinned_observed(b"/tmp/codex", 8, 42, 2),
            pinned_observed(b"/tmp/renamed-codex", 8, 42, 1),
            pinned_observed(b"/tmp/codex", 9, 43, 1),
        ] {
            assert_ne!(original.command, replacement.command);
            assert!(!same_process_observations(
                std::slice::from_ref(&original),
                std::slice::from_ref(&replacement),
            ));
        }
    }

    #[test]
    fn final_tool_fence_rejects_a_changed_session_record() {
        let record = |session_id: &str| {
            OpenedClaudeSessionFile::try_new(
                42,
                RecordedAbsolutePath::try_from_bytes(b"/tmp/42.json".to_vec()).unwrap(),
                format!(r#"{{"sessionId":"{session_id}"}}"#).into_bytes(),
            )
            .unwrap()
        };
        let before = ToolEvidenceObservation {
            codex: ToolRecordObservation::Disabled,
            claude: ToolRecordObservation::Available(vec![record(
                "27ea5a6d-5b84-4770-998e-a1a8285b0e9a",
            )]),
        };
        let after = ToolEvidenceObservation {
            codex: ToolRecordObservation::Disabled,
            claude: ToolRecordObservation::Available(vec![record(
                "ffca1353-ce6a-4d7a-9fe5-a89062b5542c",
            )]),
        };

        assert_eq!(
            ensure_tool_evidence_unchanged(&before, &after),
            Err(super::ProcessInspectionFailure::ToolEvidenceRaced)
        );
    }
}

fn trim_ascii(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
}

fn parse_number<T>(
    value: &[u8],
    process_id: u32,
    field: &str,
) -> Result<T, ProcessInspectionFailure>
where
    T: std::str::FromStr,
{
    let value = std::str::from_utf8(value)
        .map_err(|_| malformed_stat(process_id, format!("{field} is not ASCII")))?;
    value
        .parse()
        .map_err(|_| malformed_stat(process_id, format!("{field} is not numeric")))
}

fn malformed_stat(process_id: u32, reason: impl Into<String>) -> ProcessInspectionFailure {
    ProcessInspectionFailure::MalformedProcStat {
        process_id,
        reason: reason.into(),
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ProcessInspectionFailure {
    #[error("/proc/{process_id}/stat is malformed: {reason}")]
    MalformedProcStat { process_id: u32, reason: String },
    #[error("process stat PID changed: expected {expected}, read {actual}")]
    ProcessIdMismatch { expected: u32, actual: u32 },
    #[error("pane process has no controlling tty")]
    PaneHasNoTty,
    #[error("pane process {process_id} is not bound to the tmux pane terminal")]
    PaneTtyMismatch { process_id: u32 },
    #[error("foreground process {process_id} is outside the recorded pane working directory")]
    PaneWorkingDirectoryMismatch { process_id: u32 },
    #[error("pane has no positive foreground process group")]
    NoForegroundProcessGroup,
    #[error("foreground process-group leader is absent")]
    ForegroundLeaderGone,
    #[error("foreground job is not one process tree rooted at its group leader")]
    AmbiguousForegroundJob,
    #[error("process cmdline is {actual} bytes; the maximum is {maximum}")]
    CmdlineTooLarge { actual: usize, maximum: usize },
    #[error("process cmdline does not end in NUL")]
    CmdlineMissingFinalNul,
    #[error("process cmdline has an empty argv[0]")]
    EmptyArgvZero,
    #[error("process command contains an invalid OS value: {0}")]
    InvalidCommandValue(String),
    #[error("process {process_id} is no longer live")]
    ProcessNotLive { process_id: u32 },
    #[error("process {process_id} changed while it was being inspected")]
    ObservationRaced { process_id: u32 },
    #[error("tool session evidence changed while it was being inspected")]
    ToolEvidenceRaced,
    #[error("{operation} failed for process {process_id:?}: {reason}")]
    Io {
        operation: String,
        process_id: Option<u32>,
        reason: String,
    },
    #[error("{operation} for process {process_id} read {actual} bytes; the maximum is {maximum}")]
    ReadLimitExceeded {
        operation: String,
        process_id: u32,
        actual: usize,
        maximum: usize,
    },
    #[error("process evidence could not be refined: {0}")]
    InvalidEvidence(String),
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    #[test]
    fn process_fd_inspection_does_not_block_on_a_fifo() {
        let temp = tempfile::tempdir().unwrap();
        let fifo = temp.path().join("process-fd");
        let encoded = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);

        let opened = open_process_fd(&fifo).unwrap();

        assert!(!opened.metadata().unwrap().is_file());
    }

    #[test]
    fn direct_tool_record_loading_rejects_a_fifo_without_blocking() {
        let temp = tempfile::tempdir().unwrap();
        let fifo = temp.path().join("tool-record");
        let encoded = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);

        let error = open_tool_record(&fifo).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
}
