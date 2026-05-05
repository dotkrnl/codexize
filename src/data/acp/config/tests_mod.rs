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
        policy: super::super::AcpLaunchPolicy::final_validation(&verdict_path, &live_summary_path),
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
fn simplifier_policy_keeps_workspace_writable_with_full_shell_access() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let simplification_path = temp.path().join("rounds/001/simplification.toml");
    let live_summary_path = temp
        .path()
        .join("artifacts/live_summary.simplifier-stage-r1-a1.txt");
    let request = AcpLaunchRequest {
        cwd: temp.path().to_path_buf(),
        policy: super::super::AcpLaunchPolicy::simplifier(&simplification_path, &live_summary_path),
        ..sample_request(VendorKind::Codex)
    };

    let resolved = AcpConfig::default()
        .resolve(&request)
        .expect("resolve codex");

    // Code-producing parity with coder/reviewer: Code permission mode,
    // workspace not enforced read-only, shell policy is full access.
    assert_eq!(resolved.session.permission_mode, AcpPermissionMode::Code);
    assert!(!resolved.session.policy.enforce_readonly_workspace);
    assert!(matches!(
        resolved.session.policy.shell_policy,
        super::super::AcpShellCommandPolicy::FullAccess
    ));
    // Mandatory write paths still advertised so the runtime can surface
    // misrouted required-output writes.
    assert_eq!(
        resolved.session.policy.allowed_write_paths,
        vec![simplification_path.clone(), live_summary_path.clone()]
    );
    assert_eq!(
        resolved
            .spawn
            .env
            .get("CODEXIZE_ACP_ALLOWED_WRITE_PATHS")
            .cloned(),
        Some(format!(
            "{}\n{}",
            simplification_path.display(),
            live_summary_path.display()
        ))
    );
    assert_eq!(
        resolved
            .spawn
            .env
            .get("CODEXIZE_ACP_ENFORCE_READONLY_WORKSPACE")
            .map(String::as_str),
        Some("false")
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
