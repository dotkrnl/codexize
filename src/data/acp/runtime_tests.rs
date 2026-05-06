use super::*;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

struct StubSession {
    id: String,
    updates: VecDeque<ClientUpdate>,
    closed: Arc<Mutex<bool>>,
}

impl AcpSession for StubSession {
    fn session_id(&self) -> &str {
        &self.id
    }

    fn try_next_update(&mut self) -> AcpResult<Option<ClientUpdate>> {
        Ok(self.updates.pop_front())
    }

    fn submit_prompt(&mut self, _text: &str) -> AcpResult<()> {
        Ok(())
    }

    fn cancel_prompt(&mut self) -> AcpResult<()> {
        Ok(())
    }

    fn close(&mut self) -> AcpResult<()> {
        *self
            .closed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        Ok(())
    }
}

struct StubConnector {
    updates: VecDeque<ClientUpdate>,
    closed: Arc<Mutex<bool>>,
}

impl StubConnector {
    fn new(updates: impl IntoIterator<Item = ClientUpdate>) -> Self {
        Self {
            updates: updates.into_iter().collect(),
            closed: Arc::new(Mutex::new(false)),
        }
    }
}

impl AcpConnector for StubConnector {
    fn connect(&self, _launch: &AcpResolvedLaunch) -> AcpResult<Box<dyn AcpSession>> {
        Ok(Box::new(StubSession {
            id: "sess-test".to_string(),
            updates: self.updates.clone(),
            closed: Arc::clone(&self.closed),
        }))
    }
}

fn sample_request(vendor: VendorKind) -> AcpLaunchRequest {
    AcpLaunchRequest {
        vendor,
        cwd: PathBuf::from("/tmp/project"),
        prompt: PromptPayload::Text("implement".to_string()),
        model: "model-x".to_string(),
        requested_effort: EffortLevel::Normal,
        effective_effort: EffortLevel::Low,
        interactive: false,
        modes: LaunchModes {
            yolo: true,
            cheap: true,
            interactive: false,
        },
        required_artifacts: vec![PathBuf::from("/tmp/project/summary.toml")],
        policy: AcpLaunchPolicy::default(),
    }
}

#[test]
fn runtime_interface_compiles_without_real_agent_binaries() {
    let connector = StubConnector::new([
        ClientUpdate::AgentMessageText {
            text: "hello".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        },
        ClientUpdate::PromptTurnFinished,
    ]);
    let closed = Arc::clone(&connector.closed);
    let mut runtime = AcpRuntime::with_connector(AcpConfig::default(), connector);

    let mut run = runtime
        .start_run(sample_request(VendorKind::Codex))
        .expect("start run");
    assert_eq!(run.session_id(), "sess-test");

    let ready = run.next_event().expect("ready event");
    assert!(matches!(
        ready,
        Some(AcpRuntimeEvent::Lifecycle(
            AcpLifecycleEvent::SessionReady { .. }
        ))
    ));

    let text = run.next_event().expect("text event");
    assert!(matches!(
        text,
        Some(AcpRuntimeEvent::Text(AcpTextEvent {
            text,
            interactive: false,
            thought: false,
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        })) if text == "hello"
    ));

    let completion = run.next_event().expect("completion event");
    assert!(matches!(
        completion,
        Some(AcpRuntimeEvent::Completion(
            AcpCompletionEvent::PromptTurnFinished
        ))
    ));

    run.close().expect("close run");
    assert!(!runtime.is_busy());
    assert!(
        *closed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    );
}

#[test]
fn dropping_a_run_clears_busy_state() {
    let connector = StubConnector::new(std::iter::empty::<ClientUpdate>());
    let mut runtime = AcpRuntime::with_connector(AcpConfig::default(), connector);

    {
        let _run = runtime
            .start_run(sample_request(VendorKind::Claude))
            .expect("first run");
    }

    assert!(!runtime.is_busy());
}
