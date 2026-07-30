use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tmux_rescue::{
    AttentionReason, AutomaticPaneObservation, AutomaticRecovery, AutomaticRecoveryExpectation,
    CapturedCodexPromptArea, CapturedCommand, CodexPromptPasteFailure, CodexSessionId,
    GuardedPaneFailure, GuardedPaneOperation, LosslessOsString, OpenedCodexSessionFile,
    PaneProcessObservation, PaneProcessProbe, PaneRecovery, PaneRestoreOutcome,
    PaneTiedForegroundEvidence, PlanningExecutable, ProcessInspectionFailure, RecordedAbsolutePath,
    RestoreEnvironment, RestoreEnvironmentFailure, RestoreExecutionFailure, RestoreExecutor,
    RestorePlan, RestoreRunStatus, RestoreTargetCapability, RestoreTargetState, RollbackOutcome,
    TargetDisposition, TargetShell, TmuxRestoreAdapter, TmuxSelector, TopologyPane,
    ValidatedSnapshot, plan_restore,
};

static ISOLATED_TMUX_TEST: Mutex<()> = Mutex::new(());
const CODEX_SESSION_A: &str = "018f8f15-2e24-7a8a-a5c0-bf32e04c45be";
const CODEX_SESSION_B: &str = "a27834ae-6192-4287-a005-86063335c28e";
const CODEX_PROMPT_TEXT: &str = "The test prompt for recovering.\n\nLine 1.\n\nLine 2.";

fn encoded(value: &str) -> Value {
    json!({"encoding": "utf8", "value": value})
}

fn encoded_path(path: &Path) -> Value {
    encoded(path.to_str().expect("temporary paths are UTF-8"))
}

fn idle_pane(source_index: u32, working_directory: &Path) -> Value {
    json!({
        "source_index": source_index,
        "working_directory": encoded_path(working_directory),
        "recovery": {"kind": "idle"}
    })
}

fn manual_pane(source_index: u32, working_directory: &Path, marker: &Path) -> Value {
    json!({
        "source_index": source_index,
        "working_directory": encoded_path(working_directory),
        "recovery": {
            "kind": "manual",
            "command": {
                "executable": encoded("/usr/bin/touch"),
                "argv": [encoded("/usr/bin/touch"), encoded_path(marker)]
            }
        }
    })
}

fn automatic_mdbook_pane(
    source_index: u32,
    working_directory: &Path,
    executable: &Path,
    port: u16,
) -> Value {
    let executable = executable.to_str().expect("mdbook path is UTF-8");
    json!({
        "source_index": source_index,
        "working_directory": encoded_path(working_directory),
        "recovery": {
            "kind": "automatic",
            "recovery": {
                "kind": "md_book_serve",
                "command": {
                    "executable": encoded(executable),
                    "argv": [
                        encoded(executable),
                        encoded("serve"),
                        encoded("--hostname"),
                        encoded("127.0.0.1"),
                        encoded("--port"),
                        encoded(&port.to_string())
                    ]
                }
            }
        }
    })
}

fn automatic_codex_pane(
    source_index: u32,
    working_directory: &Path,
    session_id: &str,
    prompt: &str,
) -> Value {
    json!({
        "source_index": source_index,
        "working_directory": encoded_path(working_directory),
        "recovery": {
            "kind": "automatic",
            "recovery": {
                "kind": "codex",
                "session_id": session_id,
                "prompt_area": {"text": prompt}
            }
        }
    })
}

fn window(source_index: u32, name: &str, panes: Vec<Value>) -> Value {
    json!({
        "source_index": source_index,
        "name": name,
        "panes": panes
    })
}

fn session(name: &str, working_directory: &Path, windows: Vec<Value>) -> Value {
    json!({
        "name": name,
        "working_directory": encoded_path(working_directory),
        "windows": windows
    })
}

fn snapshot(source: &Path, sessions: Vec<Value>) -> ValidatedSnapshot {
    let raw = json!({
        "captured_at": "2026-07-23T00:00:00Z",
        "source": encoded_path(source),
        "consistency": {"kind": "stable"},
        "sessions": sessions
    });
    ValidatedSnapshot::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap()
}

struct PlanningEnvironment {
    home: RecordedAbsolutePath,
    executables: HashMap<Vec<u8>, Vec<u8>>,
    target_shell: PathBuf,
}

impl PlanningEnvironment {
    fn new(home: &Path) -> Self {
        Self {
            home: RecordedAbsolutePath::try_from_bytes(home.as_os_str().as_bytes().to_vec())
                .unwrap(),
            executables: HashMap::new(),
            target_shell: PathBuf::from("/bin/sh"),
        }
    }

    fn with_target_shell(mut self, target_shell: &Path) -> Self {
        self.target_shell = target_shell.to_owned();
        self
    }

    fn with_executable(mut self, command: &Path) -> Self {
        let bytes = command.as_os_str().as_bytes().to_vec();
        let command_name = command.file_name().unwrap().as_bytes().to_vec();
        self.executables.insert(command_name, bytes);
        self
    }
}

impl RestoreEnvironment for PlanningEnvironment {
    fn target_shell(&self) -> Result<TargetShell, RestoreEnvironmentFailure> {
        TargetShell::try_from_bytes(self.target_shell.as_os_str().as_bytes().to_vec())
            .map_err(|error| RestoreEnvironmentFailure::new(error.to_string()))
    }

    fn home_directory(&self) -> Result<RecordedAbsolutePath, RestoreEnvironmentFailure> {
        Ok(self.home.clone())
    }

    fn directory_exists(&self, directory: &RecordedAbsolutePath) -> bool {
        Path::new(directory.as_os_str()).is_dir()
    }

    fn resolve_executable(
        &self,
        _directory: &RecordedAbsolutePath,
        command_word: &LosslessOsString,
    ) -> Option<PlanningExecutable> {
        self.executables
            .get(command_word.as_bytes())
            .cloned()
            .and_then(|path| PlanningExecutable::try_from_bytes(path).ok())
    }
}

fn target_selector(socket: &Path) -> TmuxSelector {
    TmuxSelector::SocketPath(socket.as_os_str().to_owned())
}

fn isolated_tmux(socket: &Path) -> Command {
    assert!(socket.is_absolute());
    let mut command = Command::new("tmux");
    command
        .args(["-u", "-N", "-S"])
        .arg(socket)
        .env_remove("TMUX");
    command
}

fn require_success(operation: &str, output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn tmux_stdout(socket: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = isolated_tmux(socket)
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("failed to run isolated tmux: {error}"));
    require_success("isolated tmux command", output)
}

struct IsolatedServerGuard {
    socket: PathBuf,
}

struct SelectedServerGuard {
    selector: TmuxSelector,
}

struct ChildProcessGuard {
    child: Child,
}

struct EnvironmentGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

struct CurrentDirectoryGuard {
    previous: PathBuf,
}

#[derive(Debug, Eq, PartialEq)]
struct TreeEntry {
    path: PathBuf,
    contents: TreeContents,
}

#[derive(Debug, Eq, PartialEq)]
enum TreeContents {
    Directory,
    File(Vec<u8>),
    Symlink(Vec<u8>),
    Socket,
    Other,
}

struct TargetCommandContext {
    path: Option<OsString>,
    tmux: Option<OsString>,
    log: Option<OsString>,
}

struct AlwaysIdleProbe;

type ObservationAction = (usize, Box<dyn Fn(&TopologyPane)>);

struct ScriptedCodexProbe {
    session_ids: Mutex<VecDeque<&'static str>>,
    observations: Arc<AtomicUsize>,
    after_observation: Option<ObservationAction>,
}

impl ScriptedCodexProbe {
    fn new(session_ids: impl IntoIterator<Item = &'static str>) -> (Self, Arc<AtomicUsize>) {
        let observations = Arc::new(AtomicUsize::new(0));
        (
            Self {
                session_ids: Mutex::new(session_ids.into_iter().collect()),
                observations: Arc::clone(&observations),
                after_observation: None,
            },
            observations,
        )
    }

    fn after_observation(
        mut self,
        observation_number: usize,
        action: impl Fn(&TopologyPane) + 'static,
    ) -> Self {
        self.after_observation = Some((observation_number, Box::new(action)));
        self
    }
}

impl PaneProcessProbe for AlwaysIdleProbe {
    fn observe(
        &self,
        _pane: &TopologyPane,
    ) -> Result<PaneProcessObservation, ProcessInspectionFailure> {
        Ok(PaneProcessObservation::Idle)
    }
}

impl PaneProcessProbe for ScriptedCodexProbe {
    fn observe(
        &self,
        pane: &TopologyPane,
    ) -> Result<PaneProcessObservation, ProcessInspectionFailure> {
        let session_id = self
            .session_ids
            .lock()
            .unwrap()
            .pop_front()
            .expect("test script supplies one session per process observation");
        let observation = codex_observation(pane, session_id);
        let observation_number = self.observations.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some((expected_number, action)) = &self.after_observation
            && observation_number == *expected_number
        {
            action(pane);
        }
        Ok(observation)
    }
}

fn lossless(value: &str) -> LosslessOsString {
    LosslessOsString::try_from_bytes(value.as_bytes().to_vec()).unwrap()
}

