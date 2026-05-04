use std::{fs, path::Path};

#[test]
fn vendor_cli_adapter_renderers_are_removed() {
    for path in [
        "src/data/adapters/codex.rs",
        "src/data/adapters/claude.rs",
        "src/data/adapters/gemini.rs",
        "src/data/adapters/kimi.rs",
    ] {
        assert!(
            !Path::new(path).exists(),
            "{path} should be deleted; ACP config owns vendor subprocess entrypoints"
        );
    }

    let adapters_mod =
        fs::read_to_string("src/data/adapters/mod.rs").expect("read adapters module");
    for forbidden in [
        "AgentAdapter",
        "adapter_for_vendor",
        "interactive_command",
        "noninteractive_command",
        "detect_available_vendors",
    ] {
        assert!(
            !adapters_mod.contains(forbidden),
            "adapters module should not expose CLI command renderer boundary: {forbidden}"
        );
    }
}

#[test]
fn startup_and_app_runtime_do_not_require_tmux() {
    let main_rs = fs::read_to_string("src/main.rs").expect("read main");
    assert!(
        !main_rs.contains("current_context"),
        "top-level startup should not hard-require a tmux context"
    );
    assert!(
        !main_rs.contains("tmux::"),
        "top-level startup should not call tmux APIs"
    );

    for path in [
        "src/app/lifecycle.rs",
        "src/app/finalization.rs",
        "src/app/prompts.rs",
        "src/app/yolo_exit.rs",
        "src/data/preflight.rs",
        "src/ui/preflight.rs",
    ] {
        let text = fs::read_to_string(path).unwrap_or_else(|err| panic!("{path}: {err}"));
        assert!(
            !text.contains("Command::new(\"tmux\")"),
            "{path} should not shell out to tmux as an ACP runtime boundary"
        );
        assert!(
            !text.contains("TmuxContext"),
            "{path} should not require tmux context"
        );
    }
}

#[test]
fn acp_completion_does_not_use_shell_status_files() {
    for path in [
        "src/data/runner.rs",
        "src/app/finalization.rs",
        "src/app/yolo_exit.rs",
    ] {
        let text = fs::read_to_string(path).unwrap_or_else(|err| panic!("{path}: {err}"));
        assert!(
            !text.contains("run-status"),
            "{path} should not use shell-era run-status files"
        );
        assert!(
            !text.contains("status_path"),
            "{path} should not pass status files through ACP runtime boundaries"
        );
    }
}
