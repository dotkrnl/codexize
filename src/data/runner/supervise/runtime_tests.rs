use super::*;
use crate::acp::{
    AcpLaunchPolicy, AcpPermissionMode, AcpReasoningEffort, AcpResolvedLaunch, AcpResult,
    AcpSession, AcpSessionSpec, AcpSpawnSpec, ClientUpdate, PromptPayload,
};
use crate::data::runner::transport::ManagedAcpLaunch;
use crate::selection::VendorKind;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Scripted in-memory `AcpSession` that returns a fixed sequence of updates
/// and records calls to `submit_prompt` / `cancel_prompt` / `close`.
struct FakeSession {
    updates: VecDeque<ClientUpdate>,
    submitted: Vec<String>,
    cancel_calls: u32,
    closed: bool,
}

impl FakeSession {
    fn new(updates: Vec<ClientUpdate>) -> Self {
        Self {
            updates: updates.into(),
            submitted: Vec::new(),
            cancel_calls: 0,
            closed: false,
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

#[test]
fn non_interactive_interrupt_resubmits_warning_then_finishes() {
    // Watchdog flow: an in-flight non-interactive turn is interrupted with a
    // warning string, the vendor responds with PromptTurnFailed, then the
    // resumed turn ends with PromptTurnFinished. Without the fix, the runtime
    // would close the session and exit(1) the moment the failure arrived,
    // losing the warning text and killing the run.
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

    let outcome = drive_acp_session(session, &launch, &cancel, &mut input_rx, &waiting_tx)
        .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 0, "non-interactive interrupt + resume should finish gracefully");
    assert_eq!(outcome.signal_received, "");
}

#[test]
fn non_interactive_failure_without_pending_input_still_exits_one() {
    // Vanilla failure without any interrupt-with-text in flight should keep
    // the existing exit(1) behavior.
    let updates = vec![ClientUpdate::PromptTurnFailed {
        message: "model error".to_string(),
    }];
    let session: Box<dyn AcpSession> = Box::new(FakeSession::new(updates));

    let launch = launch_fixture(false);
    let cancel = cancel_signal();
    let (_input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (waiting_tx, _waiting_rx) = watch::channel(false);

    let outcome = drive_acp_session(session, &launch, &cancel, &mut input_rx, &waiting_tx)
        .expect("loop returns outcome");

    assert_eq!(outcome.exit_code, 1);
}
