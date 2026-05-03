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

#[test]
fn brainstorm_prompts_drop_skill_invocation_and_carry_no_skill_clause() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("brainstorm-no-skill");
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let spec_path = artifacts.join("spec.md");
        let summary_path = artifacts.join("session_summary.toml");
        let live_summary = artifacts.join("live_summary.txt");

        for yolo in [false, true] {
            let prompt = brainstorm_prompt(
                "verify the brainstorm prompt removed the legacy skill plumbing",
                &spec_path.display().to_string(),
                &summary_path.display().to_string(),
                &live_summary.display().to_string(),
                None,
                yolo,
            );
            assert!(
                !prompt.contains("Invoke your brainstorming skill"),
                "old skill-invocation line must be gone"
            );
            assert!(
                !prompt.contains("Use that installed package for brainstorming."),
                "old package-path plumbing must be gone"
            );
            assert!(
                prompt.contains("Do not invoke any skill"),
                "embedded prompt must explicitly forbid harness skills"
            );
            assert!(
                prompt.contains("brainstorming\nskill, writing-plans skill, or any other"),
                "no-skill clause must enumerate the affected skills"
            );
            assert!(
                prompt.contains("verify the brainstorm prompt removed the legacy skill plumbing"),
                "idea text must still be embedded verbatim"
            );
        }
    });
}

#[test]
fn planning_prompts_drop_skill_invocation_and_carry_no_skill_clause() {
    let spec = std::path::Path::new("artifacts/spec.md");
    let plan = std::path::Path::new("artifacts/plan.md");
    let live = std::path::Path::new("artifacts/live_summary.planning.txt");
    let reviews: Vec<std::path::PathBuf> = vec![
        std::path::PathBuf::from("artifacts/spec-review-1.md"),
        std::path::PathBuf::from("artifacts/spec-review-2.md"),
    ];

    for yolo in [false, true] {
        let prompt = planning_prompt(spec, &reviews, plan, live, yolo);
        assert!(
            !prompt.contains("Invoke your superpowers:writing-plans skill"),
            "old skill-invocation line must be gone"
        );
        assert!(
            prompt.contains("Do not invoke any skill"),
            "planner prompt must carry the no-skill clause"
        );
        assert!(
            prompt.contains("writing-plans skill, or any other"),
            "no-skill clause must enumerate writing-plans"
        );
        assert!(prompt.contains("artifacts/spec-review-1.md"));
        assert!(prompt.contains("artifacts/spec-review-2.md"));
    }
}

#[test]
fn simplifier_prompt_describes_behavior_preserving_pass_with_required_outputs() {
    use std::path::Path;
    let session_dir = std::path::Path::new("/tmp/simplifier-prompt-fixture");
    let review_scope = Path::new("/tmp/simplifier-prompt-fixture/rounds/001/review_scope.toml");
    let simplification = Path::new("/tmp/simplifier-prompt-fixture/rounds/001/simplification.toml");
    let live = Path::new("/tmp/simplifier-prompt-fixture/artifacts/live_summary.simplifier.txt");

    let prompt = simplifier_prompt(session_dir, review_scope, simplification, live);

    assert!(prompt.contains("preserve exact functionality"));
    assert!(prompt.contains("`refactor:`"));
    assert!(prompt.contains("`style:`"));
    assert!(prompt.contains("Do not invoke any skill"));
    assert!(prompt.contains("base_sha..HEAD"));
    assert!(prompt.contains(review_scope.display().to_string().as_str()));
    assert!(prompt.contains(simplification.display().to_string().as_str()));
    // The verdict TOML contract must list every status variant.
    assert!(prompt.contains("\"simplified\" | \"no_changes\" | \"skipped\""));
    // No behavior-changing or out-of-scope rewrites.
    assert!(prompt.contains("No API changes"));
    assert!(prompt.contains("No dependency upgrades"));
    // Ends with the live-summary instruction.
    assert!(prompt.contains("Immediately create"));
    assert!(prompt.contains("Keep this file current until you exit."));
}

/// Every `.md` template shipped under `src/app/prompts/` must have at least
/// one matching `include_str!` call site in `src/app/prompts.rs`. Catches
/// orphaned templates (renamed or never wired up) before they ship.
#[test]
fn every_prompt_template_is_referenced_by_a_call_site() {
    let templates_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/prompts");
    let prompts_rs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/prompts.rs");
    let prompts_source =
        std::fs::read_to_string(&prompts_rs).expect("read src/app/prompts.rs source");

    let mut orphans = Vec::new();
    for entry in std::fs::read_dir(&templates_dir).expect("read templates dir") {
        let entry = entry.expect("template dir entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap();
        let needle = format!("include_str!(\"prompts/{name}\")");
        if !prompts_source.contains(&needle) {
            orphans.push(name.to_string());
        }
    }
    assert!(
        orphans.is_empty(),
        "unreferenced templates in src/app/prompts/: {:?}",
        orphans
    );
}

/// `{name}` placeholders in any of the shipped templates must all resolve to
/// a binding when rendered through their public prompt-builder. The unbound
/// case is a `panic!` from `prompt_render::render` — by exercising every
/// builder once with realistic-shape inputs we surface template/Rust drift
/// before it lands in a live run.
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
                None,
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

/// Literal `{` and `}` characters inside templates (used by TOML/Rust
/// snippets in the prompt body) survive rendering as single braces — the
/// `{{`/`}}` escape is handled by the renderer, not delegated to format!().
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

// ─────────────────────────────────────────────────────────────────────────────
// Full rendered-output snapshot tests for every prompt builder.
//
// These guard against accidental drift in template files or prompt-builder
// glue: any byte-level change to a rendered prompt is caught here, not just
// the substring assertions above. Brainstorm, planner, and simplifier prompts
// changed by design in this iteration; their snapshots must be reviewed when
// they update. Every other prompt is expected to stay stable; a snapshot
// mismatch on those is a real prompt regression.
//
// Regenerate with `UPDATE_PROMPT_SNAPSHOTS=1 cargo test app::tests_prompts`,
// then review the diff and commit the updated fixture.
// ─────────────────────────────────────────────────────────────────────────────

fn snapshot_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/app/prompt_snapshots")
        .join(format!("{name}.txt"))
}

