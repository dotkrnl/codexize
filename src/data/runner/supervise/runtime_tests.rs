use super::*;
use crate::data::acp::{
    AcpLaunchPolicy, AcpPermissionMode, AcpReasoningEffort, AcpResolvedLaunch, AcpResult,
    AcpSession, AcpSessionSpec, AcpSpawnSpec, ClientUpdate, PromptPayload,
};
use crate::data::runner::transport::{FakeAcpClock, FakeAcpDiagnostics, ManagedAcpLaunch};
use crate::selection::CliKind;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// One step in a `FakeSession` script. `Update` is delivered through
/// `try_next_update`; `InjectInput` is fed back into the runtime's input
/// channel before the next update is returned, modeling an operator
/// sending another `:interrupt` mid-loop.
#[derive(Clone)]
enum ScriptStep {
    Update(ClientUpdate),
    InjectInput(AcpInput),
}

/// Mutable state shared between the test body and the in-memory
/// `FakeSession`. `Arc<Mutex<…>>` (rather than `Rc<RefCell<…>>`) is
/// required because `AcpSession: Send`, and a `Box<dyn AcpSession>`
/// inherits that bound through coercion.
#[derive(Default)]
struct FakeSessionState {
    script: VecDeque<ScriptStep>,
    submitted: Vec<String>,
    cancel_calls: u32,
    closed: bool,
    dead_reason: Option<String>,
    /// Suppresses `dead_reason` (returns None) until `cancel_calls`
    /// reaches this threshold. Lets a test prove that a particular
    /// fallback path was *not* taken by gating it on a cancel count
    /// that should never be reached.
    dead_after_cancel_calls: Option<u32>,
}

#[derive(Default, Clone)]
struct FakeSessionHandle {
    state: Arc<Mutex<FakeSessionState>>,
    /// Optional sender used by `ScriptStep::InjectInput` to push more
    /// `AcpInput` values back into the runtime's input channel without
    /// the test needing to interleave loop ticks.
    input_tx: Option<mpsc::UnboundedSender<AcpInput>>,
}

impl FakeSessionHandle {
    fn new(updates: Vec<ClientUpdate>) -> Self {
        let script = updates.into_iter().map(ScriptStep::Update).collect();
        Self {
            state: Arc::new(Mutex::new(FakeSessionState {
                script,
                ..Default::default()
            })),
            input_tx: None,
        }
    }

    fn with_script(steps: Vec<ScriptStep>) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeSessionState {
                script: steps.into(),
                ..Default::default()
            })),
            input_tx: None,
        }
    }

    fn with_input_tx(mut self, tx: mpsc::UnboundedSender<AcpInput>) -> Self {
        self.input_tx = Some(tx);
        self
    }

    fn with_dead_reason(self, reason: &str) -> Self {
        self.state.lock().unwrap().dead_reason = Some(reason.to_string());
        self
    }

    fn with_dead_after_cancel_calls(self, reason: &str, threshold: u32) -> Self {
        let mut state = self.state.lock().unwrap();
        state.dead_reason = Some(reason.to_string());
        state.dead_after_cancel_calls = Some(threshold);
        drop(state);
        self
    }

    fn submitted(&self) -> Vec<String> {
        self.state.lock().unwrap().submitted.clone()
    }

    fn cancel_calls(&self) -> u32 {
        self.state.lock().unwrap().cancel_calls
    }

    fn closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    fn into_session(self) -> Box<dyn AcpSession> {
        Box::new(FakeSession { handle: self })
    }
}

/// Scripted in-memory `AcpSession` that delegates to a shared
/// `FakeSessionHandle` so the test body can inspect submitted prompts
/// and cancel-call counts after `drive_acp_session_with_clock` returns.
struct FakeSession {
    handle: FakeSessionHandle,
}

impl AcpSession for FakeSession {
    fn session_id(&self) -> &str {
        "fake-session"
    }

    fn try_next_update(&mut self) -> AcpResult<Option<ClientUpdate>> {
        // `InjectInput` steps are flushed to the input channel and the
        // poll returns `None` for that iteration so the runtime's next
        // `try_recv` picks up the injected text before the following
        // update is delivered.
        let mut state = self.handle.state.lock().unwrap();
        match state.script.pop_front() {
            Some(ScriptStep::Update(update)) => Ok(Some(update)),
            Some(ScriptStep::InjectInput(input)) => {
                drop(state);
                if let Some(tx) = self.handle.input_tx.as_ref() {
                    let _ = tx.send(input);
                }
                Ok(None)
            }
            None => Ok(None),
        }
    }

