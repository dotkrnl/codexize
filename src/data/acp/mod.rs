mod actor;
mod client;
mod dispatch;
mod handshake;
mod tool_call;

#[cfg(test)]
pub fn client_updates_from_session_updates_for_test(
    values: impl IntoIterator<Item = serde_json::Value>,
    cwd: &std::path::Path,
) -> Vec<crate::acp::ClientUpdate> {
    let mut map = tool_call::ToolCallMap::new();
    let mut boundary = dispatch::AcpBoundaryState::new();
    let mut out = std::collections::VecDeque::new();
    for value in values {
        dispatch::dispatch_update(&value, cwd, &mut map, &mut boundary, &mut out);
    }
    out.into_iter().collect()
}
pub use client::{AcpConnector, AcpSession, SubprocessConnector};
// Launch/policy wiring and runtime text adaptation are ACP-specific, but they are
// orchestration concerns rather than JSON-RPC transport, so they live alongside
// `data::acp` instead of inflating the transport directory's footprint.
pub use super::acp_config::{
    AcpAgentDefinition, AcpConfig, claude_acp_install_root, claude_acp_local_program,
    program_is_executable, should_offer_claude_acp_install, should_offer_codex_acp_install,
};
pub use super::acp_events::{
    AcpRuntimeEvent, AcpTextAccumulator, AcpTextBoundary, AcpTextEvent, ClientTextKind,
    ClientUpdate, ToolCallActivityKind, translate_update,
};

use crate::{adapters::EffortLevel, selection::VendorKind, state::LaunchModes};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub type AcpResult<T> = Result<T, AcpError>;

#[rustfmt::skip]
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum AcpError {
    #[error("{0}")] HumanBlock(String),
    #[error("{0}")] Io(String),
    #[error("{0}")] Protocol(String),
}

#[rustfmt::skip]
impl AcpError {
    pub fn human_block(m: impl Into<String>) -> Self { Self::HumanBlock(m.into()) }
    pub fn protocol(m: impl Into<String>) -> Self { Self::Protocol(m.into()) }
    pub fn io(m: impl Into<String>) -> Self { Self::Io(m.into()) }
}

impl From<std::io::Error> for AcpError {
    #[rustfmt::skip]
    fn from(value: std::io::Error) -> Self { Self::io(value.to_string()) }
}

#[rustfmt::skip]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptPayload { Text(String), File(PathBuf) }

#[rustfmt::skip]
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
pub enum AcpReasoningEffort {
    #[strum(to_string = "low")] Low,
    #[strum(to_string = "medium")] Medium,
    #[strum(to_string = "high")] High,
}

#[rustfmt::skip]
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
pub enum AcpPermissionMode {
    #[strum(to_string = "ask")] Ask,
    #[strum(to_string = "code")] Code,
}

#[rustfmt::skip]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AcpShellCommandPolicy {
    #[default] FullAccess,
    Allowlist(Vec<String>),
}

#[rustfmt::skip]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcpLaunchPolicy {
    pub allowed_write_paths: Vec<PathBuf>,
    pub shell_policy: AcpShellCommandPolicy,
    pub enforce_readonly_workspace: bool,
}

#[rustfmt::skip]
impl AcpLaunchPolicy {
    pub fn final_validation(verdict_path: impl Into<PathBuf>, live_summary_path: impl Into<PathBuf>) -> Self {
        let cmds = ["git status", "git log", "ls", "cat", "head", "tail", "wc", "file", "find", "pwd"];
        Self {
            allowed_write_paths: vec![verdict_path.into(), live_summary_path.into()],
            shell_policy: AcpShellCommandPolicy::Allowlist(cmds.map(String::from).to_vec()),
            enforce_readonly_workspace: true,
        }
    }
    /// Simplifier writes/commits repo files; workspace not read-only, shell unrestricted.
    pub fn simplifier(simplification_path: impl Into<PathBuf>, live_summary_path: impl Into<PathBuf>) -> Self {
        Self {
            allowed_write_paths: vec![simplification_path.into(), live_summary_path.into()],
            shell_policy: AcpShellCommandPolicy::FullAccess, enforce_readonly_workspace: false,
        }
    }
}

#[rustfmt::skip]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpLaunchRequest {
    pub vendor: VendorKind, pub cwd: PathBuf, pub prompt: PromptPayload, pub model: String,
    pub requested_effort: EffortLevel, pub effective_effort: EffortLevel,
    pub interactive: bool, pub modes: LaunchModes, pub policy: AcpLaunchPolicy,
}

#[rustfmt::skip]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpSpawnSpec { pub program: String, pub args: Vec<String>, pub env: BTreeMap<String, String> }

#[rustfmt::skip]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpSessionSpec {
    pub cwd: PathBuf, pub prompt: PromptPayload, pub model: String,
    pub reasoning_effort: AcpReasoningEffort, pub permission_mode: AcpPermissionMode,
    pub policy: AcpLaunchPolicy, pub metadata: BTreeMap<String, String>,
}

#[rustfmt::skip]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpResolvedLaunch {
    pub vendor: VendorKind, pub interactive: bool, pub spawn: AcpSpawnSpec, pub session: AcpSessionSpec,
}