fn codex_observation(pane: &TopologyPane, session_id: &str) -> PaneProcessObservation {
    let process_id = 12_345;
    let tty = pane.process_anchor().pane_tty().clone();
    let command =
        CapturedCommand::try_new(lossless("/usr/bin/codex"), vec![lossless("codex")]).unwrap();
    let session_file = OpenedCodexSessionFile::try_new(
        process_id,
        8,
        42,
        RecordedAbsolutePath::try_from_bytes(
            format!("/home/user/.codex/sessions/2026/07/25/rollout-{session_id}.jsonl")
                .into_bytes(),
        )
        .unwrap(),
        serde_json::to_vec(&json!({
            "type": "session_meta",
            "payload": {
                "id": session_id,
                "originator": "codex-tui",
                "thread_source": "user",
                "cwd": pane.working_directory().as_os_str().to_str().unwrap(),
                "parent_thread_id": null
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let evidence = PaneTiedForegroundEvidence::try_new(
        command,
        pane.working_directory().clone(),
        tty.clone(),
        tty,
        process_id,
        process_id,
        process_id,
        99,
    )
    .unwrap()
    .with_codex_session_evidence(
        RecordedAbsolutePath::try_from_bytes(b"/home/user/.codex/sessions".to_vec()).unwrap(),
        vec![session_file],
    )
    .unwrap();
    PaneProcessObservation::Foreground(Box::new(evidence))
}

fn codex_prompt_fixture(
    temp: &Path,
    session_id: &str,
    prompt: &str,
) -> (CodexSessionId, CapturedCodexPromptArea) {
    let snapshot = snapshot(
        &temp.join("source.sock"),
        vec![session(
            "codex-fixture",
            temp,
            vec![window(
                0,
                "codex-fixture",
                vec![automatic_codex_pane(0, temp, session_id, prompt)],
            )],
        )],
    );
    let PaneRecovery::Automatic(AutomaticRecovery::Codex {
        session_id,
        prompt_area: Some(prompt),
    }) = snapshot.sessions()[0].windows()[0].panes()[0].recovery()
    else {
        panic!("expected a Codex prompt fixture");
    };
    (session_id.clone(), prompt.clone())
}

impl Drop for TargetCommandContext {
    fn drop(&mut self) {
        for (key, value) in [
            ("PATH", &self.path),
            ("TMUX", &self.tmux),
            ("FAKE_TMUX_LOG", &self.log),
        ] {
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

fn install_fake_target_tmux(temp: &Path) -> (PathBuf, TargetCommandContext) {
    let bin = temp.join("bin");
    fs::create_dir(&bin).unwrap();
    let executable = bin.join("tmux");
    fs::write(
        &executable,
        b"#!/bin/sh\n{ printf 'BEGIN\\nPWD=%s\\nTMUX=%s\\n' \"$PWD\" \"${TMUX-unset}\"; for arg do printf 'ARG=%s\\n' \"$arg\"; done; } >> \"$FAKE_TMUX_LOG\"\ncase \" $* \" in\n  *' start-server '*) exit 0 ;;\n  *' display-message '*) printf '1:11:11:0\\n' ;;\n  *' show-options '*) printf 'wrong-token\\n' ;;\n  *' if-shell '*) exit 0 ;;\n  *) exit 1 ;;\nesac\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let log = temp.join("target-tmux.log");
    let old_path = std::env::var_os("PATH");
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        old_path.as_deref().unwrap_or(OsStr::new("")),
    )))
    .unwrap();
    let guard = TargetCommandContext {
        path: old_path,
        tmux: std::env::var_os("TMUX"),
        log: std::env::var_os("FAKE_TMUX_LOG"),
    };
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("TMUX", "ambient.sock,1,0");
        std::env::set_var("FAKE_TMUX_LOG", &log);
    }
    (log, guard)
}

fn install_logging_tmux_proxy(temp: &Path) -> (PathBuf, Vec<EnvironmentGuard>) {
    let real_tmux = std::env::split_paths(std::env::var_os("PATH").as_deref().unwrap_or_default())
        .map(|directory| directory.join("tmux"))
        .find(|candidate| candidate.is_file())
        .expect("tmux is installed");
    let bin = temp.join("proxy-bin");
    fs::create_dir(&bin).unwrap();
    let executable = bin.join("tmux");
    fs::write(
        &executable,
        b"#!/bin/sh\nfor arg do printf '%s\\000' \"$arg\"; done >> \"$FAKE_TMUX_LOG\"\nexec \"$REAL_TMUX\" \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let log = temp.join("proxy-tmux.log");
    let old_path = std::env::var_os("PATH");
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        old_path.as_deref().unwrap_or(OsStr::new("")),
    )))
    .unwrap();
    let guards = vec![
        EnvironmentGuard::set("PATH", &path),
        EnvironmentGuard::set("FAKE_TMUX_LOG", &log),
        EnvironmentGuard::set("REAL_TMUX", &real_tmux),
    ];
    (log, guards)
}

struct BlockedPromptProxyLogs {
    input: PathBuf,
    pane_probe: PathBuf,
}

fn install_blocked_prompt_proxy(
    temp: &Path,
    selector: &TmuxSelector,
    target_pane: &str,
    remove_pane: bool,
) -> (BlockedPromptProxyLogs, Vec<EnvironmentGuard>) {
    let real_tmux = std::env::split_paths(std::env::var_os("PATH").as_deref().unwrap_or_default())
        .map(|directory| directory.join("tmux"))
        .find(|candidate| candidate.is_file())
        .expect("tmux is installed");
    let bin = temp.join(if remove_pane {
        "remove-pane-proxy-bin"
    } else {
        "retain-pane-proxy-bin"
    });
    fs::create_dir(&bin).unwrap();
    let executable = bin.join("tmux");
    fs::write(
        &executable,
        br#"#!/bin/sh
case " $* " in
  *' display-message '*)
    for arg do printf '%s\000' "$arg"; done >> "$PANE_PROBE_LOG"
    ;;
  *' if-shell '*)
    output=$("$REAL_TMUX" "$@")
    status=$?
    case "$output" in
      TMUX_RESCUE_INPUT_BLOCKED_*)
        if [ "$REMOVE_BLOCKED_PANE" = 1 ]; then
          "$REAL_TMUX" -u -N "$TEST_SELECTOR_FLAG" "$TEST_SELECTOR_VALUE" set-option -s exit-empty off >/dev/null || exit 97
          "$REAL_TMUX" -u -N "$TEST_SELECTOR_FLAG" "$TEST_SELECTOR_VALUE" kill-pane -t "$TEST_TARGET_PANE" >/dev/null || exit 98
        fi
        ;;
      *) printf 'INPUT_EXECUTED\n' >> "$PROMPT_PROXY_LOG" ;;
    esac
    if [ -n "$output" ]; then printf '%s\n' "$output"; fi
    exit "$status"
    ;;
esac
exec "$REAL_TMUX" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let input_log = temp.join(if remove_pane {
        "remove-pane-proxy.log"
    } else {
        "retain-pane-proxy.log"
    });
    let pane_probe_log = temp.join(if remove_pane {
        "remove-pane-probe.log"
    } else {
        "retain-pane-probe.log"
    });
    let old_path = std::env::var_os("PATH");
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        old_path.as_deref().unwrap_or(OsStr::new("")),
    )))
    .unwrap();
    let guards = vec![
        EnvironmentGuard::set("PATH", &path),
        EnvironmentGuard::set("REAL_TMUX", &real_tmux),
        EnvironmentGuard::set("TEST_SELECTOR_FLAG", selector.flag()),
        EnvironmentGuard::set("TEST_SELECTOR_VALUE", selector.value()),
        EnvironmentGuard::set("TEST_TARGET_PANE", target_pane),
        EnvironmentGuard::set("REMOVE_BLOCKED_PANE", if remove_pane { "1" } else { "0" }),
        EnvironmentGuard::set("PROMPT_PROXY_LOG", &input_log),
        EnvironmentGuard::set("PANE_PROBE_LOG", &pane_probe_log),
    ];
    (
        BlockedPromptProxyLogs {
            input: input_log,
            pane_probe: pane_probe_log,
        },
        guards,
    )
}

struct PromptPasteRaceLogs {
    buffer_created: PathBuf,
}

fn install_prompt_paste_race_proxy(
    temp: &Path,
    selector: &TmuxSelector,
    target_pane: &str,
    replace_owner: bool,
) -> (PromptPasteRaceLogs, Vec<EnvironmentGuard>) {
    let real_tmux = std::env::split_paths(std::env::var_os("PATH").as_deref().unwrap_or_default())
        .map(|directory| directory.join("tmux"))
        .find(|candidate| candidate.is_file())
        .expect("tmux is installed");
    let bin = temp.join(if replace_owner {
        "reowned-paste-race-proxy-bin"
    } else {
        "paste-race-proxy-bin"
    });
    fs::create_dir(&bin).unwrap();
    let executable = bin.join("tmux");
    fs::write(
        &executable,
        br#"#!/bin/sh
if [ ! -e "$PROMPT_RACE_DONE" ]; then
  case " $* " in
    *' if-shell '*)
      condition=$9
      commands=${10}
      blocked=${11}
      set_command=${commands%% ; *}
      "$REAL_TMUX" -u -N "$TEST_SELECTOR_FLAG" "$TEST_SELECTOR_VALUE" if-shell -F -t "$TEST_TARGET_PANE" "$condition" "$set_command" "$blocked" || exit 96
      "$REAL_TMUX" -u -N "$TEST_SELECTOR_FLAG" "$TEST_SELECTOR_VALUE" list-buffers -F '#{buffer_name}' > "$PROMPT_RACE_BUFFER_LOG" || exit 97
      test -s "$PROMPT_RACE_BUFFER_LOG" || exit 98
      : > "$PROMPT_RACE_DONE"
      if [ "$REPLACE_PROMPT_OWNER" = 1 ]; then
        "$REAL_TMUX" -u -N "$TEST_SELECTOR_FLAG" "$TEST_SELECTOR_VALUE" set-option -s @tmux_rescue_owner replacement-owner >/dev/null || exit 99
      fi
      "$REAL_TMUX" -u -N "$TEST_SELECTOR_FLAG" "$TEST_SELECTOR_VALUE" kill-pane -t "$TEST_TARGET_PANE" >/dev/null || exit 100
      case "$commands" in
        *' ; '*)
          paste_command=${commands#* ; }
          exec "$REAL_TMUX" -u -N "$TEST_SELECTOR_FLAG" "$TEST_SELECTOR_VALUE" if-shell -F -t "$TEST_TARGET_PANE" 1 "$paste_command" ""
          ;;
        *) exit 0 ;;
      esac
      ;;
  esac
fi
exec "$REAL_TMUX" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let buffer_created = temp.join(if replace_owner {
        "reowned-buffer-created.log"
    } else {
        "buffer-created.log"
    });
    let done = temp.join(if replace_owner {
        "reowned-paste-race.done"
    } else {
        "paste-race.done"
    });
    let old_path = std::env::var_os("PATH");
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        old_path.as_deref().unwrap_or(OsStr::new("")),
    )))
    .unwrap();
    let guards = vec![
        EnvironmentGuard::set("PATH", &path),
        EnvironmentGuard::set("REAL_TMUX", &real_tmux),
        EnvironmentGuard::set("TEST_SELECTOR_FLAG", selector.flag()),
        EnvironmentGuard::set("TEST_SELECTOR_VALUE", selector.value()),
        EnvironmentGuard::set("TEST_TARGET_PANE", target_pane),
        EnvironmentGuard::set("PROMPT_RACE_BUFFER_LOG", &buffer_created),
        EnvironmentGuard::set("PROMPT_RACE_DONE", &done),
        EnvironmentGuard::set(
            "REPLACE_PROMPT_OWNER",
            if replace_owner { "1" } else { "0" },
        ),
    ];
    (PromptPasteRaceLogs { buffer_created }, guards)
}

fn nul_framed_arguments(path: &Path) -> Vec<Vec<u8>> {
    let bytes = fs::read(path).unwrap_or_default();
    if bytes.is_empty() {
        return Vec::new();
    }
    assert_eq!(bytes.last(), Some(&0));
    bytes[..bytes.len() - 1]
        .split(|byte| *byte == 0)
        .map(<[u8]>::to_vec)
        .collect()
}

fn parse_tmux_command_list(input: &[u8]) -> Vec<Vec<Vec<u8>>> {
    let mut commands = vec![Vec::new()];
    let mut cursor = 0;
    while cursor < input.len() {
        assert_eq!(input[cursor], b'"', "argument must start with a quote");
        cursor += 1;
        let mut argument = Vec::new();
        while input.get(cursor) != Some(&b'"') {
            assert_eq!(input.get(cursor), Some(&b'\\'));
            let octal = input
                .get(cursor + 1..cursor + 4)
                .expect("quoted byte must have three octal digits");
            assert!(octal.iter().all(|byte| matches!(byte, b'0'..=b'7')));
            let value = octal
                .iter()
                .fold(0_u8, |value, digit| value * 8 + (digit - b'0'));
            argument.push(value);
            cursor += 4;
        }
        cursor += 1;
        commands.last_mut().unwrap().push(argument);
        if cursor == input.len() {
            break;
        }
        if input[cursor..].starts_with(b" ; ") {
            commands.push(Vec::new());
            cursor += 3;
        } else {
            assert_eq!(input.get(cursor), Some(&b' '));
            cursor += 1;
        }
    }
    assert!(commands.iter().all(|command| !command.is_empty()));
    commands
}

