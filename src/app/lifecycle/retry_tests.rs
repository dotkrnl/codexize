//! Tests for the 5b operator-driven rewind paths.
//!
//! These exercise the App-level wiring of `LifecycleOps::rewind` through
//! `go_back` / `retry_selected_target`. The slim-phase translator helpers
//! and `LifecycleOps` itself have their own unit tests in
//! `src/lifecycle/`; this suite focuses on what 5b's cutover does *with*
//! those primitives: cleanup is applied to disk, the legacy
//! `state.current_phase` lands on the right variant, and the special-case
//! state mutators (`reset_builder_after_rewind`, skip-to-impl branch,
//! pending-decision short-circuit) still fire.
use crate::app::test_support::{mk_app, with_temp_root};
use crate::lifecycle::{GitGuardData, PendingDecisions};
use crate::state::{Phase, SessionState};

fn fresh_state(phase: Phase) -> SessionState {
    let mut state = SessionState::new("20260513-150000-000000001".to_string());
    state.current_phase = phase;
    state
}

#[test]
fn go_back_from_plan_rewinds_to_spec_and_deletes_plan_artifact() {
    with_temp_root(|| {
        let state = fresh_state(Phase::PlanningRunning);
        state.save().unwrap();
        let mut app = mk_app(state);
        // Seed plan.md so we can assert the cleanup pass removes it.
        let session_dir = app.session_dir();
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        let plan_md = artifacts.join("plan.md");
        std::fs::write(&plan_md, "seed plan").unwrap();
        assert!(plan_md.exists());

        // Drive go_back. With no active agent the path runs the Immediate
        // op; the lane-gate accepts isolated-session phases.
        app.go_back();

        // Plan.md gone via PlanningStage::artifact_paths(1).
        assert!(!plan_md.exists(), "plan.md should be removed by rewind cleanup");
        // Legacy phase landed on the running variant for Spec; the slim
        // mirror keeps up.
        assert_eq!(app.state.current_phase, Phase::SpecReviewRunning);
        assert_eq!(app.slim_phase, crate::lifecycle::Phase::Spec);
    });
}

#[test]
fn go_back_from_implementation_one_with_skip_to_impl_rewinds_to_idea() {
    with_temp_root(|| {
        let mut state = fresh_state(Phase::ImplementationRound(1));
        state.skip_to_impl_rationale = Some("operator picked the shortcut".to_string());
        state.save().unwrap();
        let mut app = mk_app(state);

        app.go_back();

        // Skip-to-impl branch overrides slim_phase.previous() (Plan) to
        // land on Idea instead, since spec/planning never ran.
        assert_eq!(app.slim_phase, crate::lifecycle::Phase::Idea);
        assert_eq!(app.state.current_phase, Phase::IdeaInput);
    });
}

#[test]
fn go_back_from_implementation_one_without_skip_to_impl_resets_builder_and_rewinds_to_plan() {
    with_temp_root(|| {
        let mut state = fresh_state(Phase::ImplementationRound(1));
        state.skip_to_impl_rationale = None;
        // Seed a pipeline item so `reset_builder_after_rewind` has something
        // to clear — its post-state mutates the builder.
        state.builder.task_titles.insert(1, "task title".to_string());
        state.builder.push_pipeline_item(crate::state::PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: Some(1),
            status: crate::state::PipelineItemStatus::Pending,
            title: Some("task title".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 1,
        });
        state.save().unwrap();
        let mut app = mk_app(state);
        assert!(!app.state.builder.pipeline_items.is_empty());

        app.go_back();

        assert_eq!(app.slim_phase, crate::lifecycle::Phase::Plan);
        assert!(
            app.state.builder.pipeline_items.is_empty(),
            "reset_builder_after_rewind should have cleared the pipeline"
        );
    });
}

#[test]
fn go_back_is_noop_while_a_pending_decision_blocks() {
    with_temp_root(|| {
        let state = fresh_state(Phase::PlanningRunning);
        state.save().unwrap();
        let mut app = mk_app(state);
        // Open a git-guard pending decision on the slim surface; go_back
        // should refuse to rewind until the operator resolves it.
        app.pending_decisions = PendingDecisions {
            git_guard: Some(GitGuardData),
            ..Default::default()
        };
        let original_phase = app.state.current_phase;
        let original_slim = app.slim_phase;

        app.go_back();

        assert_eq!(app.state.current_phase, original_phase);
        assert_eq!(app.slim_phase, original_slim);
    });
}

#[test]
fn retry_selected_target_for_task_row_rewinds_to_implementation_round() {
    with_temp_root(|| {
        // Park the App past the round where task 7 last ran, so the
        // operator's "retry task 7" maps to a backwards rewind that
        // LifecycleOps::rewind accepts.
        let mut state = fresh_state(Phase::ImplementationRound(7));
        state.builder.task_titles.insert(7, "task 7".to_string());
        let started_at = chrono::Utc::now();
        state.agent_runs.push(crate::state::RunRecord {
            id: 100,
            stage: "coder".to_string(),
            task_id: Some(7),
            round: 5,
            attempt: 1,
            model: "model".to_string(),
            subscription_label: "sub".to_string(),
            window_name: "[Round 5 Coder] 7".to_string(),
            started_at,
            ended_at: Some(started_at),
            status: crate::state::RunStatus::Failed,
            error: Some("boom".to_string()),
            effort: crate::adapters::EffortLevel::Normal,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            effort_eligible: false,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        });
        state.save().unwrap();
        let mut app = mk_app(state);

        // Drive the slim-target lookup directly: we're testing the
        // target-derivation contract, not the tree-row selection path
        // (already covered by the tree tests).
        let target = crate::lifecycle::slim_phase_for_task_retry(7, &app.state);
        assert_eq!(target, crate::lifecycle::Phase::Implementation(5));
        // And confirm the App-level wrapper applies it. The
        // run_lifecycle_op + LifecycleOps::rewind composition is what
        // `retry_selected_target` dispatches to.
        app.run_lifecycle_op("retry", |ctx| {
            crate::lifecycle::LifecycleOps::rewind(ctx, target)
        });
        assert_eq!(app.slim_phase, crate::lifecycle::Phase::Implementation(5));
    });
}
