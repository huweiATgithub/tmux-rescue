use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;

use serde_json::{Value, json};
use tmux_rescue::{
    AttentionReason, AutomaticFallbackReason, AutomaticPaneObservation,
    AutomaticRecoveryExpectation, CapturedCodexPromptArea, CodexPromptPasteFailure,
    CodexPromptPasteResult, CodexSessionId, GuardedPaneFailure, GuardedPaneOperation,
    GuardedPaneResult, LosslessOsString, OwnedRestoreTarget, PaneRestoreOutcome, PaneRestoreResult,
    PlanningExecutable, RecordedAbsolutePath, RecoveryRestoreTarget, RestoreDestination,
    RestoreEnvironment, RestoreEnvironmentFailure, RestoreExecutionFailure, RestoreExecutor,
    RestorePlan, RestoreRunResult, RestoreRunStatus, RestoreTargetCapability, RestoreTargetState,
    RollbackFailure, RollbackFailureDisposition, RollbackOutcome, SourcePaneCoordinate,
    TargetClaimFailure, TargetDisposition, TargetShell, TmuxSelector, TopologyFailure,
    ValidatedSnapshot, plan_restore,
};

fn encoded(value: &str) -> Value {
    json!({"encoding": "utf8", "value": value})
}

fn idle_pane(index: u32) -> Value {
    json!({
        "source_index": index,
        "working_directory": encoded(&format!("/workspace/pane-{index}")),
        "recovery": {"kind": "idle"}
    })
}

fn manual_pane(index: u32, argument: &str) -> Value {
    json!({
        "source_index": index,
        "working_directory": encoded(&format!("/workspace/pane-{index}")),
        "recovery": {
            "kind": "manual",
            "command": {
                "executable": encoded("/usr/bin/custom"),
                "argv": [encoded("custom"), encoded(argument)]
            }
        }
    })
}

fn automatic_pane(index: u32, session_id: &str) -> Value {
    json!({
        "source_index": index,
        "working_directory": encoded(&format!("/workspace/pane-{index}")),
        "recovery": {
            "kind": "automatic",
            "recovery": {
                "kind": "codex",
                "session_id": session_id
            }
        }
    })
}

