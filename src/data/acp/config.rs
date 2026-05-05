use super::{
    AcpError, AcpLaunchRequest, AcpPermissionMode, AcpReasoningEffort, AcpResolvedLaunch,
    AcpResult, AcpSessionSpec, AcpShellCommandPolicy, AcpSpawnSpec,
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
        let required_artifacts = request
            .required_artifacts
            .iter()
            .map(|path| absolutize(path))
            .collect::<AcpResult<Vec<_>>>()?;
        let policy = absolutize_policy(&request.policy)?;
        let reasoning_effort = match request.effective_effort {
            crate::adapters::EffortLevel::Low => AcpReasoningEffort::Low,
            crate::adapters::EffortLevel::Normal => AcpReasoningEffort::Medium,
            crate::adapters::EffortLevel::Tough => AcpReasoningEffort::High,
        };
        let permission_mode = AcpPermissionMode::Code;

        let mut env = agent.env.clone();
        env.insert(
            "CODEXIZE_ACP_VENDOR".to_string(),
            vendor_kind_to_str(request.vendor).to_string(),
        );
        env.insert("CODEXIZE_ACP_MODEL".to_string(), request.model.clone());
        env.insert(
            "CODEXIZE_ACP_REQUESTED_EFFORT".to_string(),
            effort_to_str(request.requested_effort).to_string(),
        );
        env.insert(
            "CODEXIZE_ACP_EFFECTIVE_EFFORT".to_string(),
            reasoning_effort.to_string(),
        );
        env.insert(
            "CODEXIZE_ACP_PERMISSION_MODE".to_string(),
            permission_mode.to_string(),
        );
        env.insert(
            "CODEXIZE_ACP_INTERACTIVE".to_string(),
            request.interactive.to_string(),
        );
        env.insert(
            "CODEXIZE_ACP_CHEAP".to_string(),
            request.modes.cheap.to_string(),
        );
        env.insert(
            "CODEXIZE_ACP_YOLO".to_string(),
            request.modes.yolo.to_string(),
        );
        env.insert(
            "CODEXIZE_ACP_ALLOWED_WRITE_PATHS".to_string(),
            policy
                .allowed_write_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        env.insert(
            "CODEXIZE_ACP_SHELL_POLICY".to_string(),
            shell_policy_name(&policy.shell_policy).to_string(),
        );
        env.insert(
            "CODEXIZE_ACP_ALLOWED_SHELL_COMMANDS".to_string(),
            shell_policy_commands(&policy.shell_policy).join("\n"),
        );
        env.insert(
            "CODEXIZE_ACP_ENFORCE_READONLY_WORKSPACE".to_string(),
            policy.enforce_readonly_workspace.to_string(),
        );

        let mut metadata = BTreeMap::new();
        metadata.insert(
            "codexize.vendor".to_string(),
            vendor_kind_to_str(request.vendor).to_string(),
        );
        metadata.insert("codexize.model".to_string(), request.model.clone());
        metadata.insert(
            "codexize.requested_effort".to_string(),
            effort_to_str(request.requested_effort).to_string(),
        );
        metadata.insert(
            "codexize.effective_effort".to_string(),
            reasoning_effort.to_string(),
        );
        metadata.insert(
            "codexize.permission_mode".to_string(),
            permission_mode.to_string(),
        );
        metadata.insert(
            "codexize.interactive".to_string(),
            request.interactive.to_string(),
        );
        metadata.insert(
            "codexize.cheap".to_string(),
            request.modes.cheap.to_string(),
        );
        metadata.insert("codexize.yolo".to_string(), request.modes.yolo.to_string());
        metadata.insert(
            "codexize.allowed_write_paths".to_string(),
            policy
                .allowed_write_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        metadata.insert(
            "codexize.shell_policy".to_string(),
            shell_policy_name(&policy.shell_policy).to_string(),
        );
        metadata.insert(
            "codexize.allowed_shell_commands".to_string(),
            shell_policy_commands(&policy.shell_policy).join("\n"),
        );
        metadata.insert(
            "codexize.enforce_readonly_workspace".to_string(),
            policy.enforce_readonly_workspace.to_string(),
        );

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
                requested_effort: request.requested_effort,
                effective_effort: request.effective_effort,
                reasoning_effort,
                permission_mode,
                interactive: request.interactive,
                modes: request.modes,
                required_artifacts,
                policy,
                metadata,
            },
        })
    }
}

impl Default for AcpAgentDefinition {
    fn default() -> Self {
        Self {
            vendor: VendorKind::Codex,
            program: String::new(),
            args: Vec::new(),
            env: BTreeMap::new(),
        }
    }
}

impl Default for AcpConfig {
    fn default() -> Self {
        // Current ACP entrypoints are vendor-specific: Gemini and Kimi expose
        // ACP directly, while Codex and Claude are commonly launched through
        // ACP bridge binaries, so keep the executable boundary explicit here.
        let definitions = [
            default_agent_definition(
                VendorKind::Claude,
                &default_claude_acp_program(),
                Vec::<String>::new(),
            ),
            default_agent_definition(
                VendorKind::Codex,
                "codex-acp",
                vec![
                    "-c".to_string(),
                    "sandbox_mode=\"danger-full-access\"".to_string(),
                    "-c".to_string(),
                    "approval_policy=\"never\"".to_string(),
                ],
            ),
            default_agent_definition(
                VendorKind::Gemini,
                "gemini",
                vec!["--yolo".to_string(), "--acp".to_string()],
            ),
            default_agent_definition(
                VendorKind::Kimi,
                "kimi",
                vec![
                    "--yolo".to_string(),
                    "--thinking".to_string(),
                    "acp".to_string(),
                ],
            ),
        ];
        Self::from_agents(definitions)
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
    path_is_executable(&claude_acp_local_program()) || program_is_executable("claude-agent-acp")
}

pub fn claude_cli_is_available() -> bool {
    program_is_executable("claude")
}

pub fn should_offer_claude_acp_install() -> bool {
    claude_cli_is_available() && !claude_acp_is_available()
}

pub fn codex_acp_is_available() -> bool {
    program_is_executable("codex-acp")
}

pub fn codex_cli_is_available() -> bool {
    program_is_executable("codex")
}

pub fn should_offer_codex_acp_install() -> bool {
    codex_cli_is_available() && !codex_acp_is_available()
}

fn default_claude_acp_program() -> String {
    let local = claude_acp_local_program();
    if path_is_executable(&local) {
        local.display().to_string()
    } else {
        "claude-agent-acp".to_string()
    }
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

fn absolutize_policy(policy: &super::AcpLaunchPolicy) -> AcpResult<super::AcpLaunchPolicy> {
    Ok(super::AcpLaunchPolicy {
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
mod tests_mod;
