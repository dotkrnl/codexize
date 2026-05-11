//! Prompt rendering snapshot suite.
//!
//! Each prompt builder is rendered against fixed-path fixtures so the
//! `insta` snapshots pin the agent-visible body. The test runs under
//! `with_temp_root_and_cwd` because `prompts::project_doc_instr` walks the
//! current working directory for `CLAUDE.md` / `AGENTS.md`, and we need the
//! cwd to be the empty tempdir so the snapshot is stable regardless of what
//! the host repo carries.

use super::test_support::with_temp_root_and_cwd;
use crate::app::prompts::*;

fn assert_prompt_insta_snapshot(name: &str, actual: &str) {
    if !name.starts_with("live_summary") {
        assert_memory_block(name, actual);
    }
    insta::with_settings!({
        description => "Prompt output snapshot",
        omit_expression => true,
    }, {
        insta::assert_snapshot!(name, actual);
    });
}

fn assert_memory_block(name: &str, actual: &str) {
    assert!(
        actual.contains("Project Memory"),
        "{name} prompt must include the shared memory block"
    );
    assert!(
        actual.contains(".codexize/memory/index.md"),
        "{name} prompt must point agents at the project memory index"
    );
    assert!(
        actual.contains("Do not read the entire memory directory"),
        "{name} prompt must forbid full-store memory scans"
    );
}

fn assert_capture_lessons_block(name: &str, actual: &str) {
    assert!(
        actual.contains("Capture lessons"),
        "{name} prompt must include the Capture lessons paragraph"
    );
    assert!(
        actual.contains(".codexize/memory/journal/"),
        "{name} prompt must reference the journal directory"
    );
    assert!(
        actual.contains("no new lesson"),
        "{name} prompt must mention the no-new-lesson fallback"
    );
    assert!(
        actual.contains("write_file"),
        "{name} prompt must name the write_file tool for journal creation"
    );
}

