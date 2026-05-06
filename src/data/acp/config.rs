use super::{
    AcpError, AcpLaunchPolicy, AcpLaunchRequest, AcpPermissionMode, AcpReasoningEffort,
    AcpResolvedLaunch, AcpResult, AcpSessionSpec, AcpShellCommandPolicy, AcpSpawnSpec,
};
use crate::selection::{VendorKind, vendor::vendor_kind_to_str};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const CLAUDE_CLI: &str = "claude";
const CODEX_CLI: &str = "codex";
const CODEX_ACP_CLI: &str = "codex-acp";

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
            agents: agents.into_iter().map(|a| (a.vendor, a)).collect(),
        }
    }

    pub fn available_vendors(&self) -> std::collections::BTreeSet<VendorKind> {
        self.agents
            .iter()
            .filter(|(_, a)| !a.program.trim().is_empty() && program_is_executable(&a.program))
            .map(|(v, _)| *v)
            .collect()
    }

    pub fn resolve(&self, request: &AcpLaunchRequest) -> AcpResult<AcpResolvedLaunch> {
        let agent = self.agents.get(&request.vendor).ok_or_else(|| {
            AcpError::human_block(format!(
                "ACP agent not configured for vendor {}",
                vendor_kind_to_str(request.vendor)
            ))
        })?;
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
        let join = |paths: &[PathBuf]| {
            paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (shell_name, shell_cmds): (&str, Vec<String>) = match &policy.shell_policy {
            AcpShellCommandPolicy::FullAccess => ("full-access", Vec::new()),
            AcpShellCommandPolicy::Allowlist(c) => ("allowlist", c.clone()),
        };
        let entries = [
            ("vendor", vendor_kind_to_str(request.vendor).to_string()),
            ("model", request.model.clone()),
            (
                "requested_effort",
                effort_str(request.requested_effort).to_string(),
            ),
            ("effective_effort", reasoning_effort.to_string()),
            ("permission_mode", permission_mode.to_string()),
            ("interactive", request.interactive.to_string()),
            ("cheap", request.modes.cheap.to_string()),
            ("yolo", request.modes.yolo.to_string()),
            ("allowed_write_paths", join(&policy.allowed_write_paths)),
            ("shell_policy", shell_name.to_string()),
            ("allowed_shell_commands", shell_cmds.join("\n")),
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
        let str_args = |args: &[&str]| args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let claude = {
            let local = claude_acp_local_program();
            if path_is_executable(&local) {
                local.display().to_string()
            } else {
                "claude-agent-acp".to_string()
            }
        };
        Self::from_agents([
            agent_def(VendorKind::Claude, &claude, Vec::new()),
            agent_def(
                VendorKind::Codex,
                "codex-acp",
                str_args(&[
                    "-c",
                    "sandbox_mode=\"danger-full-access\"",
                    "-c",
                    "approval_policy=\"never\"",
                ]),
            ),
            agent_def(VendorKind::Gemini, "gemini", str_args(&["--yolo", "--acp"])),
            agent_def(
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
    path_is_executable(&claude_acp_local_program()) || program_is_executable("claude-agent-acp")
}
pub fn claude_cli_is_available() -> bool {
    program_is_executable(CLAUDE_CLI)
}
pub fn should_offer_claude_acp_install() -> bool {
    program_is_executable(CLAUDE_CLI) && !claude_acp_is_available()
}
pub fn codex_acp_is_available() -> bool {
    program_is_executable(CODEX_ACP_CLI)
}
pub fn codex_cli_is_available() -> bool {
    program_is_executable(CODEX_CLI)
}
pub fn should_offer_codex_acp_install() -> bool {
    program_is_executable(CODEX_CLI) && !codex_acp_is_available()
}

fn agent_def(vendor: VendorKind, program: &str, args: Vec<String>) -> AcpAgentDefinition {
    #[cfg(test)]
    if let Some(p) = test_program_override(vendor) {
        return AcpAgentDefinition {
            vendor,
            program: p,
            args: Vec::new(),
            env: BTreeMap::new(),
        };
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
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
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
            .map(|p| absolutize(p))
            .collect::<AcpResult<Vec<_>>>()?,
        shell_policy: policy.shell_policy.clone(),
        enforce_readonly_workspace: policy.enforce_readonly_workspace,
    })
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

fn effort_str(effort: crate::adapters::EffortLevel) -> &'static str {
    match effort {
        crate::adapters::EffortLevel::Low => "low",
        crate::adapters::EffortLevel::Normal => "normal",
        crate::adapters::EffortLevel::Tough => "tough",
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests_mod;
