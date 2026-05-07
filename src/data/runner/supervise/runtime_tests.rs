use super::*;
use crate::acp::{
    AcpLaunchPolicy, AcpPermissionMode, AcpReasoningEffort, AcpResolvedLaunch, AcpResult,
    AcpSession, AcpSessionSpec, AcpSpawnSpec, ClientUpdate, PromptPayload,
};
use crate::data::runner::transport::{FakeAcpClock, ManagedAcpLaunch, RealAcpDiagnostics};
use crate::selection::VendorKind;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Scripted in-memory `AcpSession` that returns a fixed sequence of updates
/// and records calls to `submit_prompt` / `cancel_prompt` / `close`.
struct FakeSession {
    updates: VecDeque<ClientUpdate>,
    submitted: Vec<String>,
    cancel_calls: u32,
    closed: bool,
    dead_reason: Option<String>,
    /// If set, `dead_reason` is suppressed (returns None) until
    /// `cancel_calls` reaches this value, then it becomes active.
    dead_after_cancel_calls: Option<u32>,
}

impl FakeSession {
    fn new(updates: Vec<ClientUpdate>) -> Self {
        Self {
            updates: updates.into(),
            submitted: Vec::new(),
            cancel_calls: 0,
            closed: false,
            dead_reason: None,
            dead_after_cancel_calls: None,
        }
    }

    fn with_dead_reason(mut self, reason: &str) -> Self {
        self.dead_reason = Some(reason.to_string());
        self
    }

    fn with_dead_after_cancel_calls(mut self, reason: &str, threshold: u32) -> Self {
        self.dead_reason = Some(reason.to_string());
        self.dead_after_cancel_calls = Some(threshold);
        self
    }

    fn dead_reason_active(&self) -> bool {
        match self.dead_after_cancel_calls {
            Some(threshold) => self.cancel_calls >= threshold,
            None => true,
        }
    }
}

impl AcpSession for FakeSession {
    fn session_id(&self) -> &str {
        "fake-session"
    }

    fn try_next_update(&mut self) -> AcpResult<Option<ClientUpdate>> {
        Ok(self.updates.pop_front())
    }

    fn submit_prompt(&mut self, text: &str) -> AcpResult<()> {
        self.submitted.push(text.to_string());
        Ok(())
    }

    fn cancel_prompt(&mut self) -> AcpResult<()> {
        self.cancel_calls += 1;
        Ok(())
    }

    fn close(&mut self) -> AcpResult<()> {
        self.closed = true;
        Ok(())
    }

    fn dead_reason(&mut self) -> AcpResult<Option<String>> {
        if self.dead_reason_active() {
            Ok(self.dead_reason.clone())
        } else {
            Ok(None)
        }
    }
}

