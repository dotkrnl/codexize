use super::*;
use crate::{acp, adapters::EffortLevel, state::LaunchModes};

// Driving the production launch path end-to-end for a Free candidate.
// `assemble_universe` produces the row + selected candidate; the
// `pick_cli_and_launch_name` helper (used by every stage) picks the
// candidate's `cli` / `launch_name`; an `AcpLaunchRequest` built from
// those values must resolve to the verbatim free model name passed to
// the operator-chosen CLI — no provider prefixing, no row-name leak.
fn make_dashboard_entry(name: &str, vendor: &str) -> crate::cache::DashboardEntry {
    crate::cache::DashboardEntry {
        vendor: vendor.to_string(),
        name: name.to_string(),
        overall_score: 80.0,
        current_score: 78.0,
        standard_error: 1.0,
        axes: Vec::new(),
        axis_provenance: std::collections::BTreeMap::new(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
        score_source: crate::selection::ScoreSource::Ipbr,
        ipbr_row_matched: true,
        ipbr_match_key: Some(name.to_string()),
        route_underlying_vendor: None,
        route_provider: None,
        display_order: 0,
        fallback_from: None,
    }
}

#[test]
fn free_model_entry_resolves_to_verbatim_launch_name_through_acp_path() {
    use crate::selection::{CliKind, FreeModelEntry, SubscriptionKind};
    use std::collections::{BTreeMap, BTreeSet};

    // Direct candidate at 50% — Free at 100% must beat it.
    let dashboard = vec![make_dashboard_entry("deepseek-v4-flash", "codex")];
    let mut quotas: crate::cache::QuotaPayload = BTreeMap::new();
    quotas
        .entry("codex".to_string())
        .or_default()
        .insert("deepseek-v4-flash".to_string(), Some(50));
    let free_models = vec![FreeModelEntry {
        mapped_into: "deepseek-v4-flash".to_string(),
        cli: CliKind::Opencode,
        model_name: "dsk-4-flash".to_string(),
    }];
    let available = BTreeSet::from([SubscriptionKind::Codex, SubscriptionKind::OpencodeGo]);

    let (models, warnings) = crate::logic::selection::assemble::assemble_universe(
        dashboard,
        quotas,
        BTreeMap::new(),
        &available,
        &free_models,
    );
    assert!(warnings.is_empty(), "expected no warnings: {warnings:?}");
    let row = models
        .iter()
        .find(|m| m.name == "deepseek-v4-flash")
        .expect("deepseek-v4-flash row");

    // Stage launch sites flow through this helper to derive cli/launch_name.
    let (cli, launch_name) = crate::app_runtime::stages::pick_cli_and_launch_name(row);
    assert_eq!(cli, CliKind::Opencode);
    assert_eq!(launch_name, "dsk-4-flash");

    let request = AcpLaunchRequest {
        // Subscription on the AgentRun (and hence the request) is the
        // selected candidate's subscription, not the row's compatibility
        // mirror — Free is what routes through the chosen CLI in resolve().
        vendor: SubscriptionKind::Free,
        cwd: PathBuf::from("workspace"),
        prompt: acp::PromptPayload::Text("prompt".to_string()),
        model: row.name.clone(),
        route_provider: row.route_provider.clone(),
        cli,
        launch_name,
        requested_effort: EffortLevel::Normal,
        effective_effort: EffortLevel::Normal,
        interactive: false,
        modes: LaunchModes {
            yolo: true,
            cheap: false,
            interactive: false,
        },
        policy: acp::AcpLaunchPolicy::default(),
    };

    let resolved = AcpConfig::default()
        .resolve(&request)
        .expect("resolve free candidate via opencode");

    // The launch model is the operator's verbatim string: no `opencode/`
    // prefix even though we are launching through the opencode CLI, and
    // the row's canonical ipbr name (`deepseek-v4-flash`) never leaks
    // into the spawn metadata.
    assert_eq!(resolved.spawn.program, "opencode");
    assert_eq!(resolved.session.model, "dsk-4-flash");
    assert_eq!(
        resolved
            .spawn
            .env
            .get("CODEXIZE_ACP_MODEL")
            .map(String::as_str),
        Some("dsk-4-flash")
    );
    assert_eq!(
        resolved
            .spawn
            .env
            .get("CODEXIZE_ACP_VENDOR")
            .map(String::as_str),
        Some("free")
    );
}

fn sample_request(vendor: SubscriptionKind) -> AcpLaunchRequest {
    AcpLaunchRequest {
        vendor,
        cwd: PathBuf::from("workspace"),
        prompt: acp::PromptPayload::Text("prompt".to_string()),
        model: "gpt-5.5".to_string(),
        route_provider: None,
        cli: crate::selection::CliKind::Codex,
        launch_name: "gpt-5.5".to_string(),
        requested_effort: EffortLevel::Normal,
        effective_effort: EffortLevel::Low,
        interactive: false,
        modes: LaunchModes {
            yolo: true,
            cheap: true,
            interactive: false,
        },
        policy: acp::AcpLaunchPolicy::default(),
    }
}

fn non_yolo_request(vendor: SubscriptionKind) -> AcpLaunchRequest {
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
        .resolve(&sample_request(SubscriptionKind::Gemini))
        .expect("resolve gemini");

    assert_eq!(resolved.vendor, SubscriptionKind::Gemini);
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
        .resolve(&sample_request(SubscriptionKind::Claude))
        .expect_err("missing config");
    assert!(matches!(err, AcpError::HumanBlock(_)));
}

#[test]
fn launch_translation_preserves_model_and_cheap_derived_effort() {
    let resolved = AcpConfig::default()
        .resolve(&sample_request(SubscriptionKind::Codex))
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
fn opencode_launch_prefixes_bare_inventory_model_for_acp() {
    let request = AcpLaunchRequest {
        model: "gpt-5-nano".to_string(),
        ..sample_request(SubscriptionKind::OpencodeGo)
    };

    let resolved = AcpConfig::default()
        .resolve(&request)
        .expect("resolve opencode");

    assert_eq!(resolved.spawn.program, "opencode");
    assert_eq!(resolved.spawn.args, vec!["acp".to_string()]);
    assert_eq!(resolved.session.model, "opencode/gpt-5-nano");
    assert_eq!(
        resolved
            .spawn
            .env
            .get("CODEXIZE_ACP_MODEL")
            .map(String::as_str),
        Some("opencode/gpt-5-nano")
    );
    assert_eq!(
        resolved
            .session
            .metadata
            .get("codexize.model")
            .map(String::as_str),
        Some("opencode/gpt-5-nano")
    );
}

#[test]
fn opencode_launch_preserves_provider_qualified_model() {
    let request = AcpLaunchRequest {
        model: "opencode/big-pickle".to_string(),
        ..sample_request(SubscriptionKind::OpencodeGo)
    };

    let resolved = AcpConfig::default()
        .resolve(&request)
        .expect("resolve opencode");

    assert_eq!(resolved.session.model, "opencode/big-pickle");
    assert_eq!(
        resolved
            .spawn
            .env
            .get("CODEXIZE_ACP_MODEL")
            .map(String::as_str),
        Some("opencode/big-pickle")
    );
}

#[test]
fn opencode_go_route_provider_drives_launch_qualifier() {
    // route_provider = "opencode-go" must reach the spawn as
    // `opencode-go/<id>` so the Go-tier API URL is hit, not the zen tier.
    // Without route_provider the launch would default to `opencode/<id>`,
    // which would 404 against opencode-go-only models like deepseek.
    let request = AcpLaunchRequest {
        model: "deepseek-v4-flash".to_string(),
        route_provider: Some("opencode-go".to_string()),
        ..sample_request(SubscriptionKind::OpencodeGo)
    };

    let resolved = AcpConfig::default()
        .resolve(&request)
        .expect("resolve opencode-go");

    assert_eq!(resolved.session.model, "opencode-go/deepseek-v4-flash");
    assert_eq!(
        resolved
            .spawn
            .env
            .get("CODEXIZE_ACP_MODEL")
            .map(String::as_str),
        Some("opencode-go/deepseek-v4-flash")
    );
}

#[test]
fn acp_launches_use_code_permission_mode_even_without_codexize_yolo() {
    let resolved = AcpConfig::default()
        .resolve(&non_yolo_request(SubscriptionKind::Kimi))
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
        policy: acp::AcpLaunchPolicy::final_validation(&verdict_path, &live_summary_path),
        ..sample_request(SubscriptionKind::Codex)
    };

    let resolved = AcpConfig::default()
        .resolve(&request)
        .expect("resolve codex");

    assert_eq!(resolved.session.permission_mode, AcpPermissionMode::Code);
    assert_eq!(
        resolved.session.policy.allowed_write_paths,
        vec![
            verdict_path.clone(),
            live_summary_path.clone(),
            temp.path().join(".codexize/memory/**")
        ]
    );
    assert!(resolved.session.policy.enforce_readonly_workspace);
    assert!(matches!(
        resolved.session.policy.shell_policy,
        acp::AcpShellCommandPolicy::Allowlist(_)
    ));
    assert_eq!(
        resolved
            .spawn
            .env
            .get("CODEXIZE_ACP_ALLOWED_WRITE_PATHS")
            .cloned(),
        Some(format!(
            "{}\n{}\n{}",
            verdict_path.display(),
            live_summary_path.display(),
            temp.path().join(".codexize/memory/**").display()
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
fn dreaming_policy_allows_only_memory_report_and_live_summary_writes() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let report_path = temp.path().join(".codexize/memory/dreams/dream-0002.toml");
    let live_summary_path = temp
        .path()
        .join(".codexize/sessions/session/artifacts/live_summary.dreaming-r2-a1.txt");
    let request = AcpLaunchRequest {
        cwd: temp.path().to_path_buf(),
        policy: acp::AcpLaunchPolicy::dreaming(&report_path, &live_summary_path),
        ..sample_request(SubscriptionKind::Codex)
    };

    let resolved = AcpConfig::default()
        .resolve(&request)
        .expect("resolve codex");

    assert_eq!(resolved.session.permission_mode, AcpPermissionMode::Code);
    assert_eq!(
        resolved.session.policy.allowed_write_paths,
        vec![
            temp.path().join(".codexize/memory/**"),
            report_path.clone(),
            live_summary_path.clone()
        ]
    );
    assert!(resolved.session.policy.enforce_readonly_workspace);
    assert!(matches!(
        resolved.session.policy.shell_policy,
        acp::AcpShellCommandPolicy::Allowlist(_)
    ));
    assert_eq!(
        resolved
            .spawn
            .env
            .get("CODEXIZE_ACP_ALLOWED_WRITE_PATHS")
            .cloned(),
        Some(format!(
            "{}\n{}\n{}",
            temp.path().join(".codexize/memory/**").display(),
            report_path.display(),
            live_summary_path.display()
        ))
    );
    assert_eq!(
        resolved
            .spawn
            .env
            .get("CODEXIZE_ACP_ENFORCE_READONLY_WORKSPACE")
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
        policy: acp::AcpLaunchPolicy::simplifier(&simplification_path, &live_summary_path),
        ..sample_request(SubscriptionKind::Codex)
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
        acp::AcpShellCommandPolicy::FullAccess
    ));
    // Mandatory write paths still advertised so the runtime can surface
    // misrouted required-output writes. Memory glob is included so the
    // simplifier can append durable lessons it discovers while collapsing
    // implementation details.
    let memory_glob = temp.path().join(".codexize/memory/**");
    assert_eq!(
        resolved.session.policy.allowed_write_paths,
        vec![
            simplification_path.clone(),
            live_summary_path.clone(),
            memory_glob.clone(),
        ]
    );
    assert_eq!(
        resolved
            .spawn
            .env
            .get("CODEXIZE_ACP_ALLOWED_WRITE_PATHS")
            .cloned(),
        Some(format!(
            "{}\n{}\n{}",
            simplification_path.display(),
            live_summary_path.display(),
            memory_glob.display()
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

    let program = claude_acp_local_program_for(&claude_acp_install_root());

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

    let install_root = claude_acp_install_root();

    assert!(!should_offer_claude_acp_install_for(&install_root));

    write_fake_executable(&fake_bin.path().join("claude"));
    assert!(should_offer_claude_acp_install_for(&install_root));

    write_fake_executable(&fake_bin.path().join("claude-agent-acp"));
    assert!(!should_offer_claude_acp_install_for(&install_root));

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
            vendor: SubscriptionKind::Claude,
            program: "/definitely/missing/claude-acp".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
        },
        AcpAgentDefinition {
            vendor: SubscriptionKind::Codex,
            program: "/bin/sh".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
        },
    ]);

    let available = config.available_vendors();

    assert_eq!(available.len(), 1);
    assert!(available.contains(&SubscriptionKind::Codex));
    assert!(!available.contains(&SubscriptionKind::Claude));
}

#[test]
fn available_vendors_include_opencode_only_when_program_is_executable() {
    let config = AcpConfig::from_agents([
        AcpAgentDefinition {
            vendor: SubscriptionKind::OpencodeGo,
            program: "/definitely/missing/opencode".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
        },
        AcpAgentDefinition {
            vendor: SubscriptionKind::Codex,
            program: "/bin/sh".to_string(),
            args: Vec::new(),
            env: BTreeMap::new(),
        },
    ]);

    let available = config.available_vendors();

    assert!(available.contains(&SubscriptionKind::Codex));
    assert!(!available.contains(&SubscriptionKind::OpencodeGo));
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
