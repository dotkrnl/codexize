use super::*;

#[test]
fn every_prompt_builder_renders_without_unbound_placeholders() {
    use std::path::{Path, PathBuf};
    with_temp_root(|| {
        let dir = session_state::session_dir("prompt-coverage-fixture");
        let artifacts = dir.join("artifacts");
        let round = dir.join("rounds").join("002");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::create_dir_all(&round).unwrap();

        let spec = artifacts.join("spec.md");
        let plan = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let live = artifacts.join("live_summary.txt");
        let summary = artifacts.join("session_summary.toml");
        let recovery = round.join("recovery.toml");
        let task_file = round.join("task.toml");
        let review_scope = round.join("review_scope.toml");
        let simplification_toml = round.join("simplification.toml");
        let review = round.join("review.toml");
        let plan_review = artifacts.join("plan-review-2.md");
        let triggering_review = round.join("review.toml");

        // Cover both rounds == 1 and rounds > 1 paths so prior_block /
        // prior_reviews placeholders resolve through the >1 branch too.
        let _ = spec_review_prompt(
            &spec.display().to_string(),
            &artifacts.join("spec-review-1.md").display().to_string(),
            &live.display().to_string(),
        );
        let _ = plan_review_prompt(
            &spec.display().to_string(),
            &plan.display().to_string(),
            &plan_review.display().to_string(),
            2,
            &live.display().to_string(),
        );

        for yolo in [false, true] {
            let _ = brainstorm_prompt(
                "idea text",
                &spec.display().to_string(),
                &summary.display().to_string(),
                &live.display().to_string(),
                yolo,
            );
            let _ = planning_prompt(
                &spec,
                &[PathBuf::from("artifacts/spec-review-1.md")],
                &plan,
                &live,
                yolo,
            );
        }

        let _ = sharding_prompt(&spec, &plan, &tasks_path, &live);
        let _ = final_validation_prompt(
            "raw idea body",
            "# Spec body",
            &round.join("final_validation_2.toml"),
            &live,
            None,
        );
        for interactive in [false, true] {
            let _ = recovery_prompt(
                &spec,
                &plan,
                &tasks_path,
                Some(7),
                Some("reviewer flagged Y"),
                &[1, 2, 3],
                &[1, 2, 3, 4, 5],
                &live,
                &recovery,
                interactive,
            );
        }
        let _ = recovery_plan_review_prompt(
            &spec,
            &plan,
            &triggering_review,
            &recovery,
            &live,
            &plan_review,
        );
        let _ = recovery_sharding_prompt(&spec, &plan, &live, &tasks_path, &[1, 2], 5);

        let _ = coder_prompt(
            &dir,
            7,
            2,
            &task_file,
            &live,
            true,
            &["carry-1".to_string(), "carry-2".to_string()],
        );
        let _ = reviewer_prompt(ReviewerPromptInputs {
            session_dir: &dir,
            task_id: 7,
            round: 2,
            task_file: &task_file,
            review_scope_file: &review_scope,
            coder_summary_file: Some(Path::new("artifacts/coder_summary_2.toml")),
            review_file: &review,
            live_summary_path: &live,
        });
        let _ = simplifier_prompt(&dir, &review_scope, &simplification_toml, &live);
    });
}

#[test]
fn shipped_templates_emit_literal_braces_unescaped() {
    let session_dir = std::path::Path::new("/tmp/literal-brace-fixture");
    let spec = session_dir.join("artifacts/spec.md");
    let plan = session_dir.join("artifacts/plan.md");
    let tasks_path = session_dir.join("artifacts/tasks.toml");
    let live = session_dir.join("artifacts/live_summary.txt");

    let sharding = sharding_prompt(&spec, &plan, &tasks_path, &live);
    // The sharding prompt embeds a TOML snippet with `{ path, lines }`
    // arrays; both literal braces must come through as single characters.
    assert!(sharding.contains("{ path, lines }"));
    assert!(sharding.contains("{ path = \"artifacts/spec.md\", lines = \"10-45\" }"));
}

#[test]
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
        assert_prompt_insta_snapshot(
            "spec_review",
            &spec_review_prompt(
                &spec.display().to_string(),
                &spec_review_out.display().to_string(),
                &live.display().to_string(),
            ),
        );
        assert_prompt_insta_snapshot(
            "plan_review_round1",
            &plan_review_prompt(
                &spec.display().to_string(),
                &plan.display().to_string(),
                &plan_review_r1_out.display().to_string(),
                1,
                &live.display().to_string(),
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
            ),
        );

        let spec_reviews = vec![
            artifacts.join("spec-review-1.md"),
            artifacts.join("spec-review-2.md"),
        ];
        assert_prompt_insta_snapshot(
            "planning_interactive",
            &planning_prompt(&spec, &spec_reviews, &plan, &live, false),
        );
        assert_prompt_insta_snapshot(
            "planning_yolo",
            &planning_prompt(&spec, &spec_reviews, &plan, &live, true),
        );
        assert_prompt_insta_snapshot(
            "sharding",
            &sharding_prompt(&spec, &plan, &tasks_path, &live),
        );
        assert_prompt_insta_snapshot(
            "final_validation",
            &final_validation_prompt(
                "fictional idea body — pinned for snapshot",
                "# Spec\n\n## User-stated requirements (authoritative)\n- pinned\n\n## Out of scope\n- pinned\n",
                &final_verdict_r3,
                &live,
                None,
            ),
        );
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
            ),
        );
        assert_prompt_insta_snapshot(
            "recovery_sharding",
            &recovery_sharding_prompt(&spec, &plan, &live, &tasks_path, &[1, 2], 5),
        );
        assert_prompt_insta_snapshot(
            "coder_round1",
            &coder_prompt(&session_dir, 7, 1, &task_file_r1, &live, false, &[]),
        );
        let prev_review_path = session_dir.join("rounds/002/review.toml");
        std::fs::create_dir_all(prev_review_path.parent().unwrap()).unwrap();
        std::fs::write(&prev_review_path, "status = \"refine\"\n").unwrap();
        assert_prompt_insta_snapshot(
            "coder_round3_with_carryover",
            &coder_prompt(
                &session_dir,
                7,
                3,
                &task_file_r3,
                &live,
                true,
                &[
                    "carry-1: prefer streaming reads".to_string(),
                    "carry-2: tighten error handling".to_string(),
                ],
            ),
        );
        assert_prompt_insta_snapshot(
            "reviewer_round1",
            &reviewer_prompt(ReviewerPromptInputs {
                session_dir: &session_dir,
                task_id: 7,
                round: 1,
                task_file: &task_file_r1,
                review_scope_file: &review_scope_r1,
                coder_summary_file: None,
                review_file: &review_r1,
                live_summary_path: &live,
            }),
        );
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
            }),
        );
        assert_prompt_insta_snapshot(
            "simplifier",
            &simplifier_prompt(&session_dir, &review_scope_r1, &simplification, &live),
        );
    });
}
