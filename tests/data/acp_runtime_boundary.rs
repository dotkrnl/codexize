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
        !main_rs.contains("TmuxContext"),
        "top-level startup should not depend on a tmux runtime context"
    );
    // Cosmetic best-effort calls into `crate::ui::tmux::*` (e.g. window
    // renaming when `TMUX` is set) are explicitly allowed: the boundary
    // we're protecting is the agent runtime, not unrelated UX polish that
    // no-ops outside tmux.

    for path in [
        "src/app/lifecycle",
        "src/app/finalization",
        "src/app/prompts.rs",
        "src/app/yolo_exit.rs",
        "src/data/preflight.rs",
        "src/ui/preflight.rs",
    ] {
        for (file, text) in collect_rust_sources(path) {
            assert!(
                !text.contains("Command::new(\"tmux\")"),
                "{} should not shell out to tmux as an ACP runtime boundary",
                file.display()
            );
            assert!(
                !text.contains("TmuxContext"),
                "{} should not require tmux context",
                file.display()
            );
        }
    }
}

#[test]
fn acp_completion_does_not_use_shell_status_files() {
    for path in [
        "src/data/runner.rs",
        "src/app/finalization",
        "src/app/yolo_exit.rs",
    ] {
        for (file, text) in collect_rust_sources(path) {
            assert!(
                !text.contains("run-status"),
                "{} should not use shell-era run-status files",
                file.display()
            );
            assert!(
                !text.contains("status_path"),
                "{} should not pass status files through ACP runtime boundaries",
                file.display()
            );
        }
    }
}

// Walks `path` when it points at a single .rs file, or every .rs under it
// when it's a directory — handles both `src/app/foo.rs` and the submodule
// layout `src/app/foo/{mod,...}.rs` produced by the orchestrator slice.
fn collect_rust_sources(path: &str) -> Vec<(std::path::PathBuf, String)> {
    let p = Path::new(path);
    let mut out = Vec::new();
    if p.is_file() {
        let text = fs::read_to_string(p).unwrap_or_else(|err| panic!("{path}: {err}"));
        out.push((p.to_path_buf(), text));
        return out;
    }
    if p.is_dir() {
        let mut stack = vec![p.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
            {
                let entry = entry.expect("dir entry");
                let entry_path = entry.path();
                if entry_path.is_dir() {
                    stack.push(entry_path);
                } else if entry_path.extension().map(|e| e == "rs").unwrap_or(false) {
                    let text = fs::read_to_string(&entry_path)
                        .unwrap_or_else(|err| panic!("{}: {err}", entry_path.display()));
                    out.push((entry_path, text));
                }
            }
        }
        return out;
    }
    panic!("{path}: not a file or directory");
}
