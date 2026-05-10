use codexize::{
    adapters::{EffortLevel, run_label_with_model},
    app,
    data::config::schema::EffortMapping,
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
}

#[test]
fn footer_and_status_strings_match_snapshot() {
    assert_snapshot(
        "footer-keymap.txt",
        app::snapshot_support::default_footer_keymap(80),
    );
    assert_snapshot("status-line.txt", app::snapshot_support::warn_status_line());
}

#[test]
fn acp_run_labels_match_snapshot() {
    // Per-CLI effort mappings mirror what the baked provider rows ship for
    // Claude (`max` on tough) and Codex (`xhigh` on tough). The launch label
    // builder drives off the AgentRun's mapping/eligible fields, not a
    // vendor-keyed table — these literal values keep the snapshot stable
    // even if a future row tweaks its `effort_*` tokens.
    let claude_mapping = EffortMapping::new("low", "medium", "max");
    let codex_mapping = EffortMapping::new("low", "medium", "xhigh");
    let snapshot = [
        format!(
            "brainstorm={}",
            run_label_with_model(
                "[Brainstorm]",
                "claude-opus-4.7",
                EffortLevel::Normal,
                true,
                &claude_mapping,
            )
        ),
        format!(
            "coder_tough={}",
            run_label_with_model(
                "[Round 1 Coder]",
                "gpt-5.5",
                EffortLevel::Tough,
                true,
                &codex_mapping,
            )
        ),
        format!(
            "planning_low={}",
            run_label_with_model(
                "[Planning]",
                "claude-sonnet-4.6",
                EffortLevel::Low,
                true,
                &claude_mapping,
            )
        ),
    ]
    .join("\n");
    assert_snapshot("acp-run-labels.txt", format!("{snapshot}\n"));
}