    fn submit_prompt(&mut self, text: &str) -> AcpResult<()> {
        self.handle
            .state
            .lock()
            .unwrap()
            .submitted
            .push(text.to_string());
        Ok(())
    }

    fn cancel_prompt(&mut self) -> AcpResult<()> {
        self.handle.state.lock().unwrap().cancel_calls += 1;
        Ok(())
    }

    fn close(&mut self) -> AcpResult<()> {
        self.handle.state.lock().unwrap().closed = true;
        Ok(())
    }

    fn dead_reason(&mut self) -> AcpResult<Option<String>> {
        let state = self.handle.state.lock().unwrap();
        let active = match state.dead_after_cancel_calls {
            Some(threshold) => state.cancel_calls >= threshold,
            None => true,
        };
        Ok(if active {
            state.dead_reason.clone()
        } else {
            None
        })
    }
}

fn launch_fixture(interactive: bool) -> ManagedAcpLaunch {
    ManagedAcpLaunch {
        resolved: AcpResolvedLaunch {
            cli: CliKind::Kimi,
            interactive,
            spawn: AcpSpawnSpec {
                program: String::new(),
                args: Vec::new(),
                env: BTreeMap::new(),
            },
            session: AcpSessionSpec {
                cwd: PathBuf::from("/tmp"),
                prompt: PromptPayload::Text(String::new()),
                model: String::new(),
                reasoning_effort: AcpReasoningEffort::Medium,
                permission_mode: AcpPermissionMode::Code,
                policy: AcpLaunchPolicy::default(),
                metadata: BTreeMap::new(),
            },
        },
        window_name: "[Test]".to_string(),
        session_id: None,
        stamp_path: PathBuf::from("/tmp/codexize-test-stamp.toml"),
        cause_path: PathBuf::from("/tmp/codexize-test-cause.txt"),
        required_artifact: None,
    }
}

fn cancel_signal() -> CancelSignal {
    CancelSignal::new(CancellationToken::new())
}

fn fake_clock() -> FakeAcpClock {
    FakeAcpClock {
        now: std::cell::Cell::new(Instant::now()),
    }
}

// ---------------------------------------------------------------------------
// Existing watchdog / dead-child tests (updated for shared-state harness)
// ---------------------------------------------------------------------------

#[test]
fn non_interactive_interrupt_resubmits_warning_then_finishes() {
    let handle = FakeSessionHandle::new(vec![
        ClientUpdate::PromptTurnFailed {
            message: "cancelled".to_string(),
        },
        ClientUpdate::PromptTurnFinished,
    ]);
    let session_handle = handle.clone();

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    input_tx
        .send(AcpInput::Interrupt("watchdog warning text".to_string()))
        .unwrap();

    let clock = fake_clock();
    let diagnostics = FakeAcpDiagnostics::new();
    let outcome = drive_acp_session_with_clock(
        handle.into_session(),
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &diagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.signal_received, "");
    assert_eq!(
        session_handle.submitted(),
        vec!["watchdog warning text".to_string()],
        "PromptTurnFailed must resubmit the queued interrupt text",
    );
    assert_eq!(session_handle.cancel_calls(), 1);
    assert!(session_handle.closed());
}