fn install_claim_evidence_tmux(
    temp: &Path,
    scenario: &str,
    server_process_id: u32,
) -> (PathBuf, Vec<EnvironmentGuard>) {
    let bin = temp.join("bin");
    fs::create_dir(&bin).unwrap();
    let executable = bin.join("tmux");
    fs::write(
        &executable,
        br#"#!/bin/sh
{
    printf 'BEGIN\nPWD=%s\nTMUX=%s\n' "$PWD" "${TMUX-unset}"
    for arg do printf 'ARG=%s\n' "$arg"; done
} >> "$FAKE_TMUX_LOG"
case " $* " in
  *' start-server '*)
    previous=
    config=
    for arg do
      if [ "$previous" = '-f' ]; then config=$arg; fi
      previous=$arg
    done
    sed -n 's/^set-option -s @tmux_rescue_owner //p' "$config" > "$FAKE_TMUX_STATE"
    exit 1
    ;;
  *' display-message '*)
    atomic=false
    case " $* " in *'@tmux_rescue_owner'*) atomic=true ;; esac
    case "$FAKE_TMUX_SCENARIO" in
      missing_pid) printf '0:1:11:1:0\n'; exit 0 ;;
      missing_tmux_start) printf '%s:%s0:1:0\n' "${#FAKE_SERVER_PID}" "$FAKE_SERVER_PID"; exit 0 ;;
      missing_os_process) pid=4294967294 ;;
      *) pid=$FAKE_SERVER_PID ;;
    esac
    sessions=0
    if [ "$FAKE_TMUX_SCENARIO" = 'sessions_present' ]; then sessions=1; fi
    if [ "$atomic" = true ]; then
      owner=$(cat "$FAKE_TMUX_STATE")
      case "$FAKE_TMUX_SCENARIO" in
        missing_token) owner= ;;
        mismatched_token|claim_tuple_splice) owner=wrong-token ;;
        tuple_changed_during_claim)
          observations=$(cat "$FAKE_TMUX_COUNTER" 2>/dev/null || printf 0)
          if [ "$observations" -ne 0 ]; then owner=wrong-token; fi
          printf '%s\n' "$((observations + 1))" > "$FAKE_TMUX_COUNTER"
          ;;
      esac
      printf '%s:%s%s:%s%s:%s%s:%s\n' \
        "${#pid}" "$pid" 2 11 "${#sessions}" "$sessions" "${#owner}" "$owner"
      if [ "$FAKE_TMUX_SCENARIO" = 'replaced_os_process' ]; then
        kill "$FAKE_SERVER_PID" 2>/dev/null || true
      fi
    else
      printf '%s:%s%s:%s%s:%s\n' "${#pid}" "$pid" 2 11 "${#sessions}" "$sessions"
    fi
    exit 0
    ;;
  *' show-options '*)
    case "$FAKE_TMUX_SCENARIO" in
      missing_token) exit 1 ;;
      mismatched_token) printf 'wrong-token\n'; exit 0 ;;
      replaced_os_process)
        cat "$FAKE_TMUX_STATE"
        kill "$FAKE_SERVER_PID" 2>/dev/null || true
        exit 0
        ;;
      *) cat "$FAKE_TMUX_STATE"; exit 0 ;;
    esac
    ;;
  *' if-shell '*)
    case "$FAKE_TMUX_SCENARIO" in
      mismatched_pid)
        case " $* " in
          *'#{==:#{pid},'"$FAKE_SERVER_PID"'}'*) exit 0 ;;
        esac
        ;;
      mismatched_tmux_start)
        case " $* " in *'#{==:#{start_time},11}'*) exit 0 ;; esac
        ;;
      sessions_appeared)
        case " $* " in *'#{==:#{server_sessions},0}'*) exit 0 ;; esac
        ;;
    esac
    printf 'KILL_EXECUTED\n' >> "$FAKE_TMUX_LOG"
    exit 0
    ;;
  *) exit 1 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let log = temp.join("target-tmux.log");
    let state = temp.join("target-tmux.state");
    let counter = temp.join("target-tmux.counter");
    let old_path = std::env::var_os("PATH");
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        old_path.as_deref().unwrap_or(OsStr::new("")),
    )))
    .unwrap();
    let guards = vec![
        EnvironmentGuard::set("PATH", &path),
        EnvironmentGuard::set("TMUX", "ambient.sock,1,0"),
        EnvironmentGuard::set("FAKE_TMUX_LOG", &log),
        EnvironmentGuard::set("FAKE_TMUX_STATE", &state),
        EnvironmentGuard::set("FAKE_TMUX_COUNTER", &counter),
        EnvironmentGuard::set("FAKE_TMUX_SCENARIO", scenario),
        EnvironmentGuard::set("FAKE_SERVER_PID", server_process_id.to_string()),
    ];
    (log, guards)
}

fn install_owned_target_tmux(
    temp: &Path,
    server_process_id: u32,
) -> (PathBuf, Vec<EnvironmentGuard>) {
    let bin = temp.join("bin");
    fs::create_dir(&bin).unwrap();
    let executable = bin.join("tmux");
    fs::write(
        &executable,
        br#"#!/bin/sh
{
    printf 'BEGIN\nPWD=%s\nTMUX=%s\n' "$PWD" "${TMUX-unset}"
    for arg do printf 'ARG=%s\n' "$arg"; done
} >> "$FAKE_TMUX_LOG"
case " $* " in
  *' start-server '*)
    previous=
    config=
    for arg do
      if [ "$previous" = '-f' ]; then config=$arg; fi
      previous=$arg
    done
    sed -n 's/^set-option -s @tmux_rescue_owner //p' "$config" > "$FAKE_TMUX_STATE"
    exit 0
    ;;
  *' display-message '*)
    if [ -e "$FAKE_TMUX_REMOVED" ]; then exit 1; fi
    pid=$FAKE_SERVER_PID
    sessions=0
    if [ "$FAKE_TMUX_SCENARIO" = 'sessions_present' ]; then sessions=1; fi
    case " $* " in
      *'@tmux_rescue_owner'*)
        owner=$(cat "$FAKE_TMUX_STATE")
        if [ "$FAKE_TMUX_SCENARIO" = 'disposition_tuple_splice' ]; then
          owner=wrong-token
        fi
        printf '%s:%s2:11%s:%s%s:%s\n' \
          "${#pid}" "$pid" "${#sessions}" "$sessions" "${#owner}" "$owner"
        ;;
      *) printf '%s:%s2:11%s:%s\n' "${#pid}" "$pid" "${#sessions}" "$sessions" ;;
    esac
    exit 0
    ;;
  *' show-options '*)
    if [ -e "$FAKE_TMUX_REMOVED" ]; then exit 1; fi
    cat "$FAKE_TMUX_STATE"
    exit 0
    ;;
  *' if-shell '*)
    if [ "$FAKE_TMUX_SCENARIO" = 'token_changed_before_guard' ]; then
      expected_owner=$(cat "$FAKE_TMUX_STATE")
      printf 'replacement-owner\n' > "$FAKE_TMUX_STATE"
      case " $* " in
        *'#{==:#{@tmux_rescue_owner},'"$expected_owner"'}'*)
          printf 'TMUX_RESCUE_OWNERSHIP_LOST_%s\n' "$expected_owner"
          exit 0
          ;;
      esac
    fi
    : > "$FAKE_TMUX_REMOVED"
    printf 'KILL_EXECUTED\n' >> "$FAKE_TMUX_LOG"
    kill "$FAKE_SERVER_PID" 2>/dev/null || true
    exit 0
    ;;
  *) exit 1 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let log = temp.join("target-tmux.log");
    let state = temp.join("target-tmux.state");
    let removed = temp.join("target-tmux.removed");
    let old_path = std::env::var_os("PATH");
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        old_path.as_deref().unwrap_or(OsStr::new("")),
    )))
    .unwrap();
    let guards = vec![
        EnvironmentGuard::set("PATH", &path),
        EnvironmentGuard::set("TMUX", "ambient.sock,1,0"),
        EnvironmentGuard::set("FAKE_TMUX_LOG", &log),
        EnvironmentGuard::set("FAKE_TMUX_STATE", &state),
        EnvironmentGuard::set("FAKE_TMUX_REMOVED", &removed),
        EnvironmentGuard::set("FAKE_TMUX_SCENARIO", "owned"),
        EnvironmentGuard::set("FAKE_SERVER_PID", server_process_id.to_string()),
    ];
    (log, guards)
}

impl EnvironmentGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::set_var(key, value.as_ref()) };
        Self { key, previous }
    }
}

impl CurrentDirectoryGuard {
    fn set(directory: &Path) -> Self {
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(directory).unwrap();
        Self { previous }
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}

impl Drop for CurrentDirectoryGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.previous).unwrap();
    }
}

impl IsolatedServerGuard {
    fn for_socket(socket: &Path) -> Self {
        assert!(socket.is_absolute());
        Self {
            socket: socket.to_owned(),
        }
    }
}

impl SelectedServerGuard {
    fn start_preexisting(selector: TmuxSelector, working_directory: &Path) -> Self {
        let guard = Self { selector };
        let output = selected_tmux(&guard.selector, false)
            .args([
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "preexisting",
                "-n",
                "untouched",
                "-c",
            ])
            .arg(working_directory)
            .output()
            .expect("tmux is installed");
        require_success("start pre-existing selected tmux server", output);
        require_success(
            "set pre-existing sentinel",
            selected_tmux(&guard.selector, true)
                .args([
                    "set-option",
                    "-g",
                    "@tmux_target_test_sentinel",
                    "untouched",
                ])
                .output()
                .unwrap(),
        );
        guard
    }

    fn fingerprint(&self) -> Vec<u8> {
        let mut fingerprint =
            selected_tmux_stdout(&self.selector, &["display-message", "-p", "#{pid}"]);
        fingerprint.push(0);
        fingerprint.extend(selected_tmux_stdout(
            &self.selector,
            &[
                "list-sessions",
                "-F",
                "#{session_name}\t#{session_windows}\t#{session_path}",
            ],
        ));
        fingerprint.push(0);
        fingerprint.extend(selected_tmux_stdout(
            &self.selector,
            &["show-options", "-gv", "@tmux_target_test_sentinel"],
        ));
        fingerprint.push(0);
        fingerprint.extend(selected_tmux_stdout(
            &self.selector,
            &[
                "list-panes",
                "-a",
                "-F",
                "#{session_name}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_current_path}\t#{pane_start_command}\t#{default-shell}",
            ],
        ));
        fingerprint
    }
}

impl Drop for SelectedServerGuard {
    fn drop(&mut self) {
        let _ = selected_tmux(&self.selector, true)
            .arg("kill-server")
            .output();
    }
}

impl ChildProcessGuard {
    fn sleeping() -> Self {
        Self {
            child: Command::new("/bin/sleep").arg("30").spawn().unwrap(),
        }
    }

    fn process_id(&self) -> u32 {
        self.child.id()
    }

    fn terminate(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ChildProcessGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for IsolatedServerGuard {
    fn drop(&mut self) {
        let _ = isolated_tmux(&self.socket).arg("kill-server").output();
    }
}

fn selected_tmux(selector: &TmuxSelector, no_start: bool) -> Command {
    let mut command = Command::new("tmux");
    command.arg("-u");
    if no_start {
        command.arg("-N");
    }
    command.arg(selector.flag()).arg(selector.value());
    command.env_remove("TMUX");
    command
}

fn selected_tmux_stdout(selector: &TmuxSelector, arguments: &[&str]) -> Vec<u8> {
    let output = selected_tmux(selector, true)
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("failed to run selected tmux: {error}"));
    require_success("selected tmux command", output)
}

#[derive(Debug)]
struct TopologyRow {
    session_name: String,
    session_path: String,
    window_index: u32,
    window_name: String,
    pane_index: u32,
    pane_path: String,
    pane_start_command: String,
    default_shell: String,
    pane_current_command: String,
}

impl TopologyRow {
    fn location(&self) -> (String, String, u32, String, u32, String) {
        (
            self.session_name.clone(),
            self.session_path.clone(),
            self.window_index,
            self.window_name.clone(),
            self.pane_index,
            self.pane_path.clone(),
        )
    }
}

fn topology_rows(socket: &Path) -> Vec<TopologyRow> {
    let output = tmux_stdout(
        socket,
        &[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}\t#{session_path}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_current_path}\t#{pane_start_command}\t#{default-shell}\t#{pane_current_command}",
        ],
    );
    String::from_utf8(output)
        .expect("controlled tmux fields are UTF-8")
        .lines()
        .map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            assert_eq!(fields.len(), 9, "unexpected topology row: {line:?}");
            TopologyRow {
                session_name: fields[0].to_owned(),
                session_path: fields[1].to_owned(),
                window_index: fields[2].parse().unwrap(),
                window_name: fields[3].to_owned(),
                pane_index: fields[4].parse().unwrap(),
                pane_path: fields[5].to_owned(),
                pane_start_command: fields[6].to_owned(),
                default_shell: fields[7].to_owned(),
                pane_current_command: fields[8].to_owned(),
            }
        })
        .collect()
}

fn create_directory(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    fs::create_dir(&path).unwrap();
    path
}

fn inventory_controlled_trees(trees: &[(&str, &Path)]) -> Vec<TreeEntry> {
    let mut inventory = Vec::new();
    for (name, root) in trees {
        inventory_tree(Path::new(name), root, &mut inventory);
    }
    inventory
}

