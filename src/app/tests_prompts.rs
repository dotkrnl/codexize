// tests_prompts.rs
use super::{prompts::*, test_harness::*};
use crate::{
    adapters::EffortLevel,
    state::{self as session_state, Phase, PipelineItemStatus, RunRecord, RunStatus, SessionState},
    tasks,
};

#[test]
fn review_banner_round_trip_restores_original_bytes() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("spec.md");
    let original = "# Spec\n\nbody line one\nbody line two\n";
    std::fs::write(&path, original).unwrap();

    assert!(prepend_review_banner(&path));
    let with_banner = std::fs::read_to_string(&path).unwrap();
    assert!(with_banner.starts_with(REVIEW_BANNER));
    assert!(with_banner.ends_with(original));

    strip_review_banner(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn review_banner_strip_is_noop_when_banner_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("plan.md");
    // User edited the banner away (or it was never there): we must not
    // silently delete the first N lines.
    let edited = "# Plan\n\nactual content\n";
    std::fs::write(&path, edited).unwrap();
    strip_review_banner(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
}

#[test]
fn review_banner_prepend_is_idempotent() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("spec.md");
    std::fs::write(&path, "# Spec\nbody\n").unwrap();
    assert!(prepend_review_banner(&path));
    // Second prepend on the same file must not stack a second banner.
    assert!(!prepend_review_banner(&path));
    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(contents.matches(REVIEW_BANNER).count(), 1);
}

#[test]
fn review_scope_fresh_write_omits_dirty_after() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("review-scope-fresh-schema");
        let round_dir = session_dir.join("rounds").join("001");

        write_review_scope_artifact(&round_dir, "base123").expect("write review scope");

        let text = std::fs::read_to_string(round_dir.join("review_scope.toml")).unwrap();
        assert_eq!(text, "base_sha = \"base123\"\n");
    });
}

#[test]
fn review_round_row_can_be_expanded_for_multiround_transcript_access() {
    let mut state = SessionState::new("review-drilldown".to_string());
    state.current_phase = Phase::SpecReviewRunning;
    state.agent_runs.push(RunRecord {
        id: 31,
        stage: "spec-review".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Spec Review 1]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: Some(chrono::Utc::now()),
        status: RunStatus::Done,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    state.agent_runs.push(RunRecord {
        id: 32,
        stage: "spec-review".to_string(),
        task_id: None,
        round: 2,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Spec Review 2]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    let mut app = mk_app(state);
    let round_one_idx = row_index(&app, "Round 1");
    app.selected = round_one_idx;

    assert!(app.is_expanded_body(round_one_idx));
    assert_eq!(
        app.visible_rows[round_one_idx].backing_leaf_run_id,
        Some(31)
    );
}

#[test]
fn review_human_blocked_enters_builder_recovery() {
    with_temp_root(|| {
        let session_id = "review-blocked-recovery";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("rounds").join("001")).expect("round dir");
        std::fs::write(
            session_dir.join("rounds").join("001").join("review.toml"),
            r#"status = "human_blocked"
summary = "needs recovery"
feedback = ["task 2 is superseded"]
"#,
        )
        .expect("review file");
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ReviewRound(1);
        state.builder.current_task = Some(2);
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "reviewer".to_string(),
            task_id: Some(2),
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Review]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        let mut app = idle_app(state);
        let run = app.state.agent_runs[0].clone();
        app.finalize_current_run(&run).expect("finalize review");
        assert_eq!(app.state.current_phase, Phase::BuilderRecovery(1));
        assert_eq!(app.state.builder.current_task, None);
        assert_eq!(app.state.builder.recovery_trigger_task_id, Some(2));
    });
}

#[test]
fn review_revise_with_new_tasks_rewrites_queue_and_advances_to_inserted_task() {
    with_temp_root(|| {
        let session_id = "review-revise-new-tasks";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        std::fs::write(
            artifacts.join("tasks.toml"),
            r#"[[tasks]]
id = 1
title = "Finished"
description = "done"
test = "cargo test"
estimated_tokens = 10

[[tasks]]
id = 2
title = "Too broad"
description = "split me"
test = "cargo test"
estimated_tokens = 20

[[tasks]]
id = 3
title = "Later"
description = "preserve this"
test = "cargo test runner::"
estimated_tokens = 30

[[tasks.spec_refs]]
path = "spec.md"
lines = "1-2"
"#,
        )
        .expect("tasks file");
        std::fs::write(
            round_dir.join("review.toml"),
            r#"status = "revise"
summary = "split required"
feedback = ["split into smaller work"]

[[new_tasks]]
id = 0
title = "Split A"
description = "first half"
test = "cargo test transitions::"
estimated_tokens = 11

[[new_tasks]]
id = 0
title = "Split B"
description = "second half"
test = "cargo test runner::"
estimated_tokens = 12
"#,
        )
        .expect("review file");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ReviewRound(1);
        state.builder.reset_task_pipeline(vec![
            (1, Some("Finished".to_string())),
            (2, Some("Too broad".to_string())),
            (3, Some("Later".to_string())),
        ]);
        let _ = state
            .builder
            .set_task_status(1, PipelineItemStatus::Approved, Some(1));
        let _ = state
            .builder
            .set_task_status(2, PipelineItemStatus::Running, Some(1));
        state.builder.current_task = Some(2);
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "reviewer".to_string(),
            task_id: Some(2),
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Review]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let mut app = idle_app(state);
        let run = app.state.agent_runs[0].clone();
        app.finalize_current_run(&run).expect("finalize review");

        assert_eq!(app.state.current_phase, Phase::ImplementationRound(2));
        assert_eq!(
            app.state.builder.pending_task_ids().first().copied(),
            Some(4)
        );
        let parsed = tasks::validate(&artifacts.join("tasks.toml")).expect("tasks valid");
        let ids = parsed.tasks.iter().map(|task| task.id).collect::<Vec<_>>();
        assert_eq!(ids, vec![1, 4, 5, 6]);
        assert_eq!(parsed.tasks[1].title, "Split A");
        assert_eq!(parsed.tasks[2].title, "Split B");
        assert_eq!(parsed.tasks[3].title, "Later");
        assert_eq!(parsed.tasks[3].spec_refs[0].lines, "1-2");
    });
}

