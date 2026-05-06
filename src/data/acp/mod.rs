mod client;
mod config;
mod events;
mod tool_call;

#[cfg(test)]
pub use client::client_updates_from_session_updates_for_test;
pub use client::{AcpConnector, AcpSession, SubprocessConnector};
pub use config::{
    AcpAgentDefinition, AcpConfig, claude_acp_install_root, claude_acp_is_available,
    claude_acp_local_program, claude_cli_is_available, codex_acp_is_available,
    codex_cli_is_available, program_is_executable, should_offer_claude_acp_install,
    should_offer_codex_acp_install,
};
pub use events::{
    AcpCompletionEvent, AcpLifecycleEvent, AcpRuntimeEvent, AcpTextAccumulator, AcpTextBoundary,
    AcpTextEvent, ClientUpdate, ToolCallActivityKind, translate_update,
};

use crate::{adapters::EffortLevel, selection::VendorKind, state::LaunchModes};
use std::{collections::BTreeMap, path::PathBuf};

pub type AcpResult<T> = Result<T, AcpError>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcpError {
    #[error("{0}")]
    HumanBlock(String),
    #[error("{0}")]
    Busy(String),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
pub enum AcpReasoningEffort {
    #[strum(to_string = "low")]
    Low,
    #[strum(to_string = "medium")]
    Medium,
    #[strum(to_string = "high")]
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
pub enum AcpPermissionMode {
    #[strum(to_string = "ask")]
    Ask,
    #[strum(to_string = "code")]
    Code,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AcpShellCommandPolicy {
    #[default]
    FullAccess,
    Allowlist(Vec<String>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcpLaunchPolicy {
    pub allowed_write_paths: Vec<PathBuf>,
    pub shell_policy: AcpShellCommandPolicy,
    pub enforce_readonly_workspace: bool,
}

impl AcpLaunchPolicy {
    pub fn final_validation(
        verdict_path: impl Into<PathBuf>,
        live_summary_path: impl Into<PathBuf>,
    ) -> Self {
        let allowed = [
            "git status",
            "git log",
            "ls",
            "cat",
            "head",
            "tail",
            "wc",
            "file",
            "find",
            "pwd",
        ];
        Self {
            allowed_write_paths: vec![verdict_path.into(), live_summary_path.into()],
            shell_policy: AcpShellCommandPolicy::Allowlist(
                allowed.iter().map(|s| s.to_string()).collect(),
            ),
            enforce_readonly_workspace: true,
        }
    }

    /// Simplifier ACP policy: code-producing (writes/commits repository
    /// files), so workspace is not read-only and shell access is unrestricted;
    /// required artifact writes are still listed for violation reporting.
    pub fn simplifier(
        simplification_path: impl Into<PathBuf>,
        live_summary_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            allowed_write_paths: vec![simplification_path.into(), live_summary_path.into()],
            shell_policy: AcpShellCommandPolicy::FullAccess,
            enforce_readonly_workspace: false,
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
    pub policy: AcpLaunchPolicy,
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
    pub policy: AcpLaunchPolicy,
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
#[path = "runtime_tests.rs"]
mod runtime_tests;
