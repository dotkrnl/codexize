use crate::acp::{
    AcpError, AcpLaunchPolicy, AcpLaunchRequest, AcpPermissionMode, AcpReasoningEffort,
    AcpResolvedLaunch, AcpResult, AcpSessionSpec, AcpShellCommandPolicy, AcpSpawnSpec,
};
use crate::data::config::schema::AcpAgentSection;
use crate::data::config::view::{AcpAgentView, AcpInstallView};
use crate::selection::{CliKind, SubscriptionKind, vendor::vendor_kind_to_str};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
const CLAUDE_CLI: &str = "claude";
const CODEX_CLI: &str = "codex";
const CODEX_ACP_CLI: &str = "codex-acp";
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpAgentDefinition {
    pub vendor: SubscriptionKind,
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpConfig {
    agents: BTreeMap<SubscriptionKind, AcpAgentDefinition>,
}
#[rustfmt::skip]
impl AcpConfig {
    pub fn empty() -> Self { Self { agents: BTreeMap::new() } }
    pub fn from_agents(agents: impl IntoIterator<Item = AcpAgentDefinition>) -> Self {
        Self { agents: agents.into_iter().map(|a| (a.vendor, a)).collect() }
    }
    pub fn available_vendors(&self) -> std::collections::BTreeSet<SubscriptionKind> {
        self.agents.iter()
            .filter(|(_, a)| !a.program.trim().is_empty() && program_is_executable(&a.program))
            .map(|(v, _)| *v).collect()
    }
    pub fn resolve(&self, request: &AcpLaunchRequest) -> AcpResult<AcpResolvedLaunch> {
        // For Free and OpencodeGo candidates, the CLI determines which
        // agent entry to use; for direct vendors the subscription IS the
        // agent key. Free candidates always route through the CLI named in
        // the config entry.
        let agent_key = match request.vendor {
            SubscriptionKind::Free => {
                crate::selection::CliKind::to_subscription(request.cli)
            }
            _ => request.vendor,
        };
        let agent = self.agents.get(&agent_key).ok_or_else(|| AcpError::human_block(
            format!("ACP agent not configured for vendor {}", vendor_kind_to_str(request.vendor))
        ))?;
        if agent.program.trim().is_empty() {
            return Err(AcpError::human_block(format!(
                "ACP agent for vendor {} has no executable configured", vendor_kind_to_str(request.vendor)
            )));
        }
        let cwd = absolutize(&request.cwd)?;
        let policy = absolutize_policy(&request.policy)?;
        let reasoning_effort = match request.effective_effort {
            crate::adapters::EffortLevel::Low => AcpReasoningEffort::Low,
            crate::adapters::EffortLevel::Normal => AcpReasoningEffort::Medium,
            crate::adapters::EffortLevel::Tough => AcpReasoningEffort::High,
        };
        let permission_mode = AcpPermissionMode::Code;
        let join = |paths: &[PathBuf]|
            paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
        let (shell_name, shell_cmds): (&str, Vec<String>) = match &policy.shell_policy {
            AcpShellCommandPolicy::FullAccess => ("full-access", Vec::new()),
            AcpShellCommandPolicy::Allowlist(c) => ("allowlist", c.clone()),
        };
        let launch_model = launch_model_for_vendor(
            request.vendor,
            request.route_provider.as_deref(),
            &request.model,
            request.cli,
            &request.launch_name,
        );
        let entries = [
            ("vendor", vendor_kind_to_str(request.vendor).to_string()),
            ("model", launch_model.clone()),
            ("requested_effort", effort_str(request.requested_effort).to_string()),
            ("effective_effort", reasoning_effort.to_string()),
            ("permission_mode", permission_mode.to_string()),
            ("interactive", request.interactive.to_string()),
            ("cheap", request.modes.cheap.to_string()),
            ("yolo", request.modes.yolo.to_string()),
            ("allowed_write_paths", join(&policy.allowed_write_paths)),
            ("shell_policy", shell_name.to_string()),
            ("allowed_shell_commands", shell_cmds.join("\n")),
            ("enforce_readonly_workspace", policy.enforce_readonly_workspace.to_string()),
        ];
        let mut env = agent.env.clone();
        let mut metadata = BTreeMap::new();
        for (suffix, value) in &entries {
            env.insert(format!("CODEXIZE_ACP_{}", suffix.to_uppercase()), value.clone());
            metadata.insert(format!("codexize.{suffix}"), value.clone());
        }
        Ok(AcpResolvedLaunch {
            vendor: request.vendor,
            interactive: request.interactive,
            spawn: AcpSpawnSpec { program: agent.program.clone(), args: agent.args.clone(), env },
            session: AcpSessionSpec {
                cwd, prompt: request.prompt.clone(), model: launch_model,
                reasoning_effort, permission_mode, policy, metadata,
            },
        })
    }
    pub fn from_config_views(
        agents: &crate::data::config::schema::AcpAgents,
        install: &AcpInstallView,
    ) -> Self {
        let view_for = |vendor: SubscriptionKind, section: &AcpAgentSection| -> Option<AcpAgentDefinition> {
            let v = AcpAgentView {
                enabled: *section.enabled.value(),
                program: section.program.value().clone(),
                args: section.args.value().clone(),
                env: section.env.value().clone(),
            };
            if !v.enabled { return None }
            let program = if vendor == SubscriptionKind::Claude && install.prefer_local_claude_acp {
                let local = &install.claude_acp_root.join("node_modules").join(".bin").join("claude-agent-acp");
                if path_is_executable(local) { local.display().to_string() } else { v.program.clone() }
            } else {
                v.program.clone()
            };
            let def = AcpAgentDefinition { vendor, program, args: v.args, env: v.env };
            #[cfg(test)]
            {
                let key = match vendor {
                    SubscriptionKind::Claude => "CODEXIZE_TEST_ACP_CLAUDE_PROGRAM",
                    SubscriptionKind::Codex => "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                    SubscriptionKind::Gemini => "CODEXIZE_TEST_ACP_GEMINI_PROGRAM",
                    SubscriptionKind::Kimi => "CODEXIZE_TEST_ACP_KIMI_PROGRAM",
                    SubscriptionKind::OpencodeGo => "CODEXIZE_TEST_ACP_OPENCODE_PROGRAM",
                    SubscriptionKind::Free => "CODEXIZE_TEST_ACP_OPENCODE_PROGRAM",
                };
                if let Ok(p) = std::env::var(key) && !p.trim().is_empty() {
                    return Some(AcpAgentDefinition { vendor, program: p, args: Vec::new(), env: BTreeMap::new() });
                }
            }
            Some(def)
        };
        let mut defs = Vec::new();
        if let Some(d) = view_for(SubscriptionKind::Claude, &agents.claude) { defs.push(d) }
        if let Some(d) = view_for(SubscriptionKind::Codex, &agents.codex) { defs.push(d) }
        if let Some(d) = view_for(SubscriptionKind::Gemini, &agents.gemini) { defs.push(d) }
        if let Some(d) = view_for(SubscriptionKind::Kimi, &agents.kimi) { defs.push(d) }
        if let Some(d) = view_for(SubscriptionKind::OpencodeGo, &agents.opencode) { defs.push(d) }
        Self::from_agents(defs)
    }
}

#[rustfmt::skip]
impl Default for AcpConfig {
    fn default() -> Self {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let local = claude_acp_local_program_for(&claude_acp_install_root());
        let claude = if path_is_executable(&local) { local.display().to_string() } else { "claude-agent-acp".into() };
        Self::from_agents([
            agent_def(SubscriptionKind::Claude, &claude, Vec::new()),
            agent_def(SubscriptionKind::Codex, "codex-acp",
                argv(&["-c", "sandbox_mode=\"danger-full-access\"", "-c", "approval_policy=\"never\""])),
            agent_def(SubscriptionKind::Gemini, "gemini", argv(&["--yolo", "--acp"])),
            agent_def(SubscriptionKind::Kimi, "kimi", argv(&["--yolo", "--thinking", "acp"])),
            agent_def(SubscriptionKind::OpencodeGo, "opencode", argv(&["acp"])),
        ])
    }
}
#[rustfmt::skip]
pub fn claude_acp_install_root() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
        .join(".codexize").join("acp")
}
#[rustfmt::skip]
pub fn claude_acp_local_program_for(install_root: &Path) -> PathBuf {
    install_root.join("node_modules").join(".bin").join("claude-agent-acp")
}
#[rustfmt::skip]
pub fn should_offer_claude_acp_install_for(install_root: &Path) -> bool {
    program_is_executable(CLAUDE_CLI)
        && !(path_is_executable(&claude_acp_local_program_for(install_root))
             || program_is_executable("claude-agent-acp"))
}
#[rustfmt::skip]
pub fn should_offer_codex_acp_install() -> bool {
    program_is_executable(CODEX_CLI) && !program_is_executable(CODEX_ACP_CLI)
}
#[rustfmt::skip]
fn agent_def(vendor: SubscriptionKind, program: &str, args: Vec<String>) -> AcpAgentDefinition {
    #[cfg(test)]
    {
        let key = match vendor {
            SubscriptionKind::Claude => "CODEXIZE_TEST_ACP_CLAUDE_PROGRAM",
            SubscriptionKind::Codex => "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
            SubscriptionKind::Gemini => "CODEXIZE_TEST_ACP_GEMINI_PROGRAM",
            SubscriptionKind::Kimi => "CODEXIZE_TEST_ACP_KIMI_PROGRAM",
            SubscriptionKind::OpencodeGo => "CODEXIZE_TEST_ACP_OPENCODE_PROGRAM",
            SubscriptionKind::Free => "CODEXIZE_TEST_ACP_OPENCODE_PROGRAM",
        };
        if let Ok(p) = std::env::var(key) && !p.trim().is_empty() {
            return AcpAgentDefinition { vendor, program: p, args: Vec::new(), env: BTreeMap::new() };
        }
    }
    AcpAgentDefinition { vendor, program: program.to_string(), args, env: BTreeMap::new() }
}
#[rustfmt::skip]
fn launch_model_for_vendor(
    vendor: SubscriptionKind,
    route_provider: Option<&str>,
    model: &str,
    cli: CliKind,
    launch_name: &str,
) -> String {
    // Free candidates pass the operator-supplied model name verbatim
    // to the chosen CLI — no provider prefixing or routing wrapper.
    if vendor == SubscriptionKind::Free {
        return launch_name.to_string();
    }
    if vendor == SubscriptionKind::OpencodeGo && !model.contains('/') {
        // opencode's ACP `model` config advertises provider-qualified values
        // (`opencode/<id>` for the zen tier, `opencode-go/<id>` for the Go
        // tier), while inventory stores bare ids for ipbr matching. Default
        // to the legacy `opencode` qualifier when route_provider is unset
        // so cached entries written before this field landed still launch.
        let qualifier = route_provider.unwrap_or("opencode");
        format!("{qualifier}/{model}")
    } else {
        model.to_string()
    }
}
#[rustfmt::skip]
fn absolutize(path: &Path) -> AcpResult<PathBuf> {
    if path.is_absolute() { Ok(path.to_path_buf()) } else { Ok(std::env::current_dir()?.join(path)) }
}
#[rustfmt::skip]
fn absolutize_policy(p: &AcpLaunchPolicy) -> AcpResult<AcpLaunchPolicy> {
    Ok(AcpLaunchPolicy {
        allowed_write_paths: p.allowed_write_paths.iter().map(|p| absolutize(p)).collect::<AcpResult<Vec<_>>>()?,
        shell_policy: p.shell_policy.clone(), enforce_readonly_workspace: p.enforce_readonly_workspace,
    })
}
#[rustfmt::skip]
pub fn program_is_executable(program: &str) -> bool {
    let candidate = Path::new(program);
    if candidate.components().count() > 1 { return path_is_executable(candidate); }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|dir| path_is_executable(&dir.join(program)))
}
#[rustfmt::skip]
fn path_is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else { return false };
    if !metadata.is_file() { return false }
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; metadata.permissions().mode() & 0o111 != 0 }
    #[cfg(not(unix))] { true }
}
#[rustfmt::skip]
fn effort_str(e: crate::adapters::EffortLevel) -> &'static str {
    use crate::adapters::EffortLevel::*;
    match e { Low => "low", Normal => "normal", Tough => "tough" }
}
#[cfg(test)]
#[path = "acp/config_tests.rs"]
mod tests_mod;