fn launch_fixture(interactive: bool) -> ManagedAcpLaunch {
    ManagedAcpLaunch {
        resolved: AcpResolvedLaunch {
            vendor: VendorKind::Kimi,
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
// Existing watchog / dead-child tests (updated for clock-injectable API)
// ---------------------------------------------------------------------------

#[test]
fn non_interactive_interrupt_resubmits_warning_then_finishes() {
    let updates = vec![
        ClientUpdate::PromptTurnFailed {
            message: "cancelled".to_string(),
        },
        ClientUpdate::PromptTurnFinished,
    ];
    let session: Box<dyn AcpSession> = Box::new(FakeSession::new(updates));

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    input_tx
        .send(AcpInput::Interrupt("watchdog warning text".to_string()))
        .unwrap();

    let clock = fake_clock();
    let outcome = drive_acp_session_with_clock(
        session,
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &RealAcpDiagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.signal_received, "");
}

#[test]
fn silent_child_exit_during_idle_polls_breaks_the_loop() {
    let session: Box<dyn AcpSession> =
        Box::new(FakeSession::new(Vec::new()).with_dead_reason("child crashed mid-turn"));

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (_input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    let clock = fake_clock();
    let outcome = drive_acp_session_with_clock(
        session,
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &RealAcpDiagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn non_interactive_failure_without_pending_input_still_exits_one() {
    let updates = vec![ClientUpdate::PromptTurnFailed {
        message: "model error".to_string(),
    }];
    let session: Box<dyn AcpSession> = Box::new(FakeSession::new(updates));

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (_input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    let clock = fake_clock();
    let outcome = drive_acp_session_with_clock(
        session,
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &RealAcpDiagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 1);
}

// ---------------------------------------------------------------------------
// New tests: Finished resubmit + cancel-ack watchdog
// ---------------------------------------------------------------------------

#[test]
fn non_interactive_finished_resubmits_queued_interrupt_text() {
    // A non-interactive run is interrupted, but the vendor's turn finishes
    // with PromptTurnFinished before the cancel takes effect. The queued
    // interrupt text must be submitted as the next turn instead of the
    // session closing with exit_code 0.
    let session: Box<dyn AcpSession> = Box::new(FakeSession::new(vec![
        ClientUpdate::PromptTurnFinished,
        ClientUpdate::PromptTurnFinished,
    ]));

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    input_tx
        .send(AcpInput::Interrupt("new instructions".to_string()))
        .unwrap();

    let clock = fake_clock();
    let outcome = drive_acp_session_with_clock(
        session,
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &RealAcpDiagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.signal_received, "");
}

#[test]
fn cancel_ack_timeout_resends_then_terminates() {
    // A vendor ignores session/cancel entirely (returns None forever, no
    // dead_reason). The cancel-ack watchdog must resend cancel after 60 s
    // (phase 1) and signal Terminate after another 60 s (phase 2),
    // producing exit_code 143.
    let session: Box<dyn AcpSession> = Box::new(FakeSession::new(Vec::new()));

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    input_tx
        .send(AcpInput::Interrupt("watchdog warning".to_string()))
        .unwrap();

    let clock = fake_clock();
    let outcome = drive_acp_session_with_clock(
        session,
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &RealAcpDiagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 143);
    assert_eq!(outcome.signal_received, "TERM");
}

#[test]
fn cancel_ack_timer_disarmed_by_prompt_turn_failed_before_timeout() {
    // When PromptTurnFailed arrives before the 60 s cancel-ack timer fires,
    // the timer is disarmed and the queued text is resubmitted. No cancel
    // resend should occur because the timer never reaches its threshold.
    // dead_after_cancel_calls gates the dead_reason so it only becomes
    // active if the cancel-ack resend actually fires.
    let session: Box<dyn AcpSession> = Box::new(
        FakeSession::new(vec![
            ClientUpdate::PromptTurnFailed {
                message: "cancelled".to_string(),
            },
            ClientUpdate::PromptTurnFinished,
        ])
        .with_dead_after_cancel_calls("stuck after resend", 2),
    );

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    input_tx
        .send(AcpInput::Interrupt("preempt".to_string()))
        .unwrap();

    let clock = fake_clock();
    let outcome = drive_acp_session_with_clock(
        session,
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &RealAcpDiagnostics,
    )
    .expect("loop returns outcome");

    // PromptTurnFailed disarmed the timer and resubmitted; the resubmitted
    // turn finished normally with PromptTurnFinished, producing exit_code 0.
    assert_eq!(outcome.exit_code, 0);
    // Cancel should have been called exactly once (the initial interrupt).
    // The dead_reason was never activated because cancel_calls stayed at 1.
}

#[test]
fn second_interrupt_while_pending_does_not_reset_cancel_ack_timer() {
    // Two interrupts arrive: the first arms the cancel-ack timer, the
    // second only appends to pending_input without resetting the timer.
    // After PromptTurnFailed disarms the timer and resubmits the first
    // queued text, the second text remains pending. Another PromptTurnFailed
    // exits. Cancel should be called exactly once.
    let session: Box<dyn AcpSession> = Box::new(FakeSession::new(vec![
        ClientUpdate::PromptTurnFailed {
            message: "cancelled".to_string(),
        },
        ClientUpdate::PromptTurnFailed {
            message: "cancelled".to_string(),
        },
    ]));

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    // First interrupt → arms the timer
    input_tx
        .send(AcpInput::Interrupt("first".to_string()))
        .unwrap();
    // Second interrupt → only appends text (interrupting_turn is already
    // true, so no new cancel is sent and the timer is not reset).
    input_tx
        .send(AcpInput::Interrupt("second".to_string()))
        .unwrap();

    let clock = fake_clock();
    let outcome = drive_acp_session_with_clock(
        session,
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &RealAcpDiagnostics,
    )
    .expect("loop returns outcome");

    // After resubmitting "first" on the first PromptTurnFailed, "second"
    // is still pending. The second PromptTurnFailed exits with code 1
    // because interrupting_turn was cleared by the resubmit.
    assert_eq!(outcome.exit_code, 1);
}

#[test]
fn interrupt_finished_resubmit_then_later_interrupt_arms_fresh_cancel() {
    // Full cycle: interrupt → PromptTurnFinished → resubmit queued text
    // (timer disarmed) → later interrupt (new timer armed) → vendor finishes.
    let session: Box<dyn AcpSession> = Box::new(FakeSession::new(vec![
        ClientUpdate::PromptTurnFinished, // triggers non-interactive resubmit
        ClientUpdate::PromptTurnFinished, // resubmitted turn finishes
    ]));

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    input_tx
        .send(AcpInput::Interrupt("first interrupt".to_string()))
        .unwrap();

    let clock = fake_clock();
    let outcome = drive_acp_session_with_clock(
        session,
        &launch,
        &cancel,
        &mut input_rx,
        &waiting_tx,
        &clock,
        &RealAcpDiagnostics,
    )
    .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 0);
    assert_eq!(outcome.signal_received, "");
}