// `with_temp_root_and_cwd` chdir's the entire process; guard tests in
// `app::guard` shell out to `git` from cwd, so the two cannot run
// concurrently without serializing.
#[test]
#[serial_test::serial(process_cwd)]
fn prompt_insta_snapshots_match_fixtures() {
    use std::path::{Path, PathBuf};
    with_temp_root_and_cwd(|_root| {
        let session_dir = PathBuf::from("/tmp/codexize-prompt-fixture/session");
        let artifacts = session_dir.join("artifacts");
        let round1 = session_dir.join("rounds/001");
        let round3 = session_dir.join("rounds/003");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::create_dir_all(&round1).unwrap();
        std::fs::create_dir_all(&round3).unwrap();

        let spec = artifacts.join("spec.md");
        let plan = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let summary = artifacts.join("session_summary.toml");
        let live = artifacts.join("live_summary.txt");
        let recovery = round1.join("recovery.toml");
        let task_file_r1 = round1.join("task.toml");
        let task_file_r3 = round3.join("task.toml");
        let review_scope_r1 = round1.join("review_scope.toml");
        let review_scope_r3 = round3.join("review_scope.toml");
        let simplification = round1.join("simplification.toml");
        let review_r1 = round1.join("review.toml");
        let review_r3 = round3.join("review.toml");
        let final_verdict_r3 = round3.join("final_validation_3.toml");
        let spec_review_out = artifacts.join("spec-review-1.md");
        let plan_review_r1_out = artifacts.join("plan-review-1.md");
        let plan_review_r3_out = artifacts.join("plan-review-3.md");

        assert_prompt_insta_snapshot(
            "live_summary",
            &live_summary_instruction(&session_dir.join("artifacts/live_summary.txt")),
        );
        assert_prompt_insta_snapshot(
            "live_summary_interactive",
            &live_summary_instruction_interactive(Path::new(
                "/tmp/codexize-prompt-fixture/session/artifacts/live_summary.interactive.txt",
            )),
        );
        let spec_review = spec_review_prompt(
            &spec.display().to_string(),
            &spec_review_out.display().to_string(),
            &live.display().to_string(),
            &[],
            PromptMeta::with_topics(6),
        );
        assert_memory_block("spec_review", &spec_review);
        assert_prompt_insta_snapshot("spec_review", &spec_review);
        assert_prompt_insta_snapshot(
            "plan_review_round1",
            &plan_review_prompt(
                &spec.display().to_string(),
                &plan.display().to_string(),
                &plan_review_r1_out.display().to_string(),
                1,
                &live.display().to_string(),
                PromptMeta::with_topics(6),
            ),
        );
        assert_prompt_insta_snapshot(
            "plan_review_round3",
            &plan_review_prompt(
                &spec.display().to_string(),
                &plan.display().to_string(),
                &plan_review_r3_out.display().to_string(),
                3,
                &live.display().to_string(),
                PromptMeta::with_topics(6),
            ),
        );

        let idea = "fictional idea text used only to pin the snapshot";
        assert_prompt_insta_snapshot(
            "brainstorm_interactive",
            &brainstorm_prompt(
                idea,
                &spec.display().to_string(),
                &summary.display().to_string(),
                &live.display().to_string(),
                false,
                None,
                &[],
                PromptMeta::with_topics(6),
            ),
        );
        assert_prompt_insta_snapshot(
            "brainstorm_yolo",
            &brainstorm_prompt(
                idea,
                &spec.display().to_string(),
                &summary.display().to_string(),
                &live.display().to_string(),
                true,
                None,
                &[],
                PromptMeta::with_topics(6),
            ),
        );

        assert_prompt_insta_snapshot(
            "planning_interactive",
            &planning_prompt(
                &spec,
                &plan,
                &live,
                false,
                None,
                &[],
                PromptMeta::with_topics(6),
            ),
        );
        assert_prompt_insta_snapshot(
            "planning_yolo",
            &planning_prompt(
                &spec,
                &plan,
                &live,
                true,
                None,
                &[],
                PromptMeta::with_topics(6),
            ),
        );
        assert_prompt_insta_snapshot(
            "sharding",
            &sharding_prompt(&spec, &plan, &tasks_path, &live, PromptMeta::with_topics(6)),
        );
        assert_prompt_insta_snapshot(
            "final_validation",
            &final_validation_prompt(
                "fictional idea body — pinned for snapshot",
                "# Spec\n\n## User-stated requirements (authoritative)\n- pinned\n\n## Out of scope\n- pinned\n",
                &final_verdict_r3,
                &live,
                None,
                PromptMeta::with_topics(6),
            ),
        );
        let dreaming = dreaming_prompt(
            &session_dir,
            &session_dir.join("memory/dreams/dream-0001.toml"),
            &live,
            PromptMeta::with_topics(6),
        );
        assert_capture_lessons_block("dreaming", &dreaming);
        assert_prompt_insta_snapshot("dreaming", &dreaming);
        assert_prompt_insta_snapshot(
            "recovery_interactive",
            &recovery_prompt(
                &spec,
                &plan,
                &tasks_path,
                Some(7),
                Some("reviewer flagged Y"),
                &[1, 2, 3],
                &[1, 2, 3, 4, 5],
                &live,
                &recovery,
                true,
                None,
                PromptMeta::with_topics(6),
            ),
        );
        assert_prompt_insta_snapshot(
            "recovery_noninteractive",
            &recovery_prompt(
                &spec,
                &plan,
                &tasks_path,
                Some(7),
                Some("reviewer flagged Y"),
                &[1, 2, 3],
                &[1, 2, 3, 4, 5],
                &live,
                &recovery,
                false,
                None,
                PromptMeta::with_topics(6),
            ),
        );
        assert_prompt_insta_snapshot(
            "recovery_plan_review",
            &recovery_plan_review_prompt(
                &spec,
                &plan,
                &review_r1,
                &recovery,
                &live,
                &plan_review_r1_out,
                PromptMeta::with_topics(6),
            ),
        );
        assert_prompt_insta_snapshot(
            "recovery_sharding",
            &recovery_sharding_prompt(
                &spec,
                &plan,
                &live,
                &tasks_path,
                &[1, 2],
                5,
                PromptMeta::with_topics(6),
            ),
        );
        let coder_r1 = coder_prompt(CoderPromptInputs {
            session_dir: &session_dir,
            task_id: 7,
            round: 1,
            task_file: &task_file_r1,
            live_summary_path: &live,
            resume: false,
            refine_carryover: &[],
            meta: PromptMeta::with_topics(6),
        });
        assert_capture_lessons_block("coder_round1", &coder_r1);
        assert_prompt_insta_snapshot("coder_round1", &coder_r1);
        let prev_review_path = session_dir.join("rounds/002/review.toml");
        std::fs::create_dir_all(prev_review_path.parent().unwrap()).unwrap();
        std::fs::write(&prev_review_path, "status = \"refine\"\n").unwrap();
        assert_prompt_insta_snapshot(
            "coder_round3_with_carryover",
            &coder_prompt(CoderPromptInputs {
                session_dir: &session_dir,
                task_id: 7,
                round: 3,
                task_file: &task_file_r3,
                live_summary_path: &live,
                resume: true,
                refine_carryover: &[
                    "carry-1: prefer streaming reads".to_string(),
                    "carry-2: tighten error handling".to_string(),
                ],
                meta: PromptMeta::with_topics(6),
            }),
        );
        let reviewer_r1 = reviewer_prompt(ReviewerPromptInputs {
            session_dir: &session_dir,
            task_id: 7,
            round: 1,
            task_file: &task_file_r1,
            review_scope_file: &review_scope_r1,
            coder_summary_file: None,
            review_file: &review_r1,
            live_summary_path: &live,
            is_terminal_review: false,
            meta: PromptMeta::with_topics(6),
        });
        assert_capture_lessons_block("reviewer_round1", &reviewer_r1);
        assert_prompt_insta_snapshot("reviewer_round1", &reviewer_r1);
        let coder_summary_path = round3.join("coder_summary.toml");
        assert_prompt_insta_snapshot(
            "reviewer_round3_with_summary",
            &reviewer_prompt(ReviewerPromptInputs {
                session_dir: &session_dir,
                task_id: 7,
                round: 3,
                task_file: &task_file_r3,
                review_scope_file: &review_scope_r3,
                coder_summary_file: Some(&coder_summary_path),
                review_file: &review_r3,
                live_summary_path: &live,
                is_terminal_review: true,
                meta: PromptMeta::with_topics(6),
            }),
        );
        assert_prompt_insta_snapshot(
            "simplifier",
            &simplifier_prompt(
                &session_dir,
                &review_scope_r1,
                &simplification,
                &live,
                &[],
                PromptMeta::with_topics(6),
            ),
        );
        assert_prompt_insta_snapshot(
            "simplifier_with_refine",
            &simplifier_prompt(
                &session_dir,
                &review_scope_r1,
                &simplification,
                &live,
                &[
                    "rename internal `foo` helper to `parse_foo`".to_string(),
                    "tighten error message in load path".to_string(),
                ],
                PromptMeta::with_topics(6),
            ),
        );
    });
}