fn assert_prompt_snapshot(name: &str, actual: &str) {
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

#[test]
fn prompt_snapshots_match_fixtures() {
    use std::path::{Path, PathBuf};
    with_temp_root_and_cwd(|_root| {
        // Use stable, *relative* path strings so every snapshot is
        // byte-deterministic across runs. The test cwd is a tempdir, so
        // these resolve to a per-run sandbox; the rendered prompt only
        // embeds the relative-path text, which is identical across runs.
        let session_dir = PathBuf::from("fixture/session");
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

        // Live summary instructions (the smallest templates).
        assert_prompt_snapshot(
            "live_summary",
            &live_summary_instruction(Path::new("artifacts/live_summary.txt")),
        );
        assert_prompt_snapshot(
            "live_summary_interactive",
            &live_summary_instruction_interactive(Path::new(
                "artifacts/live_summary.interactive.txt",
            )),
        );

        // Spec review.
        assert_prompt_snapshot(
            "spec_review",
            &spec_review_prompt(
                &spec.display().to_string(),
                &spec_review_out.display().to_string(),
                &live.display().to_string(),
            ),
        );

        // Plan review at round 1 (no prior reviews) and round 3 (has prior
        // reviews to embed). Both branches go through the same template.
        assert_prompt_snapshot(
            "plan_review_round1",
            &plan_review_prompt(
                &spec.display().to_string(),
                &plan.display().to_string(),
                &plan_review_r1_out.display().to_string(),
                1,
                &live.display().to_string(),
            ),
        );
        assert_prompt_snapshot(
            "plan_review_round3",
            &plan_review_prompt(
                &spec.display().to_string(),
                &plan.display().to_string(),
                &plan_review_r3_out.display().to_string(),
                3,
                &live.display().to_string(),
            ),
        );

        // Brainstorm: yolo and interactive. Both intentionally changed in
        // this iteration to embed the workflow + no-skill clause.
        let idea = "fictional idea text used only to pin the snapshot";
        assert_prompt_snapshot(
            "brainstorm_interactive",
            &brainstorm_prompt(
                idea,
                &spec.display().to_string(),
                &summary.display().to_string(),
                &live.display().to_string(),
                None,
                false,
            ),
        );
        assert_prompt_snapshot(
            "brainstorm_yolo",
            &brainstorm_prompt(
                idea,
                &spec.display().to_string(),
                &summary.display().to_string(),
                &live.display().to_string(),
                None,
                true,
            ),
        );

        // Planning: yolo and interactive, both with two prior spec reviews
        // to exercise the reviews block.
        let spec_reviews = vec![
            PathBuf::from("artifacts/spec-review-1.md"),
            PathBuf::from("artifacts/spec-review-2.md"),
        ];
        assert_prompt_snapshot(
            "planning_interactive",
            &planning_prompt(&spec, &spec_reviews, &plan, &live, false),
        );
        assert_prompt_snapshot(
            "planning_yolo",
            &planning_prompt(&spec, &spec_reviews, &plan, &live, true),
        );

        // Sharding.
        assert_prompt_snapshot(
            "sharding",
            &sharding_prompt(&spec, &plan, &tasks_path, &live),
        );

        // Final validation.
        assert_prompt_snapshot(
            "final_validation",
            &final_validation_prompt(
                "fictional idea body — pinned for snapshot",
                "# Spec\n\n## User-stated requirements (authoritative)\n- pinned\n\n## Out of scope\n- pinned\n",
                &final_verdict_r3,
                &live,
            ),
        );

        // Recovery (interactive + non-interactive).
        assert_prompt_snapshot(
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
        assert_prompt_snapshot(
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
        assert_prompt_snapshot(
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
        assert_prompt_snapshot(
            "recovery_sharding",
            &recovery_sharding_prompt(&spec, &plan, &live, &tasks_path, &[1, 2], 5),
        );

        // Coder: round 1 with no prior review and no carryover, plus round 3
        // with carryover and resume to exercise every conditional block.
        assert_prompt_snapshot(
            "coder_round1",
            &coder_prompt(&session_dir, 7, 1, &task_file_r1, &live, false, &[]),
        );
        // For round 3 the prior review exists, so the prev_review block
        // renders. Touch the file so `coder_prompt` sees it.
        let prev_review_path = session_dir.join("rounds/002/review.toml");
        std::fs::create_dir_all(prev_review_path.parent().unwrap()).unwrap();
        std::fs::write(&prev_review_path, "status = \"refine\"\n").unwrap();
        assert_prompt_snapshot(
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

        // Reviewer: round 1 (no prior reviews, no coder summary path), and
        // round 3 with both — both branches of the optional blocks.
        assert_prompt_snapshot(
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
        assert_prompt_snapshot(
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

        // Simplifier — new prompt; intentionally has its own snapshot from
        // day one.
        assert_prompt_snapshot(
            "simplifier",
            &simplifier_prompt(&session_dir, &review_scope_r1, &simplification, &live),
        );
    });
}
