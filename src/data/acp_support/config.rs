use crate::data::acp::{
    AcpError, AcpLaunchPolicy, AcpLaunchRequest, AcpPermissionMode, AcpReasoningEffort,
    AcpResolvedLaunch, AcpResult, AcpSessionSpec, AcpShellCommandPolicy, AcpSpawnSpec,
};
use crate::selection::{VendorKind, vendor::vendor_kind_to_str};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpAgentDefinition {
    pub vendor: VendorKind,
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpConfig {
    agents: BTreeMap<VendorKind, AcpAgentDefinition>,
}

impl AcpConfig {
    pub fn empty() -> Self {
        Self {
            agents: BTreeMap::new(),
        }
    }

    pub fn from_agents(agents: impl IntoIterator<Item = AcpAgentDefinition>) -> Self {
        Self {
            agents: agents
                .into_iter()
                .map(|agent| (agent.vendor, agent))
                .collect(),
        }
    }

    pub fn available_vendors(&self) -> std::collections::BTreeSet<VendorKind> {
        self.agents
            .iter()
            .filter(|(_, agent)| {
                !agent.program.trim().is_empty() && program_is_executable(agent.program.as_str())
            })
            .map(|(vendor, _)| *vendor)
            .collect()
    }

    pub fn resolve(&self, request: &AcpLaunchRequest) -> AcpResult<AcpResolvedLaunch> {
        let Some(agent) = self.agents.get(&request.vendor) else {
            return Err(AcpError::human_block(format!(
                "ACP agent not configured for vendor {}",
                vendor_kind_to_str(request.vendor)
            )));
        };
        if agent.program.trim().is_empty() {
            return Err(AcpError::human_block(format!(
                "ACP agent for vendor {} has no executable configured",
                vendor_kind_to_str(request.vendor)
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
        let join_paths = |paths: &[PathBuf]| {
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let entries: [(&str, String); 12] = [
            ("vendor", vendor_kind_to_str(request.vendor).to_string()),
            ("model", request.model.clone()),
            (
                "requested_effort",
                effort_to_str(request.requested_effort).to_string(),
            ),
            ("effective_effort", reasoning_effort.to_string()),
            ("permission_mode", permission_mode.to_string()),
            ("interactive", request.interactive.to_string()),
            ("cheap", request.modes.cheap.to_string()),
            ("yolo", request.modes.yolo.to_string()),
            (
                "allowed_write_paths",
                join_paths(&policy.allowed_write_paths),
            ),
            (
                "shell_policy",
                shell_policy_name(&policy.shell_policy).to_string(),
            ),
            (
                "allowed_shell_commands",
                shell_policy_commands(&policy.shell_policy).join("\n"),
            ),
            (
                "enforce_readonly_workspace",
                policy.enforce_readonly_workspace.to_string(),
            ),
        ];
        let mut env = agent.env.clone();
        let mut metadata = BTreeMap::new();
        for (suffix, value) in &entries {
            env.insert(
                format!("CODEXIZE_ACP_{}", suffix.to_uppercase()),
                value.clone(),
            );
            metadata.insert(format!("codexize.{suffix}"), value.clone());
        }

        Ok(AcpResolvedLaunch {
            vendor: request.vendor,
            interactive: request.interactive,
            spawn: AcpSpawnSpec {
                program: agent.program.clone(),
                args: agent.args.clone(),
                env,
            },
            session: AcpSessionSpec {
                cwd,
                prompt: request.prompt.clone(),
                model: request.model.clone(),
                reasoning_effort,
                permission_mode,
                policy,
                metadata,
            },
        })
    }
}

impl Default for AcpConfig {
    fn default() -> Self {
        // Codex and Claude commonly launch through bridge binaries; Gemini and
        // Kimi expose ACP directly. Keep the executable boundary explicit.
        let str_args = |args: &[&str]| args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let claude_program = default_claude_acp_program();
        Self::from_agents([
            default_agent_definition(VendorKind::Claude, &claude_program, Vec::new()),
            default_agent_definition(
                VendorKind::Codex,
                "codex-acp",
                str_args(&[
                    "-c",
                    "sandbox_mode=\"danger-full-access\"",
                    "-c",
                    "approval_policy=\"never\"",
                ]),
            ),
            default_agent_definition(VendorKind::Gemini, "gemini", str_args(&["--yolo", "--acp"])),
            default_agent_definition(
                VendorKind::Kimi,
                "kimi",
                str_args(&["--yolo", "--thinking", "acp"]),
            ),
        ])
    }
}

pub fn claude_acp_install_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codexize")
        .join("acp")
}

pub fn claude_acp_local_program() -> PathBuf {
    claude_acp_install_root()
        .join("node_modules")
        .join(".bin")
        .join("claude-agent-acp")
}

pub fn claude_acp_is_available() -> bool {
    local_or_path_program_is_available(&claude_acp_local_program(), "claude-agent-acp")
}

pub fn claude_cli_is_available() -> bool {
    program_is_executable(CLAUDE_CLI)
}

pub fn should_offer_claude_acp_install() -> bool {
    should_offer_install(CLAUDE_CLI, claude_acp_is_available())
}

pub fn codex_acp_is_available() -> bool {
    program_is_executable(CODEX_ACP_CLI)
}

pub fn codex_cli_is_available() -> bool {
    program_is_executable(CODEX_CLI)
}

pub fn should_offer_codex_acp_install() -> bool {
    should_offer_install(CODEX_CLI, codex_acp_is_available())
}

fn default_claude_acp_program() -> String {
    let local = claude_acp_local_program();
    if path_is_executable(&local) {
        local.display().to_string()
    } else {
        "claude-agent-acp".to_string()
    }
}

const CLAUDE_CLI: &str = "claude";
const CODEX_CLI: &str = "codex";
const CODEX_ACP_CLI: &str = "codex-acp";

fn local_or_path_program_is_available(local: &Path, path_program: &str) -> bool {
    path_is_executable(local) || program_is_executable(path_program)
}

fn should_offer_install(source_cli: &str, bridge_available: bool) -> bool {
    program_is_executable(source_cli) && !bridge_available
}

fn default_agent_definition(
    vendor: VendorKind,
    program: &str,
    args: Vec<String>,
) -> AcpAgentDefinition {
    #[cfg(test)]
    {
        if let Some(program_override) = test_program_override(vendor) {
            return AcpAgentDefinition {
                vendor,
                program: program_override,
                args: Vec::new(),
                env: BTreeMap::new(),
            };
        }
    }

    AcpAgentDefinition {
        vendor,
        program: program.to_string(),
        args,
        env: BTreeMap::new(),
    }
}

#[cfg(test)]
fn test_program_override(vendor: VendorKind) -> Option<String> {
    let key = match vendor {
        VendorKind::Claude => "CODEXIZE_TEST_ACP_CLAUDE_PROGRAM",
        VendorKind::Codex => "CODEXIZE_TEST_ACP_CODEX_PROGRAM",
        VendorKind::Gemini => "CODEXIZE_TEST_ACP_GEMINI_PROGRAM",
        VendorKind::Kimi => "CODEXIZE_TEST_ACP_KIMI_PROGRAM",
    };
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn absolutize(path: &Path) -> AcpResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn absolutize_policy(policy: &AcpLaunchPolicy) -> AcpResult<AcpLaunchPolicy> {
    Ok(AcpLaunchPolicy {
        allowed_write_paths: policy
            .allowed_write_paths
            .iter()
            .map(|path| absolutize(path))
            .collect::<AcpResult<Vec<_>>>()?,
        shell_policy: policy.shell_policy.clone(),
        enforce_readonly_workspace: policy.enforce_readonly_workspace,
    })
}

fn shell_policy_name(policy: &AcpShellCommandPolicy) -> &'static str {
    match policy {
        AcpShellCommandPolicy::FullAccess => "full-access",
        AcpShellCommandPolicy::Allowlist(_) => "allowlist",
    }
}

fn shell_policy_commands(policy: &AcpShellCommandPolicy) -> Vec<String> {
    match policy {
        AcpShellCommandPolicy::FullAccess => Vec::new(),
        AcpShellCommandPolicy::Allowlist(commands) => commands.clone(),
    }
}

pub fn program_is_executable(program: &str) -> bool {
    let candidate = Path::new(program);
    if candidate.components().count() > 1 {
        return path_is_executable(candidate);
    }

    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|dir| path_is_executable(&dir.join(program)))
}

fn path_is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn effort_to_str(effort: crate::adapters::EffortLevel) -> &'static str {
    match effort {
        crate::adapters::EffortLevel::Low => "low",
        crate::adapters::EffortLevel::Normal => "normal",
        crate::adapters::EffortLevel::Tough => "tough",
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests_mod;