fn inventory_tree(label: &Path, path: &Path, inventory: &mut Vec<TreeEntry>) {
    let metadata = fs::symlink_metadata(path).unwrap();
    let file_type = metadata.file_type();
    let contents = if file_type.is_dir() {
        TreeContents::Directory
    } else if file_type.is_file() {
        TreeContents::File(fs::read(path).unwrap())
    } else if file_type.is_symlink() {
        TreeContents::Symlink(fs::read_link(path).unwrap().as_os_str().as_bytes().to_vec())
    } else if file_type.is_socket() {
        TreeContents::Socket
    } else {
        TreeContents::Other
    };
    inventory.push(TreeEntry {
        path: label.to_owned(),
        contents,
    });
    if file_type.is_dir() {
        let mut children = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect::<Vec<_>>();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            inventory_tree(&label.join(child.file_name()), &child.path(), inventory);
        }
    }
}

fn single_pane_plan(temp: &Path, selector: TmuxSelector) -> RestorePlan {
    let session_directory = create_directory(temp, "planned-session");
    let pane_directory = create_directory(temp, "planned-pane");
    let snapshot = snapshot(
        &temp.join("source.sock"),
        vec![session(
            "planned",
            &session_directory,
            vec![window(
                0,
                "planned-window",
                vec![idle_pane(0, &pane_directory)],
            )],
        )],
    );
    plan_restore(&snapshot, Some(selector), &PlanningEnvironment::new(temp)).unwrap()
}

#[test]
fn claim_and_later_clients_keep_the_exact_explicit_selector() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let session_directory = create_directory(temp.path(), "session");
    let pane_directory = create_directory(temp.path(), "pane");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "work",
            &session_directory,
            vec![window(0, "work", vec![idle_pane(0, &pane_directory)])],
        )],
    );
    let (log_path, _context) = install_fake_target_tmux(temp.path());
    let expected_directory = format!("PWD={}\n", std::env::current_dir().unwrap().display());
    for (selector, selector_arguments) in [
        (
            TmuxSelector::SocketName(OsString::from("exact-name")),
            b"ARG=-L\nARG=exact-name\n".as_slice(),
        ),
        (
            TmuxSelector::SocketPath(OsString::from("./relative.sock")),
            b"ARG=-S\nARG=./relative.sock\n".as_slice(),
        ),
    ] {
        fs::write(&log_path, b"").unwrap();
        let plan = plan_restore(
            &snapshot,
            Some(selector),
            &PlanningEnvironment::new(temp.path()),
        )
        .unwrap();
        let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

        let result = executor.execute(plan);

        assert_eq!(result.status(), RestoreRunStatus::Fatal);
        let log = fs::read(&log_path).unwrap();
        let starts = log
            .windows(b"BEGIN\n".len())
            .enumerate()
            .filter_map(|(index, bytes)| (bytes == b"BEGIN\n").then_some(index))
            .collect::<Vec<_>>();
        assert!(starts.len() >= 2, "{}", String::from_utf8_lossy(&log));
        for (index, start) in starts.iter().copied().enumerate() {
            let end = starts.get(index + 1).copied().unwrap_or(log.len());
            let command = &log[start..end];
            assert!(
                command
                    .windows(selector_arguments.len())
                    .any(|bytes| bytes == selector_arguments)
            );
            assert!(
                command
                    .windows(b"TMUX=unset\n".len())
                    .any(|bytes| bytes == b"TMUX=unset\n")
            );
            assert!(
                command
                    .windows(expected_directory.len())
                    .any(|bytes| bytes == expected_directory.as_bytes())
            );
            if index == 0 {
                assert!(
                    command
                        .windows(b"ARG=start-server\n".len())
                        .any(|bytes| bytes == b"ARG=start-server\n")
                );
                assert!(
                    !command
                        .windows(b"ARG=-N\n".len())
                        .any(|bytes| bytes == b"ARG=-N\n")
                );
            } else {
                assert!(
                    command
                        .windows(b"ARG=-N\n".len())
                        .any(|bytes| bytes == b"ARG=-N\n")
                );
            }
        }
    }
}

#[test]
fn claim_dispatch_failure_is_not_established_and_consumes_the_attempt() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let plan = single_pane_plan(
        temp.path(),
        TmuxSelector::SocketPath(OsString::from("dispatch-failure.sock")),
    );
    let bin = temp.path().join("empty-bin");
    fs::create_dir(&bin).unwrap();
    let _path = EnvironmentGuard::set("PATH", &bin);
    let mut adapter = TmuxRestoreAdapter::new();

    let first_failure = match adapter.claim(plan.destination(), plan.target_shell()) {
        Ok(_) => panic!("a missing tmux executable cannot establish ownership"),
        Err(failure) => failure,
    };

    assert_eq!(
        first_failure.target_state(),
        &RestoreTargetState::NotEstablished
    );

    let dispatch_log = temp.path().join("unexpected-retry");
    let executable = bin.join("tmux");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf dispatched >> '{}'\nexit 1\n",
            dispatch_log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();

    let retry_failure = match adapter.claim(plan.destination(), plan.target_shell()) {
        Ok(_) => panic!("a consumed claim attempt cannot return an owned target"),
        Err(failure) => failure,
    };

    assert!(retry_failure.message().contains("already claimed a target"));
    assert!(
        !dispatch_log.exists(),
        "the consumed attempt was dispatched again"
    );
}

#[test]
fn dispatched_claim_failure_is_observed_returns_no_capability_and_cannot_retry() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let plan = single_pane_plan(
        temp.path(),
        TmuxSelector::SocketPath(OsString::from("post-dispatch.sock")),
    );
    let (log_path, _guards) =
        install_claim_evidence_tmux(temp.path(), "missing_token", std::process::id());
    let mut adapter = TmuxRestoreAdapter::new();

    let first_failure = match adapter.claim(plan.destination(), plan.target_shell()) {
        Ok(_) => panic!("a failed post-dispatch claim cannot return an owned target"),
        Err(failure) => failure,
    };

    assert!(matches!(
        first_failure.target_state(),
        RestoreTargetState::Observed(_)
    ));

    let retry_failure = match adapter.claim(plan.destination(), plan.target_shell()) {
        Ok(_) => panic!("an ambiguous dispatched attempt cannot be retried"),
        Err(failure) => failure,
    };
    assert!(retry_failure.message().contains("already claimed a target"));
    let log = fs::read(&log_path).unwrap();
    assert_eq!(
        log.windows(b"ARG=start-server\n".len())
            .filter(|bytes| *bytes == b"ARG=start-server\n")
            .count(),
        1,
        "start-server was dispatched more than once: {}",
        String::from_utf8_lossy(&log)
    );
}

#[test]
fn claim_does_not_compose_identity_fields_from_different_server_observations() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let plan = single_pane_plan(
        temp.path(),
        TmuxSelector::SocketPath(OsString::from("spliced-claim.sock")),
    );
    let (log_path, _guards) =
        install_claim_evidence_tmux(temp.path(), "claim_tuple_splice", std::process::id());
    let mut adapter = TmuxRestoreAdapter::new();

    let failure = match adapter.claim(plan.destination(), plan.target_shell()) {
        Ok(_) => panic!("a spliced server identity cannot authorize cleanup"),
        Err(failure) => failure,
    };

    assert!(matches!(
        failure.target_state(),
        RestoreTargetState::Observed(_)
    ));
    let log = fs::read(&log_path).unwrap();
    assert!(
        !log.windows(b"ARG=if-shell\n".len())
            .any(|bytes| bytes == b"ARG=if-shell\n"),
        "cleanup was dispatched from a composed identity: {}",
        String::from_utf8_lossy(&log)
    );
}

#[test]
fn claim_rejects_a_complete_tuple_change_around_process_capture() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let plan = single_pane_plan(
        temp.path(),
        TmuxSelector::SocketPath(OsString::from("changed-claim-tuple.sock")),
    );
    let (log_path, _guards) = install_claim_evidence_tmux(
        temp.path(),
        "tuple_changed_during_claim",
        std::process::id(),
    );
    let mut adapter = TmuxRestoreAdapter::new();

    let failure = match adapter.claim(plan.destination(), plan.target_shell()) {
        Ok(_) => panic!("a changed claim tuple cannot return an owned target"),
        Err(failure) => failure,
    };

    assert!(matches!(
        failure.target_state(),
        RestoreTargetState::Observed(_)
    ));
    let log = fs::read(&log_path).unwrap();
    assert!(
        !log.windows(b"ARG=if-shell\n".len())
            .any(|bytes| bytes == b"ARG=if-shell\n"),
        "cleanup was dispatched after the complete claim tuple changed: {}",
        String::from_utf8_lossy(&log)
    );
}

#[test]
fn successful_start_with_zero_sessions_returns_owned_capability() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let plan = single_pane_plan(
        temp.path(),
        TmuxSelector::SocketPath(OsString::from("empty-after-start.sock")),
    );
    let process = ChildProcessGuard::sleeping();
    let (_log_path, _guards) = install_owned_target_tmux(temp.path(), process.process_id());
    let mut adapter = TmuxRestoreAdapter::new();

    let owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("zero-session claim failed: {failure}"));
    let mut recovery = owned.begin_recovery();

    assert_eq!(recovery.observe_disposition(), TargetDisposition::Retained);
}

#[test]
fn successful_start_with_sessions_returns_no_owned_capability() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let plan = single_pane_plan(
        temp.path(),
        TmuxSelector::SocketPath(OsString::from("sessions-after-start.sock")),
    );
    let process = ChildProcessGuard::sleeping();
    let (log_path, _guards) = install_owned_target_tmux(temp.path(), process.process_id());
    let _scenario = EnvironmentGuard::set("FAKE_TMUX_SCENARIO", "sessions_present");
    let mut adapter = TmuxRestoreAdapter::new();

    let failure = match adapter.claim(plan.destination(), plan.target_shell()) {
        Ok(_) => panic!("a claimed server with sessions cannot return an owned target"),
        Err(failure) => failure,
    };

    assert!(matches!(
        failure.target_state(),
        RestoreTargetState::Observed(_)
    ));
    let log = fs::read(&log_path).unwrap();
    assert!(
        !log.windows(b"ARG=if-shell\n".len())
            .any(|bytes| bytes == b"ARG=if-shell\n"),
        "a nonempty target was sent a cleanup command: {}",
        String::from_utf8_lossy(&log)
    );
}

#[test]
fn disposition_does_not_compose_identity_fields_from_different_observations() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let plan = single_pane_plan(
        temp.path(),
        TmuxSelector::SocketPath(OsString::from("spliced-disposition.sock")),
    );
    let process = ChildProcessGuard::sleeping();
    let (_log_path, _guards) = install_owned_target_tmux(temp.path(), process.process_id());
    let mut adapter = TmuxRestoreAdapter::new();
    let owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    let _scenario = EnvironmentGuard::set("FAKE_TMUX_SCENARIO", "disposition_tuple_splice");
    let mut recovery = owned.begin_recovery();

    assert_eq!(recovery.observe_disposition(), TargetDisposition::Unknown);
}

#[test]
fn owner_token_change_before_guarded_command_blocks_mutation() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let plan = single_pane_plan(
        temp.path(),
        TmuxSelector::SocketPath(OsString::from("changed-token.sock")),
    );
    let process = ChildProcessGuard::sleeping();
    let (log_path, _guards) = install_owned_target_tmux(temp.path(), process.process_id());
    let mut adapter = TmuxRestoreAdapter::new();
    let owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    let _scenario = EnvironmentGuard::set("FAKE_TMUX_SCENARIO", "token_changed_before_guard");

    let outcome = owned.rollback();
    let log = fs::read(&log_path).unwrap();
    assert!(
        !log.windows(b"KILL_EXECUTED\n".len())
            .any(|bytes| bytes == b"KILL_EXECUTED\n"),
        "the guarded command ran after the owner token changed: {}",
        String::from_utf8_lossy(&log)
    );
    assert!(matches!(outcome, RollbackOutcome::Failed(_)));
}

