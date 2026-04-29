use codexize::{
    adapters::{EffortLevel, window_name_with_model},
    app,
    selection::VendorKind,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn snapshot_path(name: &str) -> PathBuf {
    Path::new("tests/fixtures/snapshots").join(name)
}

fn assert_snapshot(name: &str, actual: String) {
    let expected = fs::read_to_string(snapshot_path(name))
        .unwrap_or_else(|err| panic!("{name}: {err}\n--- actual ---\n{actual}"));
    assert_eq!(actual, expected, "snapshot mismatch for {name}");
}

fn run_help(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_codexize"))
        .args(args)
        .output()
        .expect("run codexize");
    assert!(
        output.status.success(),
        "help command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 help output")
}

#[test]
fn cli_help_matches_snapshot() {
    assert_snapshot("cli-help.txt", run_help(&["--help"]));
    assert_snapshot("cli-agent-run-help.txt", run_help(&["agent-run", "--help"]));
}

#[test]
fn footer_and_status_strings_match_snapshot() {
    assert_snapshot("footer-keymap.txt", app::snapshot_default_footer_keymap(80));
    assert_snapshot("status-line.txt", app::snapshot_warn_status_line());
}

#[test]
fn tmux_name_templates_match_snapshot() {
    let snapshot = [
        "session=<current tmux session>".to_string(),
        format!(
            "brainstorm={}",
            window_name_with_model(
                "[Brainstorm]",
                "claude-opus-4-7",
                VendorKind::Claude,
                EffortLevel::Normal,
            )
        ),
        format!(
            "coder_tough={}",
            window_name_with_model(
                "[Round 1 Coder]",
                "gpt-5.5",
                VendorKind::Codex,
                EffortLevel::Tough,
            )
        ),
        format!(
            "planning_low={}",
            window_name_with_model(
                "[Planning]",
                "claude-sonnet-4.6",
                VendorKind::Claude,
                EffortLevel::Low,
            )
        ),
    ]
    .join("\n");
    assert_snapshot("tmux-templates.txt", format!("{snapshot}\n"));
}