#[test]
fn review_prompts_protect_authoritative_user_requirements() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("review-authoritative-section");
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let spec_review_path = artifacts.join("spec-review-1.md");
        let plan_review_path = artifacts.join("plan-review-1.md");
        let live_summary = artifacts.join("live_summary.txt");
        std::fs::create_dir_all(&artifacts).unwrap();

        let spec_prompt = spec_review_prompt(
            &spec_path.display().to_string(),
            &spec_review_path.display().to_string(),
            &live_summary.display().to_string(),
        );
        assert!(spec_prompt.contains(
            "Treat the `## User-stated requirements (authoritative)` section as\n    read-only."
        ));
        assert!(spec_prompt.contains("Do not propose edits to that section."));

        let plan_prompt = plan_review_prompt(
            &spec_path.display().to_string(),
            &plan_path.display().to_string(),
            &plan_review_path.display().to_string(),
            1,
            &live_summary.display().to_string(),
        );
        assert!(
            plan_prompt
                .contains("NEVER the `## User-stated requirements\n(authoritative)` section")
        );
        assert!(plan_prompt.contains("it must be raised to the\noperator, not patched"));
    });
}

#[test]
fn live_summary_instruction_requires_immediate_creation_and_current_updates() {
    let path = std::path::Path::new("artifacts/live_summary.test.txt");
    let prompt = live_summary_instruction(path);

    assert!(prompt.contains("Immediately create artifacts/live_summary.test.txt"));
    assert!(prompt.contains("every 2–3 min and on each sub-goal change"));
    assert!(prompt.contains("Keep this file current until you exit."));
}

#[test]
fn final_validation_prompt_embeds_idea_spec_and_precedence_rules() {
    let verdict = std::path::Path::new("artifacts/final_validation_3.toml");
    let live = std::path::Path::new("artifacts/live_summary.final-validation-r3.txt");
    let idea = "Make the validator agent run end-to-end every milestone.";
    let spec = "# Spec\n\n## User-stated requirements (authoritative)\n- run\n\n## Out of scope\n- migration\n";

    let prompt = final_validation_prompt(idea, spec, verdict, live);

    // Raw idea text and final spec text must appear verbatim.
    assert!(
        prompt.contains(idea),
        "prompt must embed raw idea text verbatim"
    );
    assert!(
        prompt.contains(spec),
        "prompt must embed final spec text verbatim"
    );

    // Source-of-truth precedence rules must be stated.
    assert!(prompt.contains("`## User-stated requirements (authoritative)`"));
    assert!(prompt.contains("`## Out of scope`"));
    assert!(
        prompt.contains("Source-of-truth precedence"),
        "prompt must explicitly call out the precedence rules"
    );
    assert!(
        prompt.contains("NOT gaps"),
        "prompt must explicitly state out-of-scope items are not gaps"
    );

    // Required workspace status check, no git diff, allowlist intent.
    assert!(prompt.contains("`git status --short`"));
    assert!(
        prompt.contains("Do **NOT** use `git diff`"),
        "prompt must explicitly forbid `git diff`"
    );
    assert!(prompt.contains("`git log` (read-only)"));

    // The two allowed output paths.
    assert!(prompt.contains(verdict.display().to_string().as_str()));
    assert!(prompt.contains(live.display().to_string().as_str()));

    // Excluded inputs — the validator must explicitly note they are not provided.
    assert!(
        prompt.contains("not given the plan"),
        "validator prompt must declare plan is not an input"
    );
    assert!(
        prompt.contains("any git diff"),
        "validator prompt must declare git diff is not an input"
    );
    assert!(
        prompt.contains("test or"),
        "validator prompt must declare test/build output is not an input"
    );
    assert!(
        prompt.contains("per-task review"),
        "validator prompt must declare per-task review verdicts are not inputs"
    );
    assert!(
        prompt.contains("prior validation rounds"),
        "validator prompt must declare prior validation rounds are not inputs"
    );
    // Validator-only paths: no plan/review pointers in the inputs/outputs.
    assert!(
        !prompt.contains("artifacts/plan.md"),
        "validator prompt must not reference artifacts/plan.md as a path"
    );
    assert!(
        !prompt.contains("review_scope.toml"),
        "validator prompt must not reference review scope artifacts"
    );

    // Non-interactive, no mutations.
    assert!(prompt.contains("NON-INTERACTIVE"));
    assert!(prompt.contains("may not mutate the workspace"));
    assert!(prompt.contains("may not write code"));
}

#[test]
fn interactive_live_summary_instruction_requires_immediate_creation() {
    let path = std::path::Path::new("artifacts/live_summary.interactive.txt");
    let prompt = live_summary_instruction_interactive(path);

    assert!(prompt.contains("Immediately create artifacts/live_summary.interactive.txt"));
    assert!(prompt.contains("every 2–3 min"));
    assert!(prompt.contains("Keep this file current until you exit."));
}