#[test]
fn failed_claim_cleanup_requires_complete_matching_identity_and_zero_sessions() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (scenario, proof_can_be_constructed) in [
        ("missing_token", false),
        ("mismatched_token", false),
        ("missing_pid", false),
        ("missing_tmux_start", false),
        ("missing_os_process", false),
        ("replaced_os_process", false),
        ("sessions_present", false),
        ("mismatched_pid", true),
        ("mismatched_tmux_start", true),
        ("sessions_appeared", true),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let plan = single_pane_plan(
            temp.path(),
            TmuxSelector::SocketPath(OsString::from("failed-cleanup.sock")),
        );
        let replaced_process =
            (scenario == "replaced_os_process").then(ChildProcessGuard::sleeping);
        let server_process_id = replaced_process
            .as_ref()
            .map_or_else(std::process::id, ChildProcessGuard::process_id);
        let (log_path, _guards) =
            install_claim_evidence_tmux(temp.path(), scenario, server_process_id);
        let mut adapter = TmuxRestoreAdapter::new();

        let failure = match adapter.claim(plan.destination(), plan.target_shell()) {
            Ok(_) => panic!("scenario {scenario} returned an owned target after claim failure"),
            Err(failure) => failure,
        };

        assert!(matches!(
            failure.target_state(),
            RestoreTargetState::Observed(_)
        ));
        let log = fs::read(&log_path).unwrap();
        assert!(
            !log.windows(b"KILL_EXECUTED\n".len())
                .any(|bytes| bytes == b"KILL_EXECUTED\n"),
            "scenario {scenario} allowed cleanup to kill without full identity: {}",
            String::from_utf8_lossy(&log)
        );
        if !proof_can_be_constructed {
            assert!(
                !log.windows(b"ARG=if-shell\n".len())
                    .any(|bytes| bytes == b"ARG=if-shell\n"),
                "scenario {scenario} dispatched cleanup without a proof: {}",
                String::from_utf8_lossy(&log)
            );
        }
    }
}

#[test]
fn post_claim_endpoint_process_replacement_prevents_the_next_mutation() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let plan = single_pane_plan(
        temp.path(),
        TmuxSelector::SocketPath(OsString::from("replaced-endpoint.sock")),
    );
    let mut original_process = ChildProcessGuard::sleeping();
    let replacement_process = ChildProcessGuard::sleeping();
    let (log_path, _guards) = install_owned_target_tmux(temp.path(), original_process.process_id());
    let mut adapter = TmuxRestoreAdapter::new();
    let mut owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    original_process.terminate();
    unsafe {
        std::env::set_var(
            "FAKE_SERVER_PID",
            replacement_process.process_id().to_string(),
        )
    };
    let before_mutation = fs::read(&log_path).unwrap();

    let result = owned.create_topology(&plan);

    assert!(result.is_err());
    assert_eq!(
        fs::read(&log_path).unwrap(),
        before_mutation,
        "a target command was dispatched after the owned OS process changed"
    );
}

#[test]
fn endpoint_replacement_before_recovery_input_dispatches_no_tmux_command() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("replaced-before-input.sock");
    let selector = target_selector(&socket);
    let plan = single_pane_plan(temp.path(), selector.clone());
    let mut adapter = TmuxRestoreAdapter::with_process_probe(AlwaysIdleProbe);
    let mut owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    owned
        .create_topology(&plan)
        .unwrap_or_else(|failure| panic!("topology setup failed: {failure}"));
    let original_server_pid = String::from_utf8(selected_tmux_stdout(
        &selector,
        &["display-message", "-p", "#{pid}"],
    ))
    .unwrap()
    .trim()
    .parse::<u32>()
    .unwrap();
    require_success(
        "stop original owned server",
        selected_tmux(&selector, true)
            .arg("kill-server")
            .output()
            .unwrap(),
    );
    let original_process = PathBuf::from(format!("/proc/{original_server_pid}"));
    let process_exit_deadline = Instant::now() + Duration::from_secs(2);
    while original_process.exists() && Instant::now() < process_exit_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !original_process.exists(),
        "original owned server process did not exit before replacement"
    );
    if socket.exists() {
        fs::remove_file(&socket).unwrap();
    }
    let _replacement = SelectedServerGuard::start_preexisting(selector, temp.path());
    let mut recovery = owned.begin_recovery();
    let (log_path, _proxy) = install_logging_tmux_proxy(temp.path());

    let result = recovery.guarded_pane_operation(
        plan.panes()[0].coordinate(),
        plan.target_shell(),
        GuardedPaneOperation::VerifyShell,
    );

    assert!(matches!(result, Err(GuardedPaneFailure::Failed(_))));
    assert_eq!(
        fs::read(&log_path).unwrap_or_default(),
        b"",
        "recovery input reached the replacement endpoint"
    );
}

#[test]
fn changed_codex_identity_dispatches_no_tmux_input() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("changed-codex-identity.sock");
    let plan = single_pane_plan(temp.path(), target_selector(&socket));
    let (session_id, prompt) = codex_prompt_fixture(
        temp.path(),
        CODEX_SESSION_A,
        "draft line\nsecond line: \u{4f60}\u{597d}",
    );
    let expected = AutomaticRecoveryExpectation::Codex(session_id.clone());
    let (probe, observations) = ScriptedCodexProbe::new([CODEX_SESSION_A, CODEX_SESSION_B]);
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut adapter = TmuxRestoreAdapter::with_process_probe(probe);
    let mut owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    owned
        .create_topology(&plan)
        .unwrap_or_else(|failure| panic!("topology setup failed: {failure}"));
    let coordinate = plan.panes()[0].coordinate().clone();
    let mut recovery = owned.begin_recovery();

    assert_eq!(
        recovery.observe_automatic(&coordinate, &expected),
        AutomaticPaneObservation::Recovered
    );
    let (log_path, _proxy) = install_logging_tmux_proxy(temp.path());

    let result = recovery.paste_codex_prompt_area(&coordinate, &session_id, &prompt);

    assert_eq!(result, Err(CodexPromptPasteFailure::SessionMismatch));
    assert_eq!(observations.load(Ordering::SeqCst), 2);
    assert_eq!(
        fs::read(&log_path).unwrap_or_default(),
        b"",
        "changed Codex identity dispatched tmux input"
    );
}

#[test]
fn disabled_pane_input_rejects_prompt_paste_without_writing() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("disabled-prompt-input.sock");
    let selector = target_selector(&socket);
    let plan = single_pane_plan(temp.path(), selector.clone());
    let (session_id, prompt) = codex_prompt_fixture(
        temp.path(),
        CODEX_SESSION_A,
        "disabled pane must not receive this sensitive prompt",
    );
    let expected = AutomaticRecoveryExpectation::Codex(session_id.clone());
    let (probe, observations) = ScriptedCodexProbe::new([CODEX_SESSION_A, CODEX_SESSION_A]);
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut adapter = TmuxRestoreAdapter::with_process_probe(probe);
    let mut owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    owned
        .create_topology(&plan)
        .unwrap_or_else(|failure| panic!("topology setup failed: {failure}"));
    let coordinate = plan.panes()[0].coordinate().clone();
    let mut recovery = owned.begin_recovery();
    assert_eq!(
        recovery.observe_automatic(&coordinate, &expected),
        AutomaticPaneObservation::Recovered
    );
    let target_pane = String::from_utf8(selected_tmux_stdout(
        &selector,
        &["display-message", "-p", "-t", "planned:0.0", "#{pane_id}"],
    ))
    .unwrap();
    let target_pane = target_pane.trim_end();
    require_success(
        "disable exact pane input",
        selected_tmux(&selector, true)
            .args(["select-pane", "-d", "-t", target_pane])
            .output()
            .unwrap(),
    );
    assert_eq!(
        selected_tmux_stdout(
            &selector,
            &[
                "display-message",
                "-p",
                "-t",
                target_pane,
                "#{pane_input_off}"
            ],
        ),
        b"1\n"
    );

    let result = recovery.paste_codex_prompt_area(&coordinate, &session_id, &prompt);
    let captured = selected_tmux_stdout(&selector, &["capture-pane", "-p", "-t", target_pane]);

    assert_eq!(observations.load(Ordering::SeqCst), 2);
    assert!(
        !captured
            .windows(prompt.text().as_str().len())
            .any(|bytes| bytes == prompt.text().as_str().as_bytes()),
        "disabled pane received the recovered prompt"
    );
    assert_eq!(result, Err(CodexPromptPasteFailure::InputDisabled));
}

#[test]
fn pane_exit_between_buffer_creation_and_paste_cleans_the_unique_buffer() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("prompt-paste-pane-exit.sock");
    let selector = target_selector(&socket);
    let plan = single_pane_plan(temp.path(), selector.clone());
    let (session_id, prompt) = codex_prompt_fixture(
        temp.path(),
        CODEX_SESSION_A,
        "pane exit must not leave this sensitive prompt buffered",
    );
    let expected = AutomaticRecoveryExpectation::Codex(session_id.clone());
    let (probe, observations) = ScriptedCodexProbe::new([CODEX_SESSION_A, CODEX_SESSION_A]);
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut adapter = TmuxRestoreAdapter::with_process_probe(probe);
    let mut owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    owned
        .create_topology(&plan)
        .unwrap_or_else(|failure| panic!("topology setup failed: {failure}"));
    let coordinate = plan.panes()[0].coordinate().clone();
    let mut recovery = owned.begin_recovery();
    assert_eq!(
        recovery.observe_automatic(&coordinate, &expected),
        AutomaticPaneObservation::Recovered
    );
    let target_pane = String::from_utf8(selected_tmux_stdout(
        &selector,
        &["display-message", "-p", "-t", "planned:0.0", "#{pane_id}"],
    ))
    .unwrap();
    let target_pane = target_pane.trim_end();
    require_success(
        "create keepalive pane for prompt-paste race",
        selected_tmux(&selector, true)
            .args(["split-window", "-d", "-t", target_pane])
            .output()
            .unwrap(),
    );
    let (race_logs, _proxy) =
        install_prompt_paste_race_proxy(temp.path(), &selector, target_pane, false);

    let result = recovery.paste_codex_prompt_area(&coordinate, &session_id, &prompt);
    let buffers = selected_tmux(&selector, true)
        .args(["list-buffers", "-F", "#{buffer_name}"])
        .output()
        .unwrap();
    let created_buffer = fs::read(&race_logs.buffer_created).unwrap();
    let created_buffer = created_buffer
        .strip_suffix(b"\n")
        .expect("buffer proof has one line");

    assert_eq!(observations.load(Ordering::SeqCst), 2);
    assert!(created_buffer.starts_with(b"tmux-rescue-"));
    assert!(!created_buffer.contains(&b'\n'));
    assert!(
        !buffers
            .stdout
            .split(|byte| *byte == b'\n')
            .any(|buffer| buffer == created_buffer),
        "exact prompt buffer survived failed paste: {}",
        String::from_utf8_lossy(created_buffer)
    );
    assert_eq!(result, Err(CodexPromptPasteFailure::PasteFailed));
}

