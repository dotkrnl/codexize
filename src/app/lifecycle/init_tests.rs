//! Tests for resume + crash recovery wiring in `App::new`.
//!
//! Step 6 of the lifecycle redesign: any `RunRecord` left as `Running` on
//! disk is the residue of a prior TUI invocation that exited mid-run. On
//! resume we backfill it as `Failed`, the FSM starts `Idle`, and the legacy
//! `current_run_id`/`run_launched` mirrors are forced to "no live run"
//! regardless of what's on disk.
use crate::adapters::EffortLevel;
use crate::app::App;
use crate::app::test_support::with_temp_root;
use crate::lifecycle::{
    AgentState, DreamingData, GitGuardData, PendingDecisions, Phase as SlimPhase, PlanApprovalData,
    SkipToImplData, SpecApprovalData,
};
use crate::state::{LaunchModes, Phase as LegacyPhase, RunRecord, RunStatus, SessionState};
use std::sync::Arc;

fn running_run_record(id: u64) -> RunRecord {
    RunRecord {
        id,
        stage: "brainstorm".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "claude-opus-4.7".to_string(),
        subscription_label: "anthropic".to_string(),
        window_name: format!("[Brainstorm-{id}]"),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        effort_eligible: false,
        modes: LaunchModes::default(),
        hostname: SessionState::capture_hostname(),
        mount_device_id: SessionState::capture_mount_device_id(),
        section_path: None,
    }
}

fn build_app(state: SessionState) -> App {
    let config = Arc::new(crate::data::config::Config::baked_defaults());
    App::new_with_startup_origin_config_without_model_refresh(
        state,
        crate::app::AppStartupOrigin::Default,
        config,
    )
}

#[test]
fn resume_backfills_orphaned_running_run() {
    with_temp_root(|| {
        let mut state = SessionState::new("20260513-150000-000000001".to_string());
        state.current_phase = LegacyPhase::BrainstormRunning;
        state.agent_runs.push(running_run_record(1));
        state.save().unwrap();
        let session_id = state.session_id.clone();

        // Drop the in-memory `state`; reload through the App constructor.
        drop(state);
        let loaded = SessionState::load(&session_id).expect("reload session");
        let app = build_app(loaded);

        let run = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 1)
            .expect("run 1 must survive");
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(
            run.error.as_deref(),
            Some("aborted: TUI exited while running")
        );
        assert!(run.ended_at.is_some());
    });
}

#[test]
fn resume_initializes_fsm_to_idle() {
    with_temp_root(|| {
        let mut state = SessionState::new("20260513-150000-000000002".to_string());
        state.current_phase = LegacyPhase::BrainstormRunning;
        state.agent_runs.push(running_run_record(1));
        state.save().unwrap();
        let session_id = state.session_id.clone();

        drop(state);
        let loaded = SessionState::load(&session_id).expect("reload session");
        let app = build_app(loaded);

        assert_eq!(app.fsm.view(), &AgentState::Idle);
    });
}

#[test]
fn resume_clears_current_run_id_and_run_launched() {
    with_temp_root(|| {
        let mut state = SessionState::new("20260513-150000-000000003".to_string());
        state.current_phase = LegacyPhase::BrainstormRunning;
        state.agent_runs.push(running_run_record(1));
        state.save().unwrap();
        let session_id = state.session_id.clone();

        drop(state);
        let loaded = SessionState::load(&session_id).expect("reload session");
        let app = build_app(loaded);

        assert_eq!(app.current_run_id, None);
        assert!(!app.run_launched);
    });
}

#[test]
fn session_round_trips_paused_at_phase_and_pending_decisions() {
    with_temp_root(|| {
        let mut state = SessionState::new("20260513-150000-000000004".to_string());
        state.current_phase = LegacyPhase::PlanReviewPaused;
        state.paused_at_phase = Some(SlimPhase::Plan);
        state.pending_decisions = PendingDecisions {
            git_guard: Some(GitGuardData {}),
            spec_approval: Some(SpecApprovalData {}),
            plan_approval: Some(PlanApprovalData {}),
            skip_to_impl: Some(SkipToImplData {}),
            dreaming: Some(DreamingData {}),
        };
        state.save().unwrap();
        let session_id = state.session_id.clone();
        let expected_paused = state.paused_at_phase;
        let expected_pending = state.pending_decisions.clone();

        drop(state);
        let reloaded = SessionState::load(&session_id).expect("reload session");
        assert_eq!(reloaded.paused_at_phase, expected_paused);
        assert_eq!(reloaded.pending_decisions, expected_pending);
    });
}