#[test]
fn silent_child_exit_during_idle_polls_breaks_the_loop() {
    let handle = FakeSessionHandle::new(Vec::new()).with_dead_reason("child crashed mid-turn");

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (_input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    let clock = fake_clock();
    let diagnostics = FakeAcpDiagnostics::new();
    let outcome = drive_acp_session_with_clock(
        handle.into_session(),
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &diagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn non_interactive_failure_without_pending_input_still_exits_one() {
    let handle = FakeSessionHandle::new(vec![ClientUpdate::PromptTurnFailed {
        message: "model error".to_string(),
    }]);
    let session_handle = handle.clone();

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (_input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    let clock = fake_clock();
    let diagnostics = FakeAcpDiagnostics::new();
    let outcome = drive_acp_session_with_clock(
        handle.into_session(),
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &diagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 1);
    assert_eq!(session_handle.cancel_calls(), 0);
    assert!(session_handle.submitted().is_empty());
}

// ---------------------------------------------------------------------------
// New tests: Finished resubmit + cancel-ack watchdog
// ---------------------------------------------------------------------------

#[test]
fn non_interactive_finished_resubmits_queued_interrupt_text() {
    // A non-interactive run is interrupted, but the vendor's turn finishes
    // with PromptTurnFinished before the cancel takes effect. The queued
    // interrupt text must be submitted as the next turn instead of the
    // session closing with exit_code 0; the resubmitted turn then naturally
    // finishes on the second PromptTurnFinished.
    let handle = FakeSessionHandle::new(vec![
        ClientUpdate::PromptTurnFinished,
        ClientUpdate::PromptTurnFinished,
    ]);
    let session_handle = handle.clone();

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    input_tx
        .send(AcpInput::Interrupt("new instructions".to_string()))
        .unwrap();

    let clock = fake_clock();
    let diagnostics = FakeAcpDiagnostics::new();
    let outcome = drive_acp_session_with_clock(
        handle.into_session(),
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &diagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.signal_received, "");
    // Behavioral proof: the queued :interrupt text was actually submitted
    // as the next prompt rather than silently dropped on close.
    assert_eq!(
        session_handle.submitted(),
        vec!["new instructions".to_string()],
    );
    assert_eq!(session_handle.cancel_calls(), 1);
    assert!(session_handle.closed());
}

#[test]
fn cancel_ack_timeout_resends_then_terminates() {
    // A vendor ignores session/cancel entirely (returns None forever, no
    // dead_reason). The cancel-ack watchdog must resend cancel after 60 s
    // (stage 1) and signal Terminate after another 60 s (stage 2),
    // producing exit_code 143.
    let handle = FakeSessionHandle::new(Vec::new());
    let session_handle = handle.clone();

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    input_tx
        .send(AcpInput::Interrupt("watchdog warning".to_string()))
        .unwrap();

    let clock = fake_clock();
    let diagnostics = FakeAcpDiagnostics::new();
    let outcome = drive_acp_session_with_clock(
        handle.into_session(),
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &diagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 143);
    assert_eq!(outcome.signal_received, "TERM");
    // Two cancel calls: the initial one armed by the :interrupt and the
    // 60-second resend driven by the cancel-ack watchdog.
    assert_eq!(session_handle.cancel_calls(), 2);
    assert!(session_handle.closed());
    // Both stages persisted SummaryWarn dashboard messages.
    let warnings = diagnostics.warnings();
    assert_eq!(warnings.len(), 2, "expected 60s + 120s warnings");
    assert!(warnings[0].contains("60s"));
    assert!(warnings[1].contains("120s"));
}

#[test]
fn cancel_ack_timer_disarmed_by_prompt_turn_failed_before_timeout() {
    // When PromptTurnFailed arrives before the 60 s cancel-ack timer fires,
    // the timer is disarmed and the queued text is resubmitted. No cancel
    // resend should occur because the timer never reaches its threshold.
    // dead_after_cancel_calls gates the dead_reason so it would only become
    // active if cancel_prompt fired a second time — which it must not.
    let handle = FakeSessionHandle::new(vec![
        ClientUpdate::PromptTurnFailed {
            message: "cancelled".to_string(),
        },
        ClientUpdate::PromptTurnFinished,
    ])
    .with_dead_after_cancel_calls("stuck after resend", 2);
    let session_handle = handle.clone();

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    input_tx
        .send(AcpInput::Interrupt("preempt".to_string()))
        .unwrap();

    let clock = fake_clock();
    let diagnostics = FakeAcpDiagnostics::new();
    let outcome = drive_acp_session_with_clock(
        handle.into_session(),
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &diagnostics,
    )
    .expect("loop returns outcome");

    // PromptTurnFailed disarmed the timer and resubmitted; the resubmitted
    // turn finished normally with PromptTurnFinished, producing exit_code 0.
    assert_eq!(outcome.exit_code, 0);
    // Cancel must have been called exactly once (the initial interrupt).
    assert_eq!(session_handle.cancel_calls(), 1);
    assert_eq!(session_handle.submitted(), vec!["preempt".to_string()]);
    // No cancel-ack diagnostics fired because the timer was disarmed first.
    assert!(diagnostics.warnings().is_empty());
}

#[test]
fn second_interrupt_while_pending_does_not_reset_cancel_ack_timer() {
    // Two interrupts arrive: the first arms the cancel-ack timer, the
    // second only appends to pending_input without resetting the timer.
    // The vendor never emits a terminal event, so the timer must still
    // fire stage 1 (resend) and stage 2 (terminate) even though a second
    // :interrupt was queued well after the initial cancel.
    let handle = FakeSessionHandle::new(Vec::new());
    let session_handle = handle.clone();

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    input_tx
        .send(AcpInput::Interrupt("first".to_string()))
        .unwrap();
    input_tx
        .send(AcpInput::Interrupt("second".to_string()))
        .unwrap();

    let clock = fake_clock();
    let diagnostics = FakeAcpDiagnostics::new();
    let outcome = drive_acp_session_with_clock(
        handle.into_session(),
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &diagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 143);
    // Exactly two cancel calls: the initial cancel (armed by the first
    // :interrupt) and the cancel-ack watchdog's 60s resend. The second
    // :interrupt did not arm a fresh cancel and did not reset the timer,
    // so stage 2 still terminates after the original 120s budget.
    assert_eq!(session_handle.cancel_calls(), 2);
    // SummaryWarn fired twice — once at 60s, once at 120s.
    assert_eq!(diagnostics.warnings().len(), 2);
    // Neither queued interrupt text reached submit_prompt because the run
    // terminated before any PromptTurnFinished/Failed could resubmit.
    assert!(session_handle.submitted().is_empty());
}

#[test]
fn interrupt_finished_resubmit_then_later_interrupt_arms_fresh_cancel() {
    // Full cycle: interrupt → PromptTurnFinished → resubmit queued text
    // (timer disarmed) → second :interrupt injected mid-script → fresh
    // cancel armed → PromptTurnFinished closes the run.
    //
    // The mid-script `InjectInput` step lets us send a second :interrupt
    // *after* the runtime has consumed the first PromptTurnFinished and
    // resubmitted "first instructions". `try_next_update` returns None
    // for that step so the runtime's next iteration picks up the
    // injected text from the input channel before the second
    // PromptTurnFinished is delivered.
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let handle = FakeSessionHandle::with_script(vec![
        // Turn 1 finishes → resubmit "first instructions" (the pre-loop
        // :interrupt arrived with this turn already in flight).
        ScriptStep::Update(ClientUpdate::PromptTurnFinished),
        // Operator sends a second :interrupt while turn 2 is running.
        ScriptStep::InjectInput(AcpInput::Interrupt("second instructions".to_string())),
        // Turn 2 (the resubmitted first) finishes; the second :interrupt
        // is now queued and `interrupting_turn` was set in the same
        // iteration, so the runtime resubmits "second instructions".
        ScriptStep::Update(ClientUpdate::PromptTurnFinished),
        // Turn 3 (the resubmitted second) finishes naturally with no
        // queued input, so the runtime closes and exits 0.
        ScriptStep::Update(ClientUpdate::PromptTurnFinished),
    ])
    .with_input_tx(input_tx.clone());
    let session_handle = handle.clone();

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    input_tx
        .send(AcpInput::Interrupt("first instructions".to_string()))
        .unwrap();

    let clock = fake_clock();
    let diagnostics = FakeAcpDiagnostics::new();
    let outcome = drive_acp_session_with_clock(
        handle.into_session(),
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &diagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.signal_received, "");
    // Both interrupt texts reached submit_prompt: the first through the
    // PromptTurnFinished resubmit branch, the second through the fresh
    // cancel + PromptTurnFinished resubmit.
    assert_eq!(
        session_handle.submitted(),
        vec![
            "first instructions".to_string(),
            "second instructions".to_string(),
        ],
    );
    // Two cancels: one for each :interrupt. The second cancel proves the
    // post-resubmit reset of `interrupting_turn` armed a fresh cancel-ack
    // timer rather than swallowing the later interrupt.
    assert_eq!(session_handle.cancel_calls(), 2);
    assert!(session_handle.closed());
    // Neither cancel-ack timer reached its threshold because the vendor
    // emitted PromptTurnFinished promptly each time.
    assert!(diagnostics.warnings().is_empty());
}