#[test]
fn cleanup_refuses_a_reowned_server_and_surfaces_cleanup_failure() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("prompt-buffer-reowned.sock");
    let selector = target_selector(&socket);
    let plan = single_pane_plan(temp.path(), selector.clone());
    let (session_id, prompt) = codex_prompt_fixture(
        temp.path(),
        CODEX_SESSION_A,
        "replacement owner must retain this same-named buffer",
    );
    let expected = AutomaticRecoveryExpectation::Codex(session_id.clone());
    let (probe, observations) = ScriptedCodexProbe::new([CODEX_SESSION_A, CODEX_SESSION_A]);
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut adapter = TmuxRestoreAdapter::with_process_probe(probe);
    let mut owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    owned
        .create_topology(&plan)
        .unwrap_or_else(|failure| panic!("topology setup failed: {failure}"));
    let coordinate = plan.panes()[0].coordinate().clone();
    let mut recovery = owned.begin_recovery();
    assert_eq!(
        recovery.observe_automatic(&coordinate, &expected),
        AutomaticPaneObservation::Recovered
    );
    let target_pane = String::from_utf8(selected_tmux_stdout(
        &selector,
        &["display-message", "-p", "-t", "planned:0.0", "#{pane_id}"],
    ))
    .unwrap();
    let target_pane = target_pane.trim_end();
    require_success(
        "create keepalive pane for reowned prompt-paste race",
        selected_tmux(&selector, true)
            .args(["split-window", "-d", "-t", target_pane])
            .output()
            .unwrap(),
    );
    let (race_logs, _proxy) =
        install_prompt_paste_race_proxy(temp.path(), &selector, target_pane, true);

    let result = recovery.paste_codex_prompt_area(&coordinate, &session_id, &prompt);
    let buffers = selected_tmux(&selector, true)
        .args(["list-buffers", "-F", "#{buffer_name}"])
        .output()
        .unwrap();
    let created_buffer = fs::read(&race_logs.buffer_created).unwrap();
    let created_buffer = created_buffer
        .strip_suffix(b"\n")
        .expect("buffer proof has one line");

    assert_eq!(observations.load(Ordering::SeqCst), 2);
    assert!(created_buffer.starts_with(b"tmux-rescue-"));
    assert!(!created_buffer.contains(&b'\n'));
    assert_eq!(
        selected_tmux_stdout(&selector, &["show-options", "-sv", "@tmux_rescue_owner"],),
        b"replacement-owner\n"
    );
    assert!(
        buffers
            .stdout
            .split(|byte| *byte == b'\n')
            .any(|buffer| buffer == created_buffer),
        "cleanup deleted the exact buffer after target ownership changed"
    );
    assert_eq!(result, Err(CodexPromptPasteFailure::CleanupFailed));
}

#[test]
fn endpoint_replacement_before_prompt_paste_dispatches_no_tmux_input() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("replaced-before-prompt-paste.sock");
    let selector = target_selector(&socket);
    let plan = single_pane_plan(temp.path(), selector.clone());
    let (session_id, prompt) = codex_prompt_fixture(
        temp.path(),
        CODEX_SESSION_A,
        "replacement must not receive this input",
    );
    let expected = AutomaticRecoveryExpectation::Codex(session_id.clone());
    let (probe, observations) = ScriptedCodexProbe::new([CODEX_SESSION_A, CODEX_SESSION_A]);
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut adapter = TmuxRestoreAdapter::with_process_probe(probe);
    let mut owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    owned
        .create_topology(&plan)
        .unwrap_or_else(|failure| panic!("topology setup failed: {failure}"));
    let coordinate = plan.panes()[0].coordinate().clone();
    let mut recovery = owned.begin_recovery();
    assert_eq!(
        recovery.observe_automatic(&coordinate, &expected),
        AutomaticPaneObservation::Recovered
    );

    let original_server_pid = String::from_utf8(selected_tmux_stdout(
        &selector,
        &["display-message", "-p", "#{pid}"],
    ))
    .unwrap()
    .trim()
    .parse::<u32>()
    .unwrap();
    require_success(
        "stop original owned server",
        selected_tmux(&selector, true)
            .arg("kill-server")
            .output()
            .unwrap(),
    );
    let original_process = PathBuf::from(format!("/proc/{original_server_pid}"));
    let process_exit_deadline = Instant::now() + Duration::from_secs(2);
    while original_process.exists() && Instant::now() < process_exit_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !original_process.exists(),
        "original owned server process did not exit before replacement"
    );
    if socket.exists() {
        fs::remove_file(&socket).unwrap();
    }
    let _replacement = SelectedServerGuard::start_preexisting(selector, temp.path());
    let (log_path, _proxy) = install_logging_tmux_proxy(temp.path());

    let result = recovery.paste_codex_prompt_area(&coordinate, &session_id, &prompt);

    assert_eq!(result, Err(CodexPromptPasteFailure::PasteFailed));
    assert_eq!(observations.load(Ordering::SeqCst), 2);
    assert_eq!(
        fs::read(&log_path).unwrap_or_default(),
        b"",
        "prompt input reached the replacement endpoint"
    );
}

#[test]
fn blocked_prompt_guard_reports_the_exact_identity_race_without_input() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("blocked-prompt-guard.sock");
    let selector = target_selector(&socket);
    let plan = single_pane_plan(temp.path(), selector.clone());
    let (session_id, prompt) = codex_prompt_fixture(
        temp.path(),
        CODEX_SESSION_A,
        "blocked owner must not receive this input",
    );
    let expected = AutomaticRecoveryExpectation::Codex(session_id.clone());
    let owner_selector = selector.clone();
    let (probe, observations) = ScriptedCodexProbe::new([CODEX_SESSION_A, CODEX_SESSION_A]);
    let probe = probe.after_observation(2, move |_| {
        require_success(
            "replace the owner after fresh Codex observation",
            selected_tmux(&owner_selector, true)
                .args([
                    "set-option",
                    "-s",
                    "@tmux_rescue_owner",
                    "replacement-owner",
                ])
                .output()
                .unwrap(),
        );
    });
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut adapter = TmuxRestoreAdapter::with_process_probe(probe);
    let mut owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    owned
        .create_topology(&plan)
        .unwrap_or_else(|failure| panic!("topology setup failed: {failure}"));
    let coordinate = plan.panes()[0].coordinate().clone();
    let mut recovery = owned.begin_recovery();
    assert_eq!(
        recovery.observe_automatic(&coordinate, &expected),
        AutomaticPaneObservation::Recovered
    );
    let target_pane = String::from_utf8(selected_tmux_stdout(
        &selector,
        &["display-message", "-p", "-t", "planned:0.0", "#{pane_id}"],
    ))
    .unwrap();
    let (logs, _proxy) =
        install_blocked_prompt_proxy(temp.path(), &selector, target_pane.trim_end(), false);

    let result = recovery.paste_codex_prompt_area(&coordinate, &session_id, &prompt);

    assert_eq!(result, Err(CodexPromptPasteFailure::PasteFailed));
    assert_eq!(observations.load(Ordering::SeqCst), 2);
    assert_eq!(
        fs::read(&logs.input).unwrap_or_default(),
        b"",
        "the blocked conditional executed prompt input"
    );
    assert_eq!(
        nul_framed_arguments(&logs.pane_probe),
        vec![
            b"-u".to_vec(),
            b"-N".to_vec(),
            selector.flag().as_bytes().to_vec(),
            selector.value().as_bytes().to_vec(),
            b"display-message".to_vec(),
            b"-p".to_vec(),
            b"-t".to_vec(),
            target_pane.trim_end().as_bytes().to_vec(),
            b"#{pane_id}".to_vec(),
        ]
    );
}

#[test]
fn pane_removed_after_blocked_prompt_guard_reports_pane_missing() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("removed-after-blocked-prompt-guard.sock");
    let selector = target_selector(&socket);
    let plan = single_pane_plan(temp.path(), selector.clone());
    let (session_id, prompt) = codex_prompt_fixture(
        temp.path(),
        CODEX_SESSION_A,
        "removed pane must not receive this input",
    );
    let expected = AutomaticRecoveryExpectation::Codex(session_id.clone());
    let owner_selector = selector.clone();
    let (probe, observations) = ScriptedCodexProbe::new([CODEX_SESSION_A, CODEX_SESSION_A]);
    let probe = probe.after_observation(2, move |_| {
        require_success(
            "replace the owner after fresh Codex observation",
            selected_tmux(&owner_selector, true)
                .args([
                    "set-option",
                    "-s",
                    "@tmux_rescue_owner",
                    "replacement-owner",
                ])
                .output()
                .unwrap(),
        );
    });
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut adapter = TmuxRestoreAdapter::with_process_probe(probe);
    let mut owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    owned
        .create_topology(&plan)
        .unwrap_or_else(|failure| panic!("topology setup failed: {failure}"));
    require_success(
        "create an unrelated keepalive pane",
        selected_tmux(&selector, true)
            .args(["split-window", "-d", "-t", "planned:0.0"])
            .output()
            .unwrap(),
    );
    let coordinate = plan.panes()[0].coordinate().clone();
    let mut recovery = owned.begin_recovery();
    assert_eq!(
        recovery.observe_automatic(&coordinate, &expected),
        AutomaticPaneObservation::Recovered
    );
    let target_pane = String::from_utf8(selected_tmux_stdout(
        &selector,
        &["display-message", "-p", "-t", "planned:0.0", "#{pane_id}"],
    ))
    .unwrap();
    let (logs, proxy) =
        install_blocked_prompt_proxy(temp.path(), &selector, target_pane.trim_end(), true);

    let result = recovery.paste_codex_prompt_area(&coordinate, &session_id, &prompt);

    assert_eq!(result, Err(CodexPromptPasteFailure::PaneMissing));
    assert_eq!(observations.load(Ordering::SeqCst), 2);
    assert_eq!(
        fs::read(&logs.input).unwrap_or_default(),
        b"",
        "the blocked conditional executed prompt input"
    );
    assert_eq!(
        nul_framed_arguments(&logs.pane_probe),
        vec![
            b"-u".to_vec(),
            b"-N".to_vec(),
            selector.flag().as_bytes().to_vec(),
            selector.value().as_bytes().to_vec(),
            b"display-message".to_vec(),
            b"-p".to_vec(),
            b"-t".to_vec(),
            target_pane.trim_end().as_bytes().to_vec(),
            b"#{pane_id}".to_vec(),
        ]
    );
    drop(proxy);
    let remaining_panes =
        selected_tmux_stdout(&selector, &["list-panes", "-a", "-F", "#{pane_id}"]);
    assert!(
        remaining_panes
            .split(|byte| *byte == b'\n')
            .all(|pane_id| pane_id != target_pane.trim_end().as_bytes()),
        "the exact isolated pane remained after successful kill-pane"
    );
    let exact_pane_probe = selected_tmux(&selector, true)
        .args([
            "display-message",
            "-p",
            "-t",
            target_pane.trim_end(),
            "#{pane_id}",
        ])
        .output()
        .unwrap();
    assert!(
        exact_pane_probe.status.success(),
        "tmux rejected the explicit dead-pane probe instead of returning its blank result: {}",
        String::from_utf8_lossy(&exact_pane_probe.stderr)
    );
    assert_eq!(
        exact_pane_probe.stdout,
        b"\n",
        "the explicit dead-pane probe did not return tmux's blank identity: {}",
        String::from_utf8_lossy(&exact_pane_probe.stdout)
    );
}

