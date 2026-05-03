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
            reasoning_effort.as_str().to_string(),
        );
        env.insert(
            "CODEXIZE_ACP_PERMISSION_MODE".to_string(),
            permission_mode.as_str().to_string(),
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
            reasoning_effort.as_str().to_string(),
        );
        metadata.insert(
            "codexize.permission_mode".to_string(),
            permission_mode.as_str().to_string(),
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
mod tests {
    use super::*;
    use crate::{adapters::EffortLevel, state::LaunchModes};

    fn sample_request(vendor: VendorKind) -> AcpLaunchRequest {
        AcpLaunchRequest {
            vendor,
            cwd: PathBuf::from("workspace"),
            prompt: super::super::PromptPayload::Text("prompt".to_string()),
            model: "gpt-5.5".to_string(),
            requested_effort: EffortLevel::Normal,
            effective_effort: EffortLevel::Low,
            interactive: false,
            modes: LaunchModes {
                yolo: true,
                cheap: true,
                interactive: false,
            },
            required_artifacts: vec![PathBuf::from("artifacts/summary.toml")],
            policy: super::super::AcpLaunchPolicy::default(),
        }
    }

    fn non_yolo_request(vendor: VendorKind) -> AcpLaunchRequest {
        AcpLaunchRequest {
            modes: LaunchModes {
                yolo: false,
                cheap: false,
                interactive: false,
            },
            ..sample_request(vendor)
        }
    }

    #[test]
    fn resolves_vendor_keyed_definitions_with_launch_metadata() {
        let resolved = AcpConfig::default()
            .resolve(&sample_request(VendorKind::Gemini))
            .expect("resolve gemini");

        assert_eq!(resolved.vendor, VendorKind::Gemini);
        assert_eq!(resolved.spawn.program, "gemini");
        assert_eq!(
            resolved.spawn.args,
            vec!["--yolo".to_string(), "--acp".to_string()]
        );
        assert_eq!(resolved.session.reasoning_effort, AcpReasoningEffort::Low);
        assert_eq!(resolved.session.permission_mode, AcpPermissionMode::Code);
        assert_eq!(
            resolved
                .session
                .metadata
                .get("codexize.vendor")
                .map(String::as_str),
            Some("google")
        );
    }

    #[test]
    fn missing_vendor_configuration_is_reported_as_human_block() {
        let err = AcpConfig::empty()
            .resolve(&sample_request(VendorKind::Claude))
            .expect_err("missing config");
        assert!(matches!(err, AcpError::HumanBlock(_)));
    }

    #[test]
    fn launch_translation_preserves_model_and_cheap_derived_effort() {
        let resolved = AcpConfig::default()
            .resolve(&sample_request(VendorKind::Codex))
            .expect("resolve codex");

        assert_eq!(
            resolved.spawn.args,
            vec![
                "-c".to_string(),
                "sandbox_mode=\"danger-full-access\"".to_string(),
                "-c".to_string(),
                "approval_policy=\"never\"".to_string(),
            ]
        );
        assert_eq!(resolved.session.model, "gpt-5.5");
        assert_eq!(resolved.session.requested_effort, EffortLevel::Normal);
        assert_eq!(resolved.session.effective_effort, EffortLevel::Low);
        assert_eq!(resolved.session.reasoning_effort, AcpReasoningEffort::Low);
        assert_eq!(
            resolved
                .spawn
                .env
                .get("CODEXIZE_ACP_EFFECTIVE_EFFORT")
                .map(String::as_str),
            Some("low")
        );
        assert_eq!(
            resolved
                .spawn
                .env
                .get("CODEXIZE_ACP_PERMISSION_MODE")
                .map(String::as_str),
            Some("code")
        );
    }

    #[test]
    fn acp_launches_use_code_permission_mode_even_without_codexize_yolo() {
        let resolved = AcpConfig::default()
            .resolve(&non_yolo_request(VendorKind::Kimi))
            .expect("resolve kimi");

        assert_eq!(
            resolved.spawn.args,
            vec![
                "--yolo".to_string(),
                "--thinking".to_string(),
                "acp".to_string()
            ]
        );
        assert_eq!(resolved.session.permission_mode, AcpPermissionMode::Code);
        assert_eq!(
            resolved
                .spawn
                .env
                .get("CODEXIZE_ACP_PERMISSION_MODE")
                .map(String::as_str),
            Some("code")
        );
        assert_eq!(
            resolved
                .session
                .metadata
                .get("codexize.permission_mode")
                .map(String::as_str),
            Some("code")
        );
    }

    #[test]
    fn final_validation_policy_is_exported_to_session_env_and_metadata() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let verdict_path = temp.path().join("artifacts/final_validation_1.toml");
        let live_summary_path = temp
            .path()
            .join("artifacts/live_summary.final-validation-r1.txt");
        let request = AcpLaunchRequest {
            cwd: temp.path().to_path_buf(),
            policy: super::super::AcpLaunchPolicy::final_validation(
                &verdict_path,
                &live_summary_path,
            ),
            ..sample_request(VendorKind::Codex)
        };

        let resolved = AcpConfig::default()
            .resolve(&request)
            .expect("resolve codex");

        assert_eq!(resolved.session.permission_mode, AcpPermissionMode::Code);
        assert_eq!(
            resolved.session.policy.allowed_write_paths,
            vec![verdict_path.clone(), live_summary_path.clone()]
        );
        assert!(resolved.session.policy.enforce_readonly_workspace);
        assert!(matches!(
            resolved.session.policy.shell_policy,
            super::super::AcpShellCommandPolicy::Allowlist(_)
        ));
        assert_eq!(
            resolved
                .spawn
                .env
                .get("CODEXIZE_ACP_ALLOWED_WRITE_PATHS")
                .cloned(),
            Some(format!(
                "{}\n{}",
                verdict_path.display(),
                live_summary_path.display()
            ))
        );
        assert_eq!(
            resolved
                .spawn
                .env
                .get("CODEXIZE_ACP_SHELL_POLICY")
                .map(String::as_str),
            Some("allowlist")
        );
        assert!(
            resolved
                .spawn
                .env
                .get("CODEXIZE_ACP_ALLOWED_SHELL_COMMANDS")
                .is_some_and(|commands| commands.contains("git status"))
        );
        assert_eq!(
            resolved
                .spawn
                .env
                .get("CODEXIZE_ACP_ENFORCE_READONLY_WORKSPACE")
                .map(String::as_str),
            Some("true")
        );
        assert_eq!(
            resolved
                .session
                .metadata
                .get("codexize.shell_policy")
                .map(String::as_str),
            Some("allowlist")
        );
        assert_eq!(
            resolved
                .session
                .metadata
                .get("codexize.enforce_readonly_workspace")
                .map(String::as_str),
            Some("true")
        );
    }

    #[test]
    fn claude_acp_local_program_lives_under_home_codexize_acp() {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let prev_home = std::env::var_os("HOME");
        let home = tempfile::TempDir::new().expect("temp home");
        unsafe {
            std::env::set_var("HOME", home.path());
        }

        let program = claude_acp_local_program();

        unsafe {
            match prev_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }

        assert_eq!(
            program,
            home.path()
                .join(".codexize")
                .join("acp")
                .join("node_modules")
                .join(".bin")
                .join("claude-agent-acp")
        );
    }

    #[test]
    fn claude_acp_install_prompt_requires_claude_cli_and_missing_acp() {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let home = tempfile::TempDir::new().expect("temp home");
        let fake_bin = tempfile::TempDir::new().expect("fake bin");
        let prev_home = std::env::var_os("HOME");
        let prev_path = std::env::var_os("PATH");

        unsafe {
            std::env::set_var("HOME", home.path());
            std::env::set_var("PATH", fake_bin.path());
        }

        assert!(!should_offer_claude_acp_install());

        write_fake_executable(&fake_bin.path().join("claude"));
        assert!(should_offer_claude_acp_install());

        write_fake_executable(&fake_bin.path().join("claude-agent-acp"));
        assert!(!should_offer_claude_acp_install());

        unsafe {
            match prev_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match prev_path {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    #[test]
    fn codex_acp_install_prompt_requires_codex_cli_and_missing_acp() {
        let _guard = crate::state::test_fs_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let fake_bin = tempfile::TempDir::new().expect("fake bin");
        let prev_path = std::env::var_os("PATH");

        unsafe {
            std::env::set_var("PATH", fake_bin.path());
        }

        assert!(!should_offer_codex_acp_install());

        write_fake_executable(&fake_bin.path().join("codex"));
        assert!(should_offer_codex_acp_install());

        write_fake_executable(&fake_bin.path().join("codex-acp"));
        assert!(!should_offer_codex_acp_install());

        unsafe {
            match prev_path {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    #[test]
    fn available_vendors_follow_configured_programs() {
        let config = AcpConfig::from_agents([
            AcpAgentDefinition {
                vendor: VendorKind::Claude,
                program: "/definitely/missing/claude-acp".to_string(),
                args: Vec::new(),
                env: BTreeMap::new(),
            },
            AcpAgentDefinition {
                vendor: VendorKind::Codex,
                program: "/bin/sh".to_string(),
                args: Vec::new(),
                env: BTreeMap::new(),
            },
        ]);

        let available = config.available_vendors();

        assert_eq!(available.len(), 1);
        assert!(available.contains(&VendorKind::Codex));
        assert!(!available.contains(&VendorKind::Claude));
    }

    fn write_fake_executable(path: &Path) {
        std::fs::write(path, "#!/bin/sh\nexit 0\n").expect("write fake executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)
                .expect("fake executable metadata")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).expect("chmod fake executable");
        }
    }
}
