// Re-organized from tests_prompts.rs — see commit history.

use super::{prompts::*, test_harness::*};
use crate::{
    adapters::EffortLevel,
    state::{self as session_state, Phase, PipelineItemStatus, RunRecord, RunStatus, SessionState},
    tasks,
};

pub(super) fn snapshot_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/app/prompt_snapshots")
        .join(format!("{name}.txt"))
}

pub(super) fn assert_prompt_snapshot(name: &str, actual: &str) {
    let path = snapshot_path(name);
    if std::env::var("UPDATE_PROMPT_SNAPSHOTS").is_ok() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("snapshot dir");
        }
        std::fs::write(&path, actual).expect("write snapshot");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "missing snapshot {} ({}). Run with `UPDATE_PROMPT_SNAPSHOTS=1` to create it.\n--- actual ---\n{}",
            path.display(),
            err,
            actual
        )
    });
    assert_eq!(
        actual, expected,
        "prompt snapshot drift for {name}: rerun `UPDATE_PROMPT_SNAPSHOTS=1 cargo test app::tests_prompts` and review the diff before committing"
    );
}

mod chunk_00_tests;
mod chunk_01_tests;