#[test]
fn fresh_codex_identity_is_checked_after_settle_observation() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("fresh-codex-identity.sock");
    let selector = target_selector(&socket);
    let plan = single_pane_plan(temp.path(), selector.clone());
    let (session_id, prompt) = codex_prompt_fixture(
        temp.path(),
        CODEX_SESSION_A,
        "draft line\nsecond line: \u{4f60}\u{597d}",
    );
    let expected = AutomaticRecoveryExpectation::Codex(session_id.clone());
    let (probe, observations) = ScriptedCodexProbe::new([CODEX_SESSION_A, CODEX_SESSION_A]);
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut adapter = TmuxRestoreAdapter::with_process_probe(probe);
    let mut owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    owned
        .create_topology(&plan)
        .unwrap_or_else(|failure| panic!("topology setup failed: {failure}"));
    require_success(
        "start a non-shell foreground process",
        selected_tmux(&selector, true)
            .args(["send-keys", "-t", "planned:0.0", "sleep 30", "Enter"])
            .output()
            .unwrap(),
    );
    let foreground_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let current = selected_tmux_stdout(
            &selector,
            &[
                "display-message",
                "-p",
                "-t",
                "planned:0.0",
                "#{pane_current_command}",
            ],
        );
        if current == b"sleep\n" {
            break;
        }
        assert!(
            Instant::now() < foreground_deadline,
            "foreground process did not become sleep: {}",
            String::from_utf8_lossy(&current)
        );
        thread::sleep(Duration::from_millis(10));
    }
    let coordinate = plan.panes()[0].coordinate().clone();
    let mut recovery = owned.begin_recovery();
    assert_eq!(
        recovery.observe_automatic(&coordinate, &expected),
        AutomaticPaneObservation::Recovered
    );
    let target_pane = String::from_utf8(selected_tmux_stdout(
        &selector,
        &["display-message", "-p", "-t", "planned:0.0", "#{pane_id}"],
    ))
    .unwrap();
    let target_pane = target_pane.trim_end();
    let (log_path, _proxy) = install_logging_tmux_proxy(temp.path());

    let result = recovery.paste_codex_prompt_area(&coordinate, &session_id, &prompt);

    assert_eq!(result, Ok(()));
    assert_eq!(observations.load(Ordering::SeqCst), 2);
    let arguments = nul_framed_arguments(&log_path);
    assert_eq!(arguments.len(), 11, "expected one exact tmux client call");
    assert_eq!(arguments[0], b"-u");
    assert_eq!(arguments[1], b"-N");
    assert_eq!(arguments[2], selector.flag().as_bytes());
    assert_eq!(arguments[3], selector.value().as_bytes());
    assert_eq!(arguments[4], b"if-shell");
    assert_eq!(arguments[5], b"-F");
    assert_eq!(arguments[6], b"-t");
    assert_eq!(arguments[7], target_pane.as_bytes());

    let blocked = parse_tmux_command_list(&arguments[10]);
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].len(), 3);
    assert_eq!(blocked[0][0], b"display-message");
    assert_eq!(blocked[0][1], b"-p");
    let owner_token = blocked[0][2]
        .strip_prefix(b"TMUX_RESCUE_INPUT_BLOCKED_")
        .expect("blocked marker carries the owner token");
    assert_eq!(owner_token.len(), 64);
    assert!(owner_token.iter().all(u8::is_ascii_hexdigit));
    let owner_condition = [b"#{==:#{@tmux_rescue_owner},".as_slice(), owner_token, b"}"].concat();
    assert!(
        arguments[8]
            .windows(owner_condition.len())
            .any(|bytes| bytes == owner_condition)
    );
    assert!(
        !arguments[8]
            .windows(b"pane_current_command".len())
            .any(|bytes| bytes == b"pane_current_command")
    );

    let commands = parse_tmux_command_list(&arguments[9]);
    assert_eq!(commands.len(), 2, "prompt paste has exactly two commands");
    let buffer_prefix = [b"tmux-rescue-".as_slice(), owner_token, b"-"].concat();
    let buffer_name = &commands[0][2];
    let unique_suffix = buffer_name
        .strip_prefix(buffer_prefix.as_slice())
        .expect("buffer name is scoped by the owner token");
    assert_eq!(unique_suffix.len(), 32);
    assert!(unique_suffix.iter().all(u8::is_ascii_hexdigit));
    assert_eq!(
        commands,
        vec![
            vec![
                b"set-buffer".to_vec(),
                b"-b".to_vec(),
                buffer_name.clone(),
                b"--".to_vec(),
                prompt.text().as_str().as_bytes().to_vec(),
            ],
            vec![
                b"paste-buffer".to_vec(),
                b"-d".to_vec(),
                b"-p".to_vec(),
                b"-r".to_vec(),
                b"-b".to_vec(),
                buffer_name.clone(),
                b"-t".to_vec(),
                target_pane.as_bytes().to_vec(),
            ],
        ]
    );
    assert!(commands.iter().flatten().all(|argument| {
        argument.as_slice() != b"Enter" && argument.as_slice() != b"send-keys"
    }));
}

#[test]
fn multiline_codex_prompt_is_pasted_literally_without_submission_on_a_real_target() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("multiline-prompt-target.sock");
    let selector = target_selector(&socket);
    let session_directory = create_directory(temp.path(), "prompt-session");
    let pane_directory = create_directory(temp.path(), "prompt-pane");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "prompt-target",
            &session_directory,
            vec![window(
                0,
                "prompt target",
                vec![idle_pane(0, &pane_directory)],
            )],
        )],
    );
    let environment =
        PlanningEnvironment::new(temp.path()).with_target_shell(Path::new("/bin/bash"));
    let plan = plan_restore(&snapshot, Some(selector.clone()), &environment).unwrap();
    let (session_id, prompt) =
        codex_prompt_fixture(temp.path(), CODEX_SESSION_A, CODEX_PROMPT_TEXT);
    let (probe, observations) = ScriptedCodexProbe::new([CODEX_SESSION_A]);
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut adapter = TmuxRestoreAdapter::with_process_probe(probe);
    let mut owned = adapter
        .claim(plan.destination(), plan.target_shell())
        .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));
    owned
        .create_topology(&plan)
        .unwrap_or_else(|failure| panic!("topology setup failed: {failure}"));
    require_success(
        "start a controlled bracketed-paste shell",
        selected_tmux(&selector, true)
            .args([
                "send-keys",
                "-t",
                "prompt-target:0.0",
                "exec /bin/bash --noprofile --norc -i",
                "Enter",
            ])
            .output()
            .unwrap(),
    );
    let shell_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let current = selected_tmux_stdout(
            &selector,
            &[
                "display-message",
                "-p",
                "-t",
                "prompt-target:0.0",
                "#{pane_current_command}",
            ],
        );
        if current == b"bash\n" {
            break;
        }
        assert!(
            Instant::now() < shell_deadline,
            "controlled shell did not become bash: {}",
            String::from_utf8_lossy(&current)
        );
        thread::sleep(Duration::from_millis(10));
    }
    require_success(
        "set a unique test prompt and clear the visible pane",
        selected_tmux(&selector, true)
            .args([
                "send-keys",
                "-t",
                "prompt-target:0.0",
                "PS1=$(printf 'tmux-rescue-paste-test\\044 '); printf '\\033[2J\\033[H'",
                "Enter",
            ])
            .output()
            .unwrap(),
    );
    let prompt_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let captured = tmux_stdout(&socket, &["capture-pane", "-p", "-t", "prompt-target:0.0"]);
        if captured
            .windows(b"tmux-rescue-paste-test$".len())
            .any(|window| window == b"tmux-rescue-paste-test$")
        {
            break;
        }
        assert!(
            Instant::now() < prompt_deadline,
            "controlled shell prompt was not rendered: {}",
            String::from_utf8_lossy(&captured)
        );
        thread::sleep(Duration::from_millis(10));
    }
    let coordinate = plan.panes()[0].coordinate().clone();
    let mut recovery = owned.begin_recovery();

    let result = recovery.paste_codex_prompt_area(&coordinate, &session_id, &prompt);

    assert_eq!(result, Ok(()));
    assert_eq!(observations.load(Ordering::SeqCst), 1);
    let paste_deadline = Instant::now() + Duration::from_secs(2);
    let captured = loop {
        let captured = String::from_utf8(tmux_stdout(
            &socket,
            &["capture-pane", "-p", "-t", "prompt-target:0.0"],
        ))
        .unwrap();
        if captured.contains("Line 2.") {
            break captured;
        }
        assert!(
            Instant::now() < paste_deadline,
            "multiline prompt was not rendered: {captured:?}"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert!(
        captured.contains(&format!("tmux-rescue-paste-test$ {CODEX_PROMPT_TEXT}")),
        "multiline prompt bytes or row boundaries changed: {captured:?}"
    );
    assert_eq!(
        captured.matches("tmux-rescue-paste-test$").count(),
        1,
        "the multiline input was submitted: {captured:?}"
    );
    assert_eq!(
        String::from_utf8(selected_tmux_stdout(
            &selector,
            &[
                "display-message",
                "-p",
                "-t",
                "prompt-target:0.0",
                "#{pane_current_command}",
            ],
        ))
        .unwrap()
        .trim(),
        "bash"
    );
}

#[test]
fn rollback_keeps_each_exact_selector_and_uses_no_start_clients() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for (selector, selector_arguments) in [
        (
            TmuxSelector::SocketName(OsString::from("rollback-name")),
            b"ARG=-L\nARG=rollback-name\n".as_slice(),
        ),
        (
            TmuxSelector::SocketPath(OsString::from("./rollback.sock")),
            b"ARG=-S\nARG=./rollback.sock\n".as_slice(),
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let plan = single_pane_plan(temp.path(), selector);
        let process = ChildProcessGuard::sleeping();
        let (log_path, _guards) = install_owned_target_tmux(temp.path(), process.process_id());
        let mut adapter = TmuxRestoreAdapter::new();
        let owned = adapter
            .claim(plan.destination(), plan.target_shell())
            .unwrap_or_else(|failure| panic!("claim setup failed: {failure}"));

        assert_eq!(owned.rollback(), RollbackOutcome::Removed);

        let log = fs::read(&log_path).unwrap();
        let starts = log
            .windows(b"BEGIN\n".len())
            .enumerate()
            .filter_map(|(index, bytes)| (bytes == b"BEGIN\n").then_some(index))
            .collect::<Vec<_>>();
        assert!(starts.len() >= 4, "{}", String::from_utf8_lossy(&log));
        for (index, start) in starts.iter().copied().enumerate() {
            let end = starts.get(index + 1).copied().unwrap_or(log.len());
            let command = &log[start..end];
            assert!(
                command
                    .windows(selector_arguments.len())
                    .any(|bytes| bytes == selector_arguments),
                "rollback command lost its selector: {}",
                String::from_utf8_lossy(command)
            );
            if index == 0 {
                assert!(!command.windows(7).any(|bytes| bytes == b"ARG=-N\n"));
            } else {
                assert!(command.windows(7).any(|bytes| bytes == b"ARG=-N\n"));
            }
        }
    }
}

#[test]
fn claim_rejects_preexisting_name_and_path_servers_without_changing_their_identity_or_state() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let _tmux_tmpdir = EnvironmentGuard::set("TMUX_TMPDIR", temp.path());
    let session_directory = create_directory(temp.path(), "planned-session");
    let pane_directory = create_directory(temp.path(), "planned-pane");
    let existing_directory = create_directory(temp.path(), "preexisting-session");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "planned",
            &session_directory,
            vec![window(
                4,
                "planned-window",
                vec![idle_pane(0, &pane_directory)],
            )],
        )],
    );
    for selector in [
        TmuxSelector::SocketName(OsString::from("preexisting-name")),
        TmuxSelector::SocketPath(temp.path().join("preexisting.sock").into_os_string()),
    ] {
        let plan = plan_restore(
            &snapshot,
            Some(selector.clone()),
            &PlanningEnvironment::new(temp.path()),
        )
        .unwrap();
        let server = SelectedServerGuard::start_preexisting(selector, &existing_directory);
        let before = server.fingerprint();
        let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

        let result = executor.execute(plan);

        assert_eq!(result.status(), RestoreRunStatus::Fatal);
        assert_eq!(
            result.target_state(),
            &RestoreTargetState::Observed(TargetDisposition::Unknown)
        );
        assert!(matches!(
            result.failure(),
            Some(RestoreExecutionFailure::TargetClaimFailed { .. })
        ));
        assert_eq!(server.fingerprint(), before);
    }
}

