mod client;
mod config;
mod events;

pub use client::{AcpConnector, AcpSession, SubprocessConnector};
pub use config::{AcpAgentDefinition, AcpConfig, program_is_executable};
pub use events::{
    AcpCompletionEvent, AcpLifecycleEvent, AcpRuntimeEvent, AcpTextAccumulator, AcpTextEvent,
    ClientUpdate, translate_update,
};

use crate::{adapters::EffortLevel, selection::VendorKind, state::LaunchModes};
use std::{collections::BTreeMap, path::PathBuf};

pub type AcpResult<T> = Result<T, AcpError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpError {
    HumanBlock(String),
    Busy(String),
    Io(String),
    Protocol(String),
}

impl AcpError {
    pub fn human_block(message: impl Into<String>) -> Self {
        Self::HumanBlock(message.into())
    }

    pub fn busy(message: impl Into<String>) -> Self {
        Self::Busy(message.into())
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(message.into())
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::Io(message.into())
    }
}

impl std::fmt::Display for AcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HumanBlock(message)
            | Self::Busy(message)
            | Self::Io(message)
            | Self::Protocol(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for AcpError {}

impl From<std::io::Error> for AcpError {
    fn from(value: std::io::Error) -> Self {
        Self::io(value.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptPayload {
    Text(String),
    File(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpReasoningEffort {
    Low,
    Medium,
    High,
}

impl AcpReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpPermissionMode {
    Ask,
    Code,
}

impl AcpPermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Code => "code",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpLaunchRequest {
    pub vendor: VendorKind,
    pub cwd: PathBuf,
    pub prompt: PromptPayload,
    pub model: String,
    pub requested_effort: EffortLevel,
    pub effective_effort: EffortLevel,
    pub interactive: bool,
    pub modes: LaunchModes,
    pub required_artifacts: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpSpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpSessionSpec {
    pub cwd: PathBuf,
    pub prompt: PromptPayload,
    pub model: String,
    pub requested_effort: EffortLevel,
    pub effective_effort: EffortLevel,
    pub reasoning_effort: AcpReasoningEffort,
    pub permission_mode: AcpPermissionMode,
    pub interactive: bool,
    pub modes: LaunchModes,
    pub required_artifacts: Vec<PathBuf>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpResolvedLaunch {
    pub vendor: VendorKind,
    pub interactive: bool,
    pub spawn: AcpSpawnSpec,
    pub session: AcpSessionSpec,
}

pub struct AcpRuntime<C = SubprocessConnector> {
    config: AcpConfig,
    connector: C,
    active_session_id: Option<String>,
}

impl AcpRuntime<SubprocessConnector> {
    pub fn new(config: AcpConfig) -> Self {
        Self::with_connector(config, SubprocessConnector)
    }
}

impl<C> AcpRuntime<C> {
    pub fn with_connector(config: AcpConfig, connector: C) -> Self {
        Self {
            config,
            connector,
            active_session_id: None,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.active_session_id.is_some()
    }

    pub fn prepare_launch(&self, request: &AcpLaunchRequest) -> AcpResult<AcpResolvedLaunch> {
        self.config.resolve(request)
    }
}

impl<C: AcpConnector> AcpRuntime<C> {
    pub fn start_run<'runtime>(
        &'runtime mut self,
        request: AcpLaunchRequest,
    ) -> AcpResult<AcpActiveRun<'runtime, C>> {
        if self.active_session_id.is_some() {
            return Err(AcpError::busy(
                "codexize only supports one active ACP run at a time",
            ));
        }

        let resolved = self.prepare_launch(&request)?;
        let session = self.connector.connect(&resolved)?;
        self.active_session_id = Some(session.session_id().to_string());
        Ok(AcpActiveRun {
            runtime: self,
            session: Some(session),
            resolved,
            emitted_ready: false,
        })
    }
}

pub struct AcpActiveRun<'runtime, C> {
    runtime: &'runtime mut AcpRuntime<C>,
    session: Option<Box<dyn AcpSession>>,
    resolved: AcpResolvedLaunch,
    emitted_ready: bool,
}

impl<C> AcpActiveRun<'_, C> {
    pub fn session_id(&self) -> &str {
        self.session
            .as_ref()
            .expect("session available")
            .session_id()
    }

    pub fn resolved_launch(&self) -> &AcpResolvedLaunch {
        &self.resolved
    }
}

impl<C> AcpActiveRun<'_, C> {
    pub fn next_event(&mut self) -> AcpResult<Option<AcpRuntimeEvent>> {
        if !self.emitted_ready {
            self.emitted_ready = true;
            return Ok(Some(AcpRuntimeEvent::Lifecycle(
                AcpLifecycleEvent::SessionReady {
                    session_id: self.session_id().to_string(),
                    vendor: self.resolved.vendor,
                },
            )));
        }

        let update = self
            .session
            .as_mut()
            .expect("session available")
            .try_next_update()?;
        Ok(update.and_then(|item| translate_update(item, self.resolved.interactive)))
    }

    pub fn close(mut self) -> AcpResult<()> {
        if let Some(mut session) = self.session.take() {
            session.close()?;
        }
        self.runtime.active_session_id = None;
        Ok(())
    }
}

impl<C> Drop for AcpActiveRun<'_, C> {
    fn drop(&mut self) {
        if let Some(mut session) = self.session.take() {
            let _ = session.close();
        }
        self.runtime.active_session_id = None;
    }
}

#[cfg(test)]
mod tests {
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
            },
            required_artifacts: vec![PathBuf::from("/tmp/project/summary.toml")],
        }
    }

    #[test]
    fn runtime_interface_compiles_without_real_agent_binaries() {
        let connector = StubConnector::new([
            ClientUpdate::AgentMessageText("hello".to_string()),
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
}
