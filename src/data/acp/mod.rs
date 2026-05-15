mod actor;
mod client;
mod dispatch;
mod handshake;
mod tool_call;
#[cfg(test)]
pub(crate) fn client_updates_from_session_updates_for_test(
    values: impl IntoIterator<Item = serde_json::Value>,
    cwd: &std::path::Path,
) -> Vec<ClientUpdate> {
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
    AcpAgentDefinition, AcpConfig, claude_acp_install_root, claude_acp_local_program_for,
    program_is_executable, should_offer_claude_acp_install_for, should_offer_codex_acp_install,
};
pub use super::acp_events::{
    AcpRuntimeEvent, AcpTextAccumulator, AcpTextBoundary, AcpTextEvent, ClientTextKind,
    ClientUpdate, ToolCallActivityKind, translate_update,
};
use crate::{
    data::adapters::EffortLevel, logic::memory::memory_glob_from_session_path, selection::CliKind,
    state::LaunchModes,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
pub type AcpResult<T> = Result<T, AcpError>;
#[rustfmt::skip]
#[derive(thiserror::Error)]
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Build a per-call policy from the loaded `[acp.policy]` config defaults.
    /// Per-stage factories (`final_validation`, `dreaming`, `simplifier`)
    /// construct their own stricter policy and are intentionally NOT routed
    /// through this path — they always win per-call.
    pub fn from_policy_defaults(view: &crate::data::config::view::AcpPolicyDefaultsView) -> Self {
        let shell_policy = match view.shell_policy {
            crate::data::config::schema::ShellPolicy::FullAccess =>
                AcpShellCommandPolicy::FullAccess,
            crate::data::config::schema::ShellPolicy::Allowlist =>
                AcpShellCommandPolicy::Allowlist(view.shell_allowlist.clone()),
        };
        Self {
            allowed_write_paths: view.allowed_write_paths.iter().map(PathBuf::from).collect(),
            shell_policy,
            enforce_readonly_workspace: view.enforce_readonly_workspace,
        }
    }

    fn readonly_memory_shell_allowlist() -> Vec<String> {
        ["git status", "git log", "ls", "cat", "head", "tail", "wc", "file", "find", "pwd"]
            .map(String::from)
            .to_vec()
    }

    pub fn final_validation(
        verdict_path: impl Into<PathBuf>,
        live_summary_path: impl Into<PathBuf>,
    ) -> Self {
        let verdict_path = verdict_path.into();
        let live_summary_path = live_summary_path.into();
        let memory_glob = memory_glob_from_session_path(&verdict_path);
        Self {
            allowed_write_paths: vec![verdict_path, live_summary_path, memory_glob],
            shell_policy: AcpShellCommandPolicy::Allowlist(Self::readonly_memory_shell_allowlist()),
            enforce_readonly_workspace: true,
        }
    }
    pub fn dreaming(
        dream_report_path: impl Into<PathBuf>,
        live_summary_path: impl Into<PathBuf>,
    ) -> Self {
        let dream_report_path = dream_report_path.into();
        let live_summary_path = live_summary_path.into();
        let memory_glob = memory_glob_from_session_path(&dream_report_path);
        Self {
            allowed_write_paths: vec![memory_glob, dream_report_path, live_summary_path],
            shell_policy: AcpShellCommandPolicy::Allowlist(Self::readonly_memory_shell_allowlist()),
            enforce_readonly_workspace: true,
        }
    }
    /// Repo-state update is non-interactive and may write only the current
    /// session's `spec.md`/`plan.md`, its `artifacts/repo-state-update.toml`
    /// report, the run's live summary, and bounded memory updates. Code
    /// edits and other-session edits are forbidden. The shell allowlist is
    /// limited to the read-only git inspection set the reconciliation
    /// agent needs to characterize the repository state.
    pub fn repo_state_update(
        spec_path: impl Into<PathBuf>,
        plan_path: impl Into<PathBuf>,
        report_path: impl Into<PathBuf>,
        live_summary_path: impl Into<PathBuf>,
    ) -> Self {
        let spec_path = spec_path.into();
        let plan_path = plan_path.into();
        let report_path = report_path.into();
        let live_summary_path = live_summary_path.into();
        let memory_glob = memory_glob_from_session_path(&report_path);
        Self {
            allowed_write_paths: vec![
                spec_path,
                plan_path,
                report_path,
                live_summary_path,
                memory_glob,
            ],
            shell_policy: AcpShellCommandPolicy::Allowlist(
                Self::repo_state_update_shell_allowlist(),
            ),
            enforce_readonly_workspace: true,
        }
    }
    /// Read-only git inspection plus the same filesystem-read commands as
    /// the final-validation allowlist. Extends the base set with the
    /// `git diff` / `git rev-parse` / `git show` commands the
    /// reconciliation agent needs to characterize the repository state
    /// without ever mutating it.
    fn repo_state_update_shell_allowlist() -> Vec<String> {
        let mut allow = Self::readonly_memory_shell_allowlist();
        allow.extend(
            ["git diff", "git rev-parse", "git show", "git ls-files"]
                .map(String::from),
        );
        allow
    }
    /// Simplifier writes/commits repo files; workspace not read-only, shell unrestricted.
    pub fn simplifier(
        simplification_path: impl Into<PathBuf>,
        live_summary_path: impl Into<PathBuf>,
    ) -> Self {
        let simplification_path = simplification_path.into();
        let memory_glob = memory_glob_from_session_path(&simplification_path);
        Self {
            allowed_write_paths: vec![simplification_path, live_summary_path.into(), memory_glob],
            shell_policy: AcpShellCommandPolicy::FullAccess,
            enforce_readonly_workspace: false,
        }
    }
}
#[rustfmt::skip]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpLaunchRequest {
    pub cwd: PathBuf, pub prompt: PromptPayload, pub model: String,
    /// The CLI to spawn for this request. Determines which acp.agents
    /// entry supplies the program/args.
    pub cli: CliKind,
    /// The model string to pass to the CLI verbatim. The launch boundary
    /// applies no provider prefixing — any tier qualifiers (e.g.
    /// `opencode-go/`) must already be present in `launch_name`.
    pub launch_name: String,
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
    pub cli: CliKind, pub interactive: bool, pub spawn: AcpSpawnSpec, pub session: AcpSessionSpec,
}