#[test]
fn plan_only_binary_does_not_contact_destinations_or_change_controlled_trees() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let controlled_cwd = create_directory(temp.path(), "cwd");
    let controlled_tmp = create_directory(temp.path(), "tmp");
    let controlled_state = create_directory(temp.path(), "state");
    let controlled_home = create_directory(temp.path(), "home");
    let controlled_tmux = create_directory(temp.path(), "tmux");
    let planned_session = create_directory(&controlled_home, "planned-session");
    let planned_pane = create_directory(&controlled_home, "planned-pane");
    let named_existing = create_directory(&controlled_home, "named-existing");
    let path_existing = create_directory(&controlled_home, "path-existing");
    let snapshot_path = controlled_cwd.join("snapshot.json");
    let fixture = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "planned",
            &planned_session,
            vec![window(
                4,
                "planned-window",
                vec![idle_pane(0, &planned_pane)],
            )],
        )],
    );
    fs::write(&snapshot_path, fixture.to_json_pretty().unwrap()).unwrap();

    let _cwd = CurrentDirectoryGuard::set(&controlled_cwd);
    let _environment = [
        EnvironmentGuard::set("TMPDIR", &controlled_tmp),
        EnvironmentGuard::set("XDG_STATE_HOME", &controlled_state),
        EnvironmentGuard::set("HOME", &controlled_home),
        EnvironmentGuard::set("TMUX_TMPDIR", &controlled_tmux),
    ];
    let named_selector = TmuxSelector::SocketName(OsString::from("plan-only-name"));
    let path_selector = TmuxSelector::SocketPath(OsString::from("relative-target.sock"));
    let named_server =
        SelectedServerGuard::start_preexisting(named_selector.clone(), &named_existing);
    let path_server = SelectedServerGuard::start_preexisting(path_selector.clone(), &path_existing);
    let named_fingerprint = named_server.fingerprint();
    let path_fingerprint = path_server.fingerprint();
    let (contact_log, contact_guards) = install_logging_tmux_proxy(&controlled_tmp);
    let controlled_trees = [
        ("cwd", controlled_cwd.as_path()),
        ("tmp", controlled_tmp.as_path()),
        ("state", controlled_state.as_path()),
        ("home", controlled_home.as_path()),
        ("tmux", controlled_tmux.as_path()),
    ];
    let before = inventory_controlled_trees(&controlled_trees);

    for (selector, expected_target) in [
        (named_selector, "target: -L plan-only-name"),
        (path_selector, "target: -S relative-target.sock"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tmux-rescue"))
            .arg(selector.flag())
            .arg(selector.value())
            .arg("restore")
            .arg(&snapshot_path)
            .current_dir(&controlled_cwd)
            .env("TMPDIR", &controlled_tmp)
            .env("XDG_STATE_HOME", &controlled_state)
            .env("HOME", &controlled_home)
            .env("TMUX_TMPDIR", &controlled_tmux)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "plan-only restore failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.lines().next(), Some(expected_target));
        assert_eq!(
            stdout
                .lines()
                .filter(|line| line.starts_with("target: "))
                .collect::<Vec<_>>(),
            vec![expected_target]
        );
        assert_eq!(
            fs::read(&contact_log).unwrap_or_default(),
            b"",
            "plan-only restore contacted a tmux destination"
        );
        assert_eq!(inventory_controlled_trees(&controlled_trees), before);
    }
    drop(contact_guards);
    assert_eq!(named_server.fingerprint(), named_fingerprint);
    assert_eq!(path_server.fingerprint(), path_fingerprint);
}

#[test]
fn restore_creates_the_selected_target_server() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let session_directory = create_directory(temp.path(), "crash-session");
    let pane_directory = create_directory(temp.path(), "crash-pane");
    let socket = temp.path().join("crashed-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "recovered",
            &session_directory,
            vec![window(
                0,
                "recovered-window",
                vec![idle_pane(0, &pane_directory)],
            )],
        )],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()),
    )
    .unwrap();
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(result.status(), RestoreRunStatus::Complete, "{result:#?}");
    assert_eq!(
        result.target_state(),
        &RestoreTargetState::Observed(TargetDisposition::Retained)
    );
    assert_eq!(topology_rows(&socket).len(), 1);
}

#[test]
fn creates_recorded_multi_session_topology_with_interactive_target_shells() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let alpha_session = create_directory(temp.path(), "alpha-session");
    let alpha_editor_first = create_directory(temp.path(), "alpha-editor-first");
    let alpha_editor_second = create_directory(temp.path(), "alpha-editor-second");
    let alpha_logs = create_directory(temp.path(), "alpha-logs");
    let beta_session = create_directory(temp.path(), "beta-session");
    let beta_ops = create_directory(temp.path(), "beta-ops");
    let shell_start_log = temp.path().join("interactive-shell-starts");
    let shell_startup = temp.path().join("interactive-shell-startup");
    fs::write(
        &shell_startup,
        format!("printf x >> '{}'\n", shell_start_log.display()),
    )
    .unwrap();
    let _environment = EnvironmentGuard::set("ENV", &shell_startup);
    let socket = temp.path().join("topology-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![
            session(
                "alpha",
                &alpha_session,
                vec![
                    window(
                        3,
                        "editor main",
                        vec![
                            idle_pane(0, &alpha_editor_first),
                            idle_pane(1, &alpha_editor_second),
                        ],
                    ),
                    window(8, "logs", vec![idle_pane(0, &alpha_logs)]),
                ],
            ),
            session(
                "beta",
                &beta_session,
                vec![window(4, "operations", vec![idle_pane(0, &beta_ops)])],
            ),
        ],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()),
    )
    .unwrap();
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(
        result.status(),
        RestoreRunStatus::Complete,
        "restore failed: {result:?}"
    );
    assert_eq!(
        result.target_state(),
        &RestoreTargetState::Observed(TargetDisposition::Retained)
    );
    assert_eq!(result.panes().len(), 4);
    assert!(
        result
            .panes()
            .iter()
            .all(|pane| pane.outcome() == &PaneRestoreOutcome::RestoredIdleShell)
    );

    let rows = topology_rows(&socket);
    let actual_locations = rows
        .iter()
        .map(TopologyRow::location)
        .collect::<BTreeSet<_>>();
    let expected_locations = [
        (
            "alpha".to_owned(),
            alpha_session.to_string_lossy().into_owned(),
            3,
            "editor main".to_owned(),
            0,
            alpha_editor_first.to_string_lossy().into_owned(),
        ),
        (
            "alpha".to_owned(),
            alpha_session.to_string_lossy().into_owned(),
            3,
            "editor main".to_owned(),
            1,
            alpha_editor_second.to_string_lossy().into_owned(),
        ),
        (
            "alpha".to_owned(),
            alpha_session.to_string_lossy().into_owned(),
            8,
            "logs".to_owned(),
            0,
            alpha_logs.to_string_lossy().into_owned(),
        ),
        (
            "beta".to_owned(),
            beta_session.to_string_lossy().into_owned(),
            4,
            "operations".to_owned(),
            0,
            beta_ops.to_string_lossy().into_owned(),
        ),
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    assert_eq!(actual_locations, expected_locations);
    for row in rows {
        assert_eq!(row.default_shell, "/bin/sh");
        assert_eq!(row.pane_start_command, "/bin/sh -i");
        assert_eq!(row.pane_current_command, "sh");
    }
    assert_eq!(
        fs::read(&shell_start_log).unwrap(),
        b"xxxx",
        "each of the four restored panes must start its interactive shell once"
    );
}

#[test]
fn pastes_manual_input_literally_without_enter() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let session_directory = create_directory(temp.path(), "manual-session");
    let pane_directory = create_directory(temp.path(), "manual-pane");
    let marker = temp.path().join("manual wasn't executed");
    let socket = temp.path().join("manual-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "manual",
            &session_directory,
            vec![window(
                0,
                "manual hint",
                vec![manual_pane(0, &pane_directory, &marker)],
            )],
        )],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()),
    )
    .unwrap();
    let expected_input = match plan.panes()[0].action() {
        tmux_rescue::PlannedPaneAction::PasteManualHint { input, .. } => {
            String::from_utf8(input.as_bytes().to_vec()).unwrap()
        }
        action => panic!("expected a manual hint, got {action:?}"),
    };
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(
        result.status(),
        RestoreRunStatus::Complete,
        "restore failed: {result:?}"
    );
    assert_eq!(
        result.panes()[0].outcome(),
        &PaneRestoreOutcome::PreparedManualHint
    );
    thread::sleep(Duration::from_millis(100));
    let captured = String::from_utf8(tmux_stdout(
        &socket,
        &["capture-pane", "-p", "-J", "-S", "-20", "-t", "manual:0.0"],
    ))
    .unwrap();
    assert!(
        captured.contains(&expected_input),
        "pane did not contain the exact literal hint: {captured:?}"
    );
    assert!(!marker.exists(), "manual hint was submitted with Enter");
    assert_eq!(
        String::from_utf8(tmux_stdout(
            &socket,
            &[
                "display-message",
                "-p",
                "-t",
                "manual:0.0",
                "#{pane_current_command}"
            ],
        ))
        .unwrap()
        .trim(),
        "sh"
    );
}

#[test]
fn topology_failure_rolls_back_only_the_newly_owned_server() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let session_directory = create_directory(temp.path(), "rollback-session");
    let pane_directory = create_directory(temp.path(), "rollback-pane");
    let socket = temp.path().join("rollback-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "rollback",
            &session_directory,
            vec![window(0, "rollback", vec![idle_pane(0, &pane_directory)])],
        )],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()),
    )
    .unwrap();
    fs::remove_dir(&pane_directory).unwrap();
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(result.status(), RestoreRunStatus::Fatal);
    assert_eq!(
        result.target_state(),
        &RestoreTargetState::Observed(TargetDisposition::Removed)
    );
    assert!(matches!(
        result.failure(),
        Some(RestoreExecutionFailure::TopologyFailed { .. })
    ));
    assert!(
        !isolated_tmux(&socket)
            .arg("has-session")
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn automatic_recovery_sends_literal_input_then_a_separate_enter() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let book_directory = create_directory(temp.path(), "book");
    let source_directory = create_directory(&book_directory, "src");
    fs::write(
        book_directory.join("book.toml"),
        "[book]\ntitle = \"Restore test\"\n",
    )
    .unwrap();
    fs::write(source_directory.join("SUMMARY.md"), "# Summary\n").unwrap();
    let mdbook = require_success(
        "resolve mdbook",
        Command::new("sh")
            .args(["-c", "command -v mdbook"])
            .output()
            .unwrap(),
    );
    let mdbook = fs::canonicalize(Path::new(
        std::str::from_utf8(&mdbook).unwrap().trim_end_matches('\n'),
    ))
    .unwrap();
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let socket = temp.path().join("automatic-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "automatic",
            &book_directory,
            vec![window(
                0,
                "mdbook",
                vec![automatic_mdbook_pane(0, &book_directory, &mdbook, port)],
            )],
        )],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()).with_executable(&mdbook),
    )
    .unwrap();
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(result.status(), RestoreRunStatus::Complete, "{result:#?}");
    assert_eq!(
        result.panes()[0].outcome(),
        &PaneRestoreOutcome::RecoveredAutomatically
    );
    assert_eq!(
        String::from_utf8(tmux_stdout(
            &socket,
            &[
                "display-message",
                "-p",
                "-t",
                "automatic:0.0",
                "#{pane_current_command}"
            ],
        ))
        .unwrap()
        .trim(),
        "mdbook"
    );
}

#[test]
fn changed_automatic_executable_is_not_launched() {
    let _tmux_test = ISOLATED_TMUX_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let session_directory = create_directory(temp.path(), "identity-session");
    let pane_directory = create_directory(temp.path(), "identity-pane");
    let executable_directory = create_directory(temp.path(), "identity-bin");
    let executable = executable_directory.join("mdbook");
    fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let socket = temp.path().join("identity-target.sock");
    let snapshot = snapshot(
        &temp.path().join("source.sock"),
        vec![session(
            "identity",
            &session_directory,
            vec![window(
                0,
                "identity",
                vec![automatic_mdbook_pane(
                    0,
                    &pane_directory,
                    &executable,
                    31_111,
                )],
            )],
        )],
    );
    let plan = plan_restore(
        &snapshot,
        Some(target_selector(&socket)),
        &PlanningEnvironment::new(temp.path()).with_executable(&executable),
    )
    .unwrap();
    fs::remove_file(&executable).unwrap();
    fs::write(&executable, b"#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let _server = IsolatedServerGuard::for_socket(&socket);
    let mut executor = RestoreExecutor::new(TmuxRestoreAdapter::new());

    let result = executor.execute(plan);

    assert_eq!(result.status(), RestoreRunStatus::Partial);
    assert!(
        matches!(
            result.panes()[0].outcome(),
            PaneRestoreOutcome::NeedsAttention(AttentionReason::GuardedOperationFailed(reason))
                if reason.contains("automatic executable changed")
        ),
        "{:#?}",
        result.panes()[0].outcome()
    );
    assert_eq!(topology_rows(&socket)[0].pane_current_command, "sh");
}