fn automatic_pane_with_prompt(index: u32, session_id: &str, prompt: &str) -> Value {
    json!({
        "source_index": index,
        "working_directory": encoded(&format!("/workspace/pane-{index}")),
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

fn unavailable_pane(index: u32) -> Value {
    json!({
        "source_index": index,
        "working_directory": encoded(&format!("/workspace/pane-{index}")),
        "recovery": {
            "kind": "unavailable",
            "failure": "foreground process vanished during capture"
        }
    })
}

fn snapshot_with(panes: Vec<Value>) -> ValidatedSnapshot {
    let raw = json!({
        "captured_at": "2026-07-23T00:00:00Z",
        "source": encoded("/tmp/tmux-rescue-restore-target.sock"),
        "consistency": {"kind": "stable"},
        "sessions": [{
            "name": "work",
            "working_directory": encoded("/workspace"),
            "windows": [{
                "source_index": 4,
                "name": "editor",
                "panes": panes
            }]
        }]
    });
    ValidatedSnapshot::from_json(&serde_json::to_vec(&raw).unwrap()).unwrap()
}

struct PlanningEnvironment {
    available_commands: HashSet<Vec<u8>>,
}

impl PlanningEnvironment {
    fn with_commands(commands: &[&[u8]]) -> Self {
        Self {
            available_commands: commands.iter().map(|command| command.to_vec()).collect(),
        }
    }
}

impl RestoreEnvironment for PlanningEnvironment {
    fn target_shell(&self) -> Result<TargetShell, RestoreEnvironmentFailure> {
        TargetShell::try_from_bytes(b"/bin/sh".to_vec())
            .map_err(|error| RestoreEnvironmentFailure::new(error.to_string()))
    }

    fn home_directory(&self) -> Result<RecordedAbsolutePath, RestoreEnvironmentFailure> {
        RecordedAbsolutePath::try_from_bytes(b"/home/user".to_vec())
            .map_err(|error| RestoreEnvironmentFailure::new(error.to_string()))
    }

    fn directory_exists(&self, _directory: &RecordedAbsolutePath) -> bool {
        true
    }

    fn resolve_executable(
        &self,
        _directory: &RecordedAbsolutePath,
        command_word: &LosslessOsString,
    ) -> Option<PlanningExecutable> {
        self.available_commands
            .contains(command_word.as_bytes())
            .then(|| PlanningExecutable::try_from_bytes(b"/bin/sh".to_vec()).unwrap())
    }
}

fn plan_with(panes: Vec<Value>, available_commands: &[&[u8]]) -> RestorePlan {
    plan_restore(
        &snapshot_with(panes),
        None,
        &PlanningEnvironment::with_commands(available_commands),
    )
    .unwrap()
}

fn coordinate(pane_index: u32) -> SourcePaneCoordinate {
    SourcePaneCoordinate {
        session_name: "work".to_owned(),
        window_index: 4,
        pane_index,
    }
}

fn pane_result(result: &RestoreRunResult, pane_index: u32) -> &PaneRestoreResult {
    result
        .panes()
        .iter()
        .find(|pane| pane.coordinate() == &coordinate(pane_index))
        .unwrap_or_else(|| panic!("missing result for pane {pane_index}"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SubmitInput {
    NoEnter,
    SeparateEnter,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SentInput {
    pane: SourcePaneCoordinate,
    bytes: Vec<u8>,
    submit: SubmitInput,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PastedCodexPrompt {
    pane: SourcePaneCoordinate,
    expected: CodexSessionId,
    input: CapturedCodexPromptArea,
}

#[derive(Default)]
struct TargetLog {
    events: Vec<&'static str>,
    claim_calls: usize,
    claim_destinations: Vec<TmuxSelector>,
    topology_calls: usize,
    rollback_calls: usize,
    begin_recovery_calls: usize,
    guarded_attempts: Vec<SourcePaneCoordinate>,
    sent_inputs: Vec<SentInput>,
    automatic_observations: Vec<(SourcePaneCoordinate, AutomaticRecoveryExpectation)>,
    pasted_codex_prompts: Vec<PastedCodexPrompt>,
    disposition_observations: usize,
}

struct TargetScript {
    claim_failure: Option<TargetClaimFailure>,
    topology_failure: Option<TopologyFailure>,
    rollback_outcome: RollbackOutcome,
    guarded_results: VecDeque<GuardedPaneResult>,
    automatic_results: VecDeque<AutomaticPaneObservation>,
    prompt_paste_results: VecDeque<CodexPromptPasteResult>,
    final_disposition: TargetDisposition,
}

impl TargetScript {
    fn successful() -> Self {
        Self {
            claim_failure: None,
            topology_failure: None,
            rollback_outcome: RollbackOutcome::Removed,
            guarded_results: VecDeque::new(),
            automatic_results: VecDeque::new(),
            prompt_paste_results: VecDeque::new(),
            final_disposition: TargetDisposition::Retained,
        }
    }
}

struct FakeTarget {
    script: Option<TargetScript>,
    log: Rc<RefCell<TargetLog>>,
}

impl FakeTarget {
    fn new(script: TargetScript) -> (Self, Rc<RefCell<TargetLog>>) {
        let log = Rc::new(RefCell::new(TargetLog::default()));
        (
            Self {
                script: Some(script),
                log: Rc::clone(&log),
            },
            log,
        )
    }
}

impl RestoreTargetCapability for FakeTarget {
    fn claim(
        &mut self,
        destination: &RestoreDestination,
        _shell: &TargetShell,
    ) -> Result<Box<dyn OwnedRestoreTarget>, TargetClaimFailure> {
        let mut log = self.log.borrow_mut();
        log.events.push("claim");
        log.claim_calls += 1;
        log.claim_destinations.push(destination.selector().clone());
        drop(log);

        let mut script = self.script.take().expect("claim is attempted once");
        if let Some(failure) = script.claim_failure.take() {
            return Err(failure);
        }
        Ok(Box::new(FakeOwnedTarget {
            topology_failure: script.topology_failure,
            rollback_outcome: script.rollback_outcome,
            recovery: FakeRecoveryTarget {
                guarded_results: script.guarded_results,
                automatic_results: script.automatic_results,
                prompt_paste_results: script.prompt_paste_results,
                final_disposition: script.final_disposition,
                log: Rc::clone(&self.log),
            },
            log: Rc::clone(&self.log),
        }))
    }
}

struct FakeOwnedTarget {
    topology_failure: Option<TopologyFailure>,
    rollback_outcome: RollbackOutcome,
    recovery: FakeRecoveryTarget,
    log: Rc<RefCell<TargetLog>>,
}

impl OwnedRestoreTarget for FakeOwnedTarget {
    fn create_topology(&mut self, _plan: &RestorePlan) -> Result<(), TopologyFailure> {
        let mut log = self.log.borrow_mut();
        log.events.push("topology");
        log.topology_calls += 1;
        drop(log);
        match self.topology_failure.take() {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }

    fn rollback(self: Box<Self>) -> RollbackOutcome {
        let mut log = self.log.borrow_mut();
        log.events.push("rollback");
        log.rollback_calls += 1;
        self.rollback_outcome
    }

    fn begin_recovery(self: Box<Self>) -> Box<dyn RecoveryRestoreTarget> {
        let mut log = self.log.borrow_mut();
        log.events.push("begin_recovery");
        log.begin_recovery_calls += 1;
        drop(log);
        Box::new(self.recovery)
    }
}

struct FakeRecoveryTarget {
    guarded_results: VecDeque<GuardedPaneResult>,
    automatic_results: VecDeque<AutomaticPaneObservation>,
    prompt_paste_results: VecDeque<CodexPromptPasteResult>,
    final_disposition: TargetDisposition,
    log: Rc<RefCell<TargetLog>>,
}

impl RecoveryRestoreTarget for FakeRecoveryTarget {
    fn guarded_pane_operation(
        &mut self,
        pane: &SourcePaneCoordinate,
        _shell: &TargetShell,
        operation: GuardedPaneOperation<'_>,
    ) -> GuardedPaneResult {
        let result = self
            .guarded_results
            .pop_front()
            .expect("test script supplies one result per guarded attempt");
        let mut log = self.log.borrow_mut();
        log.events.push(match operation {
            GuardedPaneOperation::VerifyShell => "verify_shell",
            GuardedPaneOperation::PasteLiteral { .. } => "paste_literal",
            GuardedPaneOperation::LaunchAutomatic { .. } => "launch_automatic",
        });
        log.guarded_attempts.push(pane.clone());
        if result.is_ok() {
            let input = match operation {
                GuardedPaneOperation::VerifyShell => None,
                GuardedPaneOperation::PasteLiteral { input } => {
                    Some((input.as_bytes(), SubmitInput::NoEnter))
                }
                GuardedPaneOperation::LaunchAutomatic { input } => {
                    Some((input.rendered().as_bytes(), SubmitInput::SeparateEnter))
                }
            };
            if let Some((bytes, submit)) = input {
                log.sent_inputs.push(SentInput {
                    pane: pane.clone(),
                    bytes: bytes.to_vec(),
                    submit,
                });
            }
        }
        result
    }

    fn observe_automatic(
        &mut self,
        pane: &SourcePaneCoordinate,
        expected: &AutomaticRecoveryExpectation,
    ) -> AutomaticPaneObservation {
        let mut log = self.log.borrow_mut();
        log.events.push("observe_automatic");
        log.automatic_observations
            .push((pane.clone(), expected.clone()));
        drop(log);
        self.automatic_results
            .pop_front()
            .expect("test script supplies one result per automatic observation")
    }

    fn paste_codex_prompt_area(
        &mut self,
        pane: &SourcePaneCoordinate,
        expected: &CodexSessionId,
        input: &CapturedCodexPromptArea,
    ) -> CodexPromptPasteResult {
        let mut log = self.log.borrow_mut();
        log.events.push("paste_codex_prompt_area");
        log.pasted_codex_prompts.push(PastedCodexPrompt {
            pane: pane.clone(),
            expected: expected.clone(),
            input: input.clone(),
        });
        drop(log);
        self.prompt_paste_results
            .pop_front()
            .expect("test script supplies one result per Codex prompt paste")
    }

    fn observe_disposition(&mut self) -> TargetDisposition {
        let mut log = self.log.borrow_mut();
        log.events.push("observe_disposition");
        log.disposition_observations += 1;
        self.final_disposition
    }
}

fn execute(plan: RestorePlan, script: TargetScript) -> (RestoreRunResult, Rc<RefCell<TargetLog>>) {
    let (target, log) = FakeTarget::new(script);
    let mut executor = RestoreExecutor::new(target);
    (executor.execute(plan), log)
}

#[test]
fn ownership_claim_race_never_exposes_topology_capability() {
    let plan = plan_with(vec![idle_pane(0)], &[]);
    let mut script = TargetScript::successful();
    script.claim_failure = Some(TargetClaimFailure::new("target appeared during claim"));

    let (result, log) = execute(plan, script);

    assert_eq!(result.status(), RestoreRunStatus::Fatal);
    assert_eq!(result.target_state(), &RestoreTargetState::NotEstablished);
    assert!(matches!(
        result.failure(),
        Some(RestoreExecutionFailure::TargetClaimFailed { .. })
    ));
    assert_eq!(log.borrow().events, ["claim"]);
    assert_eq!(
        log.borrow().claim_destinations,
        [TmuxSelector::SocketPath(
            "/tmp/tmux-rescue-restore-target.sock".into()
        )]
    );
    assert_eq!(log.borrow().topology_calls, 0);
    assert_eq!(log.borrow().rollback_calls, 0);
}

#[test]
fn ownership_claim_failure_after_creation_preserves_observed_state() {
    let plan = plan_with(vec![idle_pane(0)], &[]);
    let mut script = TargetScript::successful();
    script.claim_failure = Some(TargetClaimFailure::with_target_state(
        "ownership readback failed",
        RestoreTargetState::Observed(TargetDisposition::Unknown),
    ));

    let (result, log) = execute(plan, script);

    assert_eq!(result.status(), RestoreRunStatus::Fatal);
    assert_eq!(
        result.target_state(),
        &RestoreTargetState::Observed(TargetDisposition::Unknown)
    );
    assert!(matches!(
        result.failure(),
        Some(RestoreExecutionFailure::TargetClaimFailed { .. })
    ));
    assert_eq!(log.borrow().events, ["claim"]);
    assert_eq!(log.borrow().rollback_calls, 0);
}

#[test]
fn ownership_topology_failure_consumes_capability_for_rollback() {
    let plan = plan_with(vec![idle_pane(0)], &[]);
    let mut script = TargetScript::successful();
    script.topology_failure = Some(TopologyFailure::new("second split failed"));
    script.rollback_outcome = RollbackOutcome::Removed;

    let (result, log) = execute(plan, script);

    assert_eq!(result.status(), RestoreRunStatus::Fatal);
    assert_eq!(
        result.target_state(),
        &RestoreTargetState::Observed(TargetDisposition::Removed)
    );
    assert!(matches!(
        result.failure(),
        Some(RestoreExecutionFailure::TopologyFailed { .. })
    ));
    assert!(result.panes().is_empty());
    assert_eq!(log.borrow().events, ["claim", "topology", "rollback"]);
    assert_eq!(log.borrow().rollback_calls, 1);
    assert_eq!(log.borrow().begin_recovery_calls, 0);
}

#[test]
fn ownership_cleanup_failure_preserves_retained_or_unknown_disposition() {
    for (failure_disposition, disposition) in [
        (
            RollbackFailureDisposition::Retained,
            TargetDisposition::Retained,
        ),
        (
            RollbackFailureDisposition::Unknown,
            TargetDisposition::Unknown,
        ),
    ] {
        let plan = plan_with(vec![idle_pane(0)], &[]);
        let mut script = TargetScript::successful();
        script.topology_failure = Some(TopologyFailure::new("window creation failed"));
        script.rollback_outcome = RollbackOutcome::Failed(RollbackFailure::new(
            failure_disposition,
            "kill-server failed",
        ));

        let (result, log) = execute(plan, script);

        assert_eq!(result.status(), RestoreRunStatus::Fatal);
        assert_eq!(
            result.target_state(),
            &RestoreTargetState::Observed(disposition)
        );
        assert_eq!(log.borrow().rollback_calls, 1);
        assert_eq!(log.borrow().begin_recovery_calls, 0);
        assert!(matches!(
            result.failure(),
            Some(RestoreExecutionFailure::TopologyAndCleanupFailed {
                cleanup_failure,
                ..
            }) if cleanup_failure.message() == "kill-server failed"
        ));
    }
}

#[test]
fn guarded_input_idle_verifies_shell_without_sending_input() {
    let plan = plan_with(vec![idle_pane(0)], &[]);
    let mut script = TargetScript::successful();
    script.guarded_results.push_back(Ok(()));

    let (result, log) = execute(plan, script);

    assert_eq!(result.status(), RestoreRunStatus::Complete);
    assert_eq!(
        pane_result(&result, 0).outcome(),
        &PaneRestoreOutcome::RestoredIdleShell
    );
    assert!(log.borrow().sent_inputs.is_empty());
    assert_eq!(log.borrow().rollback_calls, 0);
    assert_eq!(log.borrow().begin_recovery_calls, 1);
}

#[test]
fn guarded_input_manual_hint_is_literal_and_has_no_enter() {
    let plan = plan_with(vec![manual_pane(0, "a'b")], &[]);
    let mut script = TargetScript::successful();
    script.guarded_results.push_back(Ok(()));

    let (result, log) = execute(plan, script);

    assert_eq!(result.status(), RestoreRunStatus::Complete);
    assert_eq!(
        pane_result(&result, 0).outcome(),
        &PaneRestoreOutcome::PreparedManualHint
    );
    assert_eq!(
        log.borrow().sent_inputs,
        [SentInput {
            pane: coordinate(0),
            bytes: b"'custom' 'a'\\''b'".to_vec(),
            submit: SubmitInput::NoEnter,
        }]
    );
}

#[test]
fn guarded_input_preflight_automatic_fallback_is_literal_and_has_no_enter() {
    let plan = plan_with(
        vec![automatic_pane(0, "1d6381bf-01c5-4c4a-b725-8e376e5ad295")],
        &[],
    );
    let mut script = TargetScript::successful();
    script.guarded_results.push_back(Ok(()));

    let (result, log) = execute(plan, script);

    assert_eq!(result.status(), RestoreRunStatus::Partial);
    assert_eq!(
        pane_result(&result, 0).outcome(),
        &PaneRestoreOutcome::PreparedAutomaticFallbackHint(
            AutomaticFallbackReason::ExecutableUnavailable
        )
    );
    assert_eq!(log.borrow().sent_inputs.len(), 1);
    assert_eq!(log.borrow().sent_inputs[0].submit, SubmitInput::NoEnter);
}

#[test]
fn automatic_recovery_without_prompt_retains_its_existing_outcome() {
    let plan = plan_with(
        vec![automatic_pane(0, "1d6381bf-01c5-4c4a-b725-8e376e5ad295")],
        &[b"codex"],
    );
    let expected = match plan.panes()[0].action() {
        tmux_rescue::PlannedPaneAction::LaunchAutomatic(launch) => launch.expectation().clone(),
        action => panic!("expected automatic launch, got {action:?}"),
    };
    let mut script = TargetScript::successful();
    script.guarded_results.push_back(Ok(()));
    script
        .automatic_results
        .push_back(AutomaticPaneObservation::Recovered);

    let (result, log) = execute(plan, script);

    assert_eq!(result.status(), RestoreRunStatus::Complete);
    assert_eq!(
        pane_result(&result, 0).outcome(),
        &PaneRestoreOutcome::RecoveredAutomatically
    );
    let log = log.borrow();
    assert_eq!(log.sent_inputs.len(), 1);
    let sent = &log.sent_inputs[0];
    assert_eq!(sent.submit, SubmitInput::SeparateEnter);
    assert!(!sent.bytes.contains(&b'\r'));
    assert!(!sent.bytes.contains(&b'\n'));
    assert_eq!(
        log.automatic_observations,
        [(coordinate(0), expected)],
        "post-launch observation must use the plan's exact recovery identity"
    );
}

#[test]
fn recovered_codex_prepares_prompt_without_enter_after_fresh_identity_check() {
    const SESSION_ID: &str = "1d6381bf-01c5-4c4a-b725-8e376e5ad295";
    const PROMPT: &str = "Review the recovery plan.\nKeep the input pending.";
    let plan = plan_with(
        vec![automatic_pane_with_prompt(0, SESSION_ID, PROMPT)],
        &[b"codex"],
    );
    let mut script = TargetScript::successful();
    script.guarded_results.push_back(Ok(()));
    script
        .automatic_results
        .push_back(AutomaticPaneObservation::Recovered);
    script.prompt_paste_results.push_back(Ok(()));

    let (result, log) = execute(plan, script);

    assert_eq!(result.status(), RestoreRunStatus::Complete);
    assert_eq!(
        pane_result(&result, 0).outcome(),
        &PaneRestoreOutcome::RecoveredAutomaticallyWithPromptPrepared
    );
    let log = log.borrow();
    assert_eq!(
        log.events,
        [
            "claim",
            "topology",
            "begin_recovery",
            "launch_automatic",
            "observe_automatic",
            "paste_codex_prompt_area",
            "observe_disposition",
        ],
        "pending input is pasted only after automatic recovery settles to the expected identity"
    );
    assert_eq!(
        log.sent_inputs.len(),
        1,
        "the prompt is not submitted as shell input"
    );
    assert_eq!(log.sent_inputs[0].submit, SubmitInput::SeparateEnter);
    assert_eq!(log.pasted_codex_prompts.len(), 1);
    assert_eq!(log.pasted_codex_prompts[0].pane, coordinate(0));
    assert_eq!(
        log.pasted_codex_prompts[0].expected.as_uuid().to_string(),
        SESSION_ID
    );
    assert_eq!(log.pasted_codex_prompts[0].input.text().as_str(), PROMPT);
}

#[test]
fn prompt_preparation_failure_is_partial_and_later_panes_continue() {
    const SESSION_ID: &str = "1d6381bf-01c5-4c4a-b725-8e376e5ad295";
    let failures = [
        CodexPromptPasteFailure::SessionMismatch,
        CodexPromptPasteFailure::PaneMissing,
        CodexPromptPasteFailure::Failed("tmux rejected paste".to_owned()),
    ];

    for failure in failures {
        let plan = plan_with(
            vec![
                automatic_pane_with_prompt(0, SESSION_ID, "Pending prompt"),
                manual_pane(1, "later"),
            ],
            &[b"codex"],
        );
        let mut script = TargetScript::successful();
        script.guarded_results.extend([Ok(()), Ok(())]);
        script
            .automatic_results
            .push_back(AutomaticPaneObservation::Recovered);
        script.prompt_paste_results.push_back(Err(failure.clone()));

        let (result, log) = execute(plan, script);

        assert_eq!(result.status(), RestoreRunStatus::Partial);
        assert_eq!(
            pane_result(&result, 0).outcome(),
            &PaneRestoreOutcome::RecoveredAutomaticallyWithPromptNeedsAttention(failure)
        );
        assert_eq!(
            pane_result(&result, 1).outcome(),
            &PaneRestoreOutcome::PreparedManualHint
        );
        let log = log.borrow();
        assert_eq!(
            log.pasted_codex_prompts.len(),
            1,
            "prompt preparation failure is never retried"
        );
        assert_eq!(
            log.guarded_attempts,
            [coordinate(0), coordinate(1)],
            "a later pane still executes after prompt preparation fails"
        );
    }
}

#[test]
fn failed_or_fallback_automatic_recovery_never_pastes_prompt_input() {
    const SESSION_ID: &str = "1d6381bf-01c5-4c4a-b725-8e376e5ad295";
    let observations = [
        AutomaticPaneObservation::ShellForeground,
        AutomaticPaneObservation::UnexpectedForeground,
        AutomaticPaneObservation::PaneMissing,
        AutomaticPaneObservation::Failed("observation failed".to_owned()),
    ];

    for observation in observations {
        let plan = plan_with(
            vec![automatic_pane_with_prompt(0, SESSION_ID, "Pending prompt")],
            &[b"codex"],
        );
        let mut script = TargetScript::successful();
        script.guarded_results.push_back(Ok(()));
        if observation == AutomaticPaneObservation::ShellForeground {
            script.guarded_results.push_back(Ok(()));
        }
        script.automatic_results.push_back(observation);

        let (_result, log) = execute(plan, script);

        assert!(
            log.borrow().pasted_codex_prompts.is_empty(),
            "non-recovered automatic branches must return before prompt preparation"
        );
    }

    let plan = plan_with(
        vec![automatic_pane_with_prompt(0, SESSION_ID, "Pending prompt")],
        &[b"codex"],
    );
    let mut script = TargetScript::successful();
    script
        .guarded_results
        .push_back(Err(GuardedPaneFailure::ShellNotForeground));

    let (_result, log) = execute(plan, script);

    assert!(
        log.borrow().pasted_codex_prompts.is_empty(),
        "a failed automatic launch must return before prompt preparation"
    );
}

#[test]
fn guarded_input_failed_automatic_launch_prepares_hint_without_enter() {
    let plan = plan_with(
        vec![automatic_pane(0, "1d6381bf-01c5-4c4a-b725-8e376e5ad295")],
        &[b"codex"],
    );
    let mut script = TargetScript::successful();
    script.guarded_results.extend([Ok(()), Ok(())]);
    script
        .automatic_results
        .push_back(AutomaticPaneObservation::ShellForeground);

    let (result, log) = execute(plan, script);

    assert_eq!(result.status(), RestoreRunStatus::Partial);
    assert_eq!(
        pane_result(&result, 0).outcome(),
        &PaneRestoreOutcome::AutomaticLaunchFailedHintPrepared
    );
    assert_eq!(log.borrow().sent_inputs.len(), 2);
    assert_eq!(
        log.borrow()
            .sent_inputs
            .iter()
            .map(|sent| sent.submit)
            .collect::<Vec<_>>(),
        [SubmitInput::SeparateEnter, SubmitInput::NoEnter]
    );
    assert_eq!(
        log.borrow().sent_inputs[0].bytes,
        log.borrow().sent_inputs[1].bytes
    );
}

#[test]
fn guarded_input_shell_change_sends_nothing_and_later_panes_continue() {
    let plan = plan_with(vec![manual_pane(0, "first"), manual_pane(1, "second")], &[]);
    let mut script = TargetScript::successful();
    script
        .guarded_results
        .extend([Err(GuardedPaneFailure::ShellNotForeground), Ok(())]);

    let (result, log) = execute(plan, script);

    assert_eq!(result.status(), RestoreRunStatus::Partial);
    assert_eq!(
        pane_result(&result, 0).outcome(),
        &PaneRestoreOutcome::NeedsAttention(AttentionReason::ShellNotForeground)
    );
    assert_eq!(
        pane_result(&result, 1).outcome(),
        &PaneRestoreOutcome::PreparedManualHint
    );
    assert_eq!(
        log.borrow()
            .sent_inputs
            .iter()
            .map(|sent| sent.pane.clone())
            .collect::<Vec<_>>(),
        [coordinate(1)]
    );
}

#[test]
fn guarded_input_unexpected_post_launch_foreground_gets_no_hint_and_continues() {
    let plan = plan_with(
        vec![
            automatic_pane(0, "1d6381bf-01c5-4c4a-b725-8e376e5ad295"),
            manual_pane(1, "later"),
        ],
        &[b"codex"],
    );
    let mut script = TargetScript::successful();
    script.guarded_results.extend([Ok(()), Ok(())]);
    script
        .automatic_results
        .push_back(AutomaticPaneObservation::UnexpectedForeground);

    let (result, log) = execute(plan, script);

    assert_eq!(result.status(), RestoreRunStatus::Partial);
    assert_eq!(
        pane_result(&result, 0).outcome(),
        &PaneRestoreOutcome::NeedsAttention(AttentionReason::UnexpectedForeground)
    );
    assert_eq!(
        pane_result(&result, 1).outcome(),
        &PaneRestoreOutcome::PreparedManualHint
    );
    assert_eq!(
        log.borrow()
            .sent_inputs
            .iter()
            .filter(|sent| sent.pane == coordinate(0))
            .count(),
        1,
        "the automatic launch is sent, but no fallback hint is typed into the new program"
    );
    assert!(
        log.borrow()
            .sent_inputs
            .iter()
            .any(|sent| sent.pane == coordinate(1)),
        "a later independent pane still runs"
    );
}

#[test]
fn guarded_input_missing_and_unavailable_panes_are_attention_and_continuation_is_preserved() {
    let plan = plan_with(
        vec![
            manual_pane(0, "missing"),
            unavailable_pane(1),
            manual_pane(2, "later"),
        ],
        &[],
    );
    let mut script = TargetScript::successful();
    script
        .guarded_results
        .extend([Err(GuardedPaneFailure::PaneMissing), Ok(())]);

    let (result, log) = execute(plan, script);

    assert_eq!(result.status(), RestoreRunStatus::Partial);
    assert_eq!(
        pane_result(&result, 0).outcome(),
        &PaneRestoreOutcome::NeedsAttention(AttentionReason::MissingPane)
    );
    assert!(matches!(
        pane_result(&result, 1).outcome(),
        PaneRestoreOutcome::NeedsAttention(
            AttentionReason::CapturedRecoveryUnavailable(failure)
        ) if failure.message() == "foreground process vanished during capture"
    ));
    assert_eq!(
        pane_result(&result, 2).outcome(),
        &PaneRestoreOutcome::PreparedManualHint
    );
    assert_eq!(
        log.borrow().guarded_attempts,
        [coordinate(0), coordinate(2)],
        "unavailable recovery sends no input and does not invoke the guard"
    );
    assert_eq!(
        log.borrow()
            .sent_inputs
            .iter()
            .map(|sent| sent.pane.clone())
            .collect::<Vec<_>>(),
        [coordinate(2)]
    );
    assert_eq!(log.borrow().rollback_calls, 0);
    assert_eq!(log.borrow().begin_recovery_calls, 1);
    assert_eq!(log.borrow().disposition_observations, 1);
    assert_eq!(
        result.target_state(),
        &RestoreTargetState::Observed(TargetDisposition::Retained)
    );
}
