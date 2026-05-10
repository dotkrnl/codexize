use crate::acp::{
    AcpError, AcpLaunchPolicy, AcpLaunchRequest, AcpPermissionMode, AcpReasoningEffort,
    AcpResolvedLaunch, AcpResult, AcpSessionSpec, AcpShellCommandPolicy, AcpSpawnSpec,
};
use crate::data::config::schema::AcpAgentSection;
use crate::data::config::view::{AcpAgentView, AcpInstallView};
use crate::selection::CliKind;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
const CLAUDE_CLI: &str = "claude";
const CODEX_CLI: &str = "codex";
const CODEX_ACP_CLI: &str = "codex-acp";
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpAgentDefinition {
    pub cli: CliKind,
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpConfig {
    agents: BTreeMap<CliKind, AcpAgentDefinition>,
}
#[rustfmt::skip]
impl AcpConfig {
    pub fn empty() -> Self { Self { agents: BTreeMap::new() } }
    pub fn from_agents(agents: impl IntoIterator<Item = AcpAgentDefinition>) -> Self {
        Self { agents: agents.into_iter().map(|a| (a.cli, a)).collect() }
    }
    pub fn available_clis(&self) -> std::collections::BTreeSet<CliKind> {
        self.agents.iter()
            .filter(|(_, a)| !a.program.trim().is_empty() && program_is_executable(&a.program))
            .map(|(c, _)| *c).collect()
    }
    pub fn resolve(&self, request: &AcpLaunchRequest) -> AcpResult<AcpResolvedLaunch> {
        let agent = self.agents.get(&request.cli).ok_or_else(|| AcpError::human_block(
            format!("ACP agent not configured for cli {}", request.cli.as_str())
        ))?;
        if agent.program.trim().is_empty() {
            return Err(AcpError::human_block(format!(
                "ACP agent for cli {} has no executable configured", request.cli.as_str()
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
        let session_model = request.launch_name.clone();
        let entries = [
            ("cli", request.cli.as_str().to_string()),
            ("model", session_model.clone()),
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
            cli: request.cli,
            interactive: request.interactive,
            spawn: AcpSpawnSpec { program: agent.program.clone(), args: agent.args.clone(), env },
            session: AcpSessionSpec {
                cwd, prompt: request.prompt.clone(), model: session_model,
                reasoning_effort, permission_mode, policy, metadata,
            },
        })
    }
    pub fn from_config_views(
        agents: &crate::data::config::schema::AcpAgents,
        install: &AcpInstallView,
    ) -> Self {
        let view_for = |cli: CliKind, section: &AcpAgentSection| -> Option<AcpAgentDefinition> {
            let v = AcpAgentView {
                enabled: *section.enabled.value(),
                program: section.program.value().clone(),
                args: section.args.value().clone(),
                env: section.env.value().clone(),
            };
            if !v.enabled { return None }
            let program = if cli == CliKind::Claude && install.prefer_local_claude_acp {
                let local = &install.claude_acp_root.join("node_modules").join(".bin").join("claude-agent-acp");
                if path_is_executable(local) { local.display().to_string() } else { v.program.clone() }
            } else {
                v.program.clone()
            };
            let def = AcpAgentDefinition { cli, program, args: v.args, env: v.env };
            #[cfg(test)]
            {
                let key = match cli {
                    CliKind::Claude => "CODEXIZE_TEST_ACP_CLAUDE_PROGRAM",
                    CliKind::Codex => "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
                    CliKind::Gemini => "CODEXIZE_TEST_ACP_GEMINI_PROGRAM",
                    CliKind::Kimi => "CODEXIZE_TEST_ACP_KIMI_PROGRAM",
                    CliKind::Opencode => "CODEXIZE_TEST_ACP_OPENCODE_PROGRAM",
                };
                if let Ok(p) = std::env::var(key) && !p.trim().is_empty() {
                    return Some(AcpAgentDefinition { cli, program: p, args: Vec::new(), env: BTreeMap::new() });
                }
            }
            Some(def)
        };
        let mut defs = Vec::new();
        if let Some(d) = view_for(CliKind::Claude, &agents.claude) { defs.push(d) }
        if let Some(d) = view_for(CliKind::Codex, &agents.codex) { defs.push(d) }
        if let Some(d) = view_for(CliKind::Gemini, &agents.gemini) { defs.push(d) }
        if let Some(d) = view_for(CliKind::Kimi, &agents.kimi) { defs.push(d) }
        if let Some(d) = view_for(CliKind::Opencode, &agents.opencode) { defs.push(d) }
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
            agent_def(CliKind::Claude, &claude, Vec::new()),
            agent_def(CliKind::Codex, "codex-acp",
                argv(&["-c", "sandbox_mode=\"danger-full-access\"", "-c", "approval_policy=\"never\""])),
            agent_def(CliKind::Gemini, "gemini", argv(&["--yolo", "--acp"])),
            agent_def(CliKind::Kimi, "kimi", argv(&["--yolo", "--thinking", "acp"])),
            agent_def(CliKind::Opencode, "opencode", argv(&["acp"])),
        ])
    }
}
#[rustfmt::skip]
pub fn claude_acp_install_root() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
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
    cfg!(target_os = "macos")
        && program_is_executable("brew")
        && program_is_executable(CODEX_CLI)
        && !program_is_executable(CODEX_ACP_CLI)
}
#[rustfmt::skip]
fn agent_def(cli: CliKind, program: &str, args: Vec<String>) -> AcpAgentDefinition {
    #[cfg(test)]
    {
        let key = match cli {
            CliKind::Claude => "CODEXIZE_TEST_ACP_CLAUDE_PROGRAM",
            CliKind::Codex => "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
            CliKind::Gemini => "CODEXIZE_TEST_ACP_GEMINI_PROGRAM",
            CliKind::Kimi => "CODEXIZE_TEST_ACP_KIMI_PROGRAM",
            CliKind::Opencode => "CODEXIZE_TEST_ACP_OPENCODE_PROGRAM",
        };
        if let Ok(p) = std::env::var(key) && !p.trim().is_empty() {
            return AcpAgentDefinition { cli, program: p, args: Vec::new(), env: BTreeMap::new() };
        }
    }
    AcpAgentDefinition { cli, program: program.to_string(), args, env: BTreeMap::new() }
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
