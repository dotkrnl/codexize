//! Tests for [`super::LifecycleOps`]. Live in their own file so the impl
//! module ([`super`]) stays focused on the surface and the test surface
//! can grow without crowding it.
//!
//! All tests use hand-built [`Fsm`] / [`StageCtx`] / [`StageRegistry`]
//! inputs — there is no App, no process spawning, no disk IO.
use super::*;
use crate::lifecycle::fsm::{AgentState, Outcome};
use crate::lifecycle::pending::PendingDecisions;
use crate::lifecycle::phase::Phase;
use crate::lifecycle::spec::{ActiveRun, StageSpec};
use crate::lifecycle::stage::{StageCtx, StageRegistry};
use crate::lifecycle::stage_id::StageId;
use crate::lifecycle::stages::{
    BrainstormStage, CoderStage, DreamingStage, FinalValidationStage, PlanReviewStage,
    PlanningStage, RecoveryPlanReviewStage, RecoveryShardingStage, RecoveryStage,
    RepoStateUpdateStage, ReviewerStage, ShardingStage, SimplificationStage, SpecReviewStage,
};
use std::path::{Path, PathBuf};

/// Build a registry mirroring [`crate::lifecycle::stages::default_registry`]
/// without depending on it directly. This keeps ops tests focused on
/// operator-command behavior rather than default-registry construction.
fn full_registry() -> StageRegistry {
    let mut r = StageRegistry::new();
    r.register(Box::new(BrainstormStage));
    r.register(Box::new(SpecReviewStage));
    r.register(Box::new(PlanningStage));
    r.register(Box::new(PlanReviewStage));
    r.register(Box::new(RepoStateUpdateStage));
    r.register(Box::new(ShardingStage));
    r.register(Box::new(CoderStage));
    r.register(Box::new(ReviewerStage));
    r.register(Box::new(RecoveryStage));
    r.register(Box::new(RecoveryPlanReviewStage));
    r.register(Box::new(RecoveryShardingStage));
    r.register(Box::new(FinalValidationStage));
    r.register(Box::new(SimplificationStage));
    r.register(Box::new(DreamingStage));
    r
}

/// Test-only container for owned data the borrowed [`OpsCtx`] points at.
struct OpsState {
    fsm: Fsm,
    phase: Phase,
    paused_at_phase: Option<Phase>,
    pending: PendingDecisions,
    session_dir: PathBuf,
    prior_runs: Vec<crate::lifecycle::stage::RunHistoryEntry>,
    pending_task_ids: Vec<u32>,
}

impl OpsState {
    fn new(phase: Phase) -> Self {
        Self {
            fsm: Fsm::new(),
            phase,
            paused_at_phase: None,
            pending: PendingDecisions::default(),
            session_dir: PathBuf::from("/tmp/codexize-test-session"),
            prior_runs: Vec::new(),
            pending_task_ids: Vec::new(),
        }
    }
}

fn build_ops_ctx<'a>(state: &'a mut OpsState, registry: &'a StageRegistry) -> OpsCtx<'a> {
    let stage_ctx = StageCtx {
        session_id: "test-session",
        session_dir: state.session_dir.as_path(),
        phase: state.phase,
        prior_runs: state.prior_runs.as_slice(),
        pending_task_ids: state.pending_task_ids.as_slice(),
        yolo: false,
        cheap: false,
        recovery_active: false,
        simplification_requested: false,
        dreaming_accepted: false,
    };
    OpsCtx {
        fsm: &mut state.fsm,
        phase: &mut state.phase,
        paused_at_phase: &mut state.paused_at_phase,
        pending_decisions: &mut state.pending,
        registry,
        stage_ctx,
        now: chrono::Utc::now(),
    }
}

/// Helper to push the FSM into [`AgentState::Running`] for a given stage.
fn drive_running(fsm: &mut Fsm, stage_id: StageId, attempt: u32, run_id: u64) {
    let spec = StageSpec {
        stage_id,
        round: 1,
        task_id: None,
        attempt,
        window_name: format!("{stage_id:?}-{attempt}"),
    };
    fsm.start(spec.clone()).expect("idle accepts start");
    fsm.confirm_running(ActiveRun {
        run_id,
        spec,
        started_at: chrono::Utc::now(),
    })
    .expect("starting accepts confirm_running");
}

// ─────────────────────────── :stop ───────────────────────────

#[test]
fn stop_when_idle_is_noop() {
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::stop(&mut ctx);
    assert!(
        matches!(&outcome, OpOutcome::NoOp(msg) if msg.contains("no agent")),
        "expected NoOp, got {outcome:?}"
    );
    assert!(matches!(state.fsm.view(), AgentState::Idle));
}

#[test]
fn stop_when_running_stages_pending_stop() {
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    drive_running(&mut state.fsm, StageId::Planning, 1, 11);
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::stop(&mut ctx);
    match outcome {
        OpOutcome::Staged(OpAction::PendingStop {
            after,
            cleanup,
            phase_change,
            clear_paused,
            clear_pending,
        }) => {
            assert_eq!(after, AfterStop::GoIdle);
            assert!(cleanup.is_empty());
            assert!(phase_change.is_none());
            assert!(!clear_paused);
            assert!(!clear_pending);
        }
        other => panic!("expected PendingStop, got {other:?}"),
    }
    assert!(matches!(state.fsm.view(), AgentState::Stopping { .. }));
    assert_eq!(state.paused_at_phase, Some(Phase::Plan));
}

#[test]
fn stop_when_stopping_re_requests_with_goidle() {
    // Precedence test: a previous Restart in-flight gets overwritten by
    // a fresh GoIdle from :stop, exercising Fsm::request_stop's
    // "latest non-Cancel wins" rule.
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    drive_running(&mut state.fsm, StageId::Planning, 1, 11);
    state
        .fsm
        .request_stop(AfterStop::Restart {
            spec: StageSpec {
                stage_id: StageId::Planning,
                round: 1,
                task_id: None,
                attempt: 2,
                window_name: "[Planning]".into(),
            },
        })
        .unwrap();
    let mut ctx = build_ops_ctx(&mut state, &registry);

    LifecycleOps::stop(&mut ctx);
    match state.fsm.view() {
        AgentState::Stopping { after, .. } => assert_eq!(after, &AfterStop::GoIdle),
        other => panic!("expected Stopping, got {other:?}"),
    }
}

// ─────────────────────────── :restart ───────────────────────────

#[test]
fn restart_when_idle_is_noop() {
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::restart(&mut ctx);
    assert!(
        matches!(&outcome, OpOutcome::NoOp(msg) if msg.contains("no agent")),
        "expected NoOp, got {outcome:?}"
    );
}

#[test]
fn restart_when_running_stages_pending_stop_with_restart() {
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    state.paused_at_phase = Some(Phase::Plan);
    drive_running(&mut state.fsm, StageId::Planning, 3, 11);
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::restart(&mut ctx);
    match outcome {
        OpOutcome::Staged(OpAction::PendingStop {
            after,
            phase_change,
            clear_paused,
            ..
        }) => {
            assert!(phase_change.is_none());
            assert!(clear_paused);
            match after {
                AfterStop::Restart { spec } => {
                    assert_eq!(spec.stage_id, StageId::Planning);
                    // PlanningStage.build_spec() yields attempt=1 with an
                    // empty prior_runs list, then with_attempt_plus_one
                    // bumps it to 2. (Not 4 — restart asks the stage for
                    // a fresh spec, which is attempt-aware.)
                    assert_eq!(spec.attempt, 2);
                }
                other => panic!("expected Restart, got {other:?}"),
            }
        }
        other => panic!("expected PendingStop, got {other:?}"),
    }
    // paused_at_phase cleared so the scheduler will pick the restart up.
    assert!(state.paused_at_phase.is_none());
}

#[test]
fn restart_when_stage_does_not_support_restart_is_noop() {
    // FinalValidation has `supports_restart() == false`.
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Finalization);
    drive_running(&mut state.fsm, StageId::FinalValidation, 1, 12);
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::restart(&mut ctx);
    assert!(
        matches!(&outcome, OpOutcome::NoOp(msg) if msg.contains("restart")),
        "expected NoOp, got {outcome:?}"
    );
    // FSM untouched.
    assert!(matches!(state.fsm.view(), AgentState::Running { .. }));
}

// ─────────────────────────── :rewind ───────────────────────────

#[test]
fn rewind_to_equal_phase_is_noop() {
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::rewind(&mut ctx, Phase::Plan);
    assert!(matches!(outcome, OpOutcome::NoOp(_)));
}

#[test]
fn rewind_to_later_phase_is_noop() {
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::rewind(&mut ctx, Phase::Implementation(2));
    assert!(matches!(outcome, OpOutcome::NoOp(_)));
}

#[test]
fn rewind_to_earlier_phase_when_idle_returns_immediate_with_cleanup() {
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    state.paused_at_phase = Some(Phase::Plan);
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::rewind(&mut ctx, Phase::Spec);
    match outcome {
        OpOutcome::Staged(OpAction::Immediate {
            phase_change,
            cleanup,
            clear_paused,
            clear_pending,
            start_spec,
        }) => {
            assert_eq!(phase_change, Some(Phase::Spec));
            assert!(clear_paused);
            assert!(clear_pending);
            // Spec → SpecReviewStage is the canonical start.
            let s = start_spec.expect("start_spec for Phase::Spec");
            assert_eq!(s.stage_id, StageId::SpecReview);
            // Plan-phase artifacts must be cleaned (plan.md from Planning,
            // plan-review-*.md from PlanReview, tasks.toml from Sharding).
            let dels: Vec<String> = cleanup
                .delete
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            assert!(
                dels.iter().any(|p| p.ends_with("artifacts/plan.md")),
                "delete must include artifacts/plan.md: {dels:?}"
            );
            assert!(
                dels.iter().any(|p| p.ends_with("artifacts/tasks.toml")),
                "delete must include artifacts/tasks.toml: {dels:?}"
            );
        }
        other => panic!("expected Immediate, got {other:?}"),
    }
}

#[test]
fn rewind_to_earlier_phase_when_running_returns_pending_stop() {
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    drive_running(&mut state.fsm, StageId::Planning, 1, 21);
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::rewind(&mut ctx, Phase::Spec);
    match outcome {
        OpOutcome::Staged(OpAction::PendingStop {
            after,
            phase_change,
            clear_paused,
            clear_pending,
            ..
        }) => {
            assert!(matches!(after, AfterStop::Rewind { target: Phase::Spec, .. }));
            assert_eq!(phase_change, Some(Phase::Spec));
            assert!(clear_paused);
            assert!(clear_pending);
        }
        other => panic!("expected PendingStop, got {other:?}"),
    }
    // The FSM is now Stopping with the rewind plan attached.
    match state.fsm.view() {
        AgentState::Stopping { after, .. } => match after {
            AfterStop::Rewind {
                target,
                clear_pending,
                ..
            } => {
                assert_eq!(*target, Phase::Spec);
                assert!(*clear_pending);
            }
            other => panic!("expected AfterStop::Rewind, got {other:?}"),
        },
        other => panic!("expected Stopping, got {other:?}"),
    }
}

#[test]
fn rewind_cleanup_includes_artifacts_for_all_stages_after_target() {
    // Rewinding to Phase::Idea from Phase::Implementation(2) must clean
    // every artifact produced by stages strictly after Phase::Idea —
    // spec-review outputs, plan, plan-review, sharding outputs, and the
    // per-round implementation directories. (Brainstorm's spec.md is
    // *at* Phase::Idea, not strictly after, so it's intentionally
    // preserved — operators rewinding to Idea retain whatever
    // brainstorm produced; they re-run brainstorm explicitly.)
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Implementation(2));
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::rewind(&mut ctx, Phase::Idea);
    let cleanup = match outcome {
        OpOutcome::Staged(OpAction::Immediate { cleanup, .. }) => cleanup,
        other => panic!("expected Immediate, got {other:?}"),
    };
    let dels: Vec<String> = cleanup
        .delete
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    for needle in [
        "artifacts/spec-review-1.md",
        "artifacts/plan.md",
        "artifacts/plan-review-1.md",
        "artifacts/tasks.toml",
        "rounds/001",
        "rounds/002",
    ] {
        assert!(
            dels.iter().any(|p| p.ends_with(needle)),
            "delete must include {needle}: {dels:?}"
        );
    }
}

#[test]
fn rewind_cleanup_includes_target_stage_backup_restore_for_plan_review() {
    // Rewinding to Phase::Plan restores the round-1 plan.md / spec.md
    // backups that PlanReviewStage takes before it overwrites plan.md.
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Implementation(1));
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::rewind(&mut ctx, Phase::Plan);
    let cleanup = match outcome {
        OpOutcome::Staged(OpAction::Immediate { cleanup, .. }) => cleanup,
        other => panic!("expected Immediate, got {other:?}"),
    };
    let restores: Vec<(String, String)> = cleanup
        .restore_backups
        .iter()
        .map(|(b, d)| {
            (
                b.to_string_lossy().into_owned(),
                d.to_string_lossy().into_owned(),
            )
        })
        .collect();
    assert!(
        restores
            .iter()
            .any(|(b, d)| b.ends_with("plan.pre-review-1.md") && d.ends_with("plan.md")),
        "expected plan.pre-review-1.md → plan.md restore, got {restores:?}"
    );
    assert!(
        restores
            .iter()
            .any(|(b, d)| b.ends_with("spec.pre-review-1.md") && d.ends_with("spec.md")),
        "expected spec.pre-review-1.md → spec.md restore, got {restores:?}"
    );
}

// ─────────────────────────── :cancel ───────────────────────────

#[test]
fn cancel_when_idle_sets_phase_immediately() {
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::cancel(&mut ctx);
    match outcome {
        OpOutcome::Staged(OpAction::Immediate {
            phase_change,
            cleanup,
            start_spec,
            clear_pending,
            ..
        }) => {
            assert_eq!(phase_change, Some(Phase::Cancelled));
            assert!(cleanup.is_empty());
            assert!(start_spec.is_none());
            assert!(clear_pending);
        }
        other => panic!("expected Immediate, got {other:?}"),
    }
}

#[test]
fn cancel_when_running_stages_pending_stop_with_cancel() {
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    drive_running(&mut state.fsm, StageId::Planning, 1, 31);
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::cancel(&mut ctx);
    match outcome {
        OpOutcome::Staged(OpAction::PendingStop {
            after,
            phase_change,
            clear_pending,
            ..
        }) => {
            assert_eq!(after, AfterStop::Cancel);
            assert_eq!(phase_change, Some(Phase::Cancelled));
            assert!(clear_pending);
        }
        other => panic!("expected PendingStop, got {other:?}"),
    }
}

#[test]
fn cancel_beats_restart_in_stop_precedence() {
    // Start a Restart, then Cancel — the FSM's Cancel-sticks rule must
    // hold, and the AfterStop carried by Stopping must be Cancel.
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    drive_running(&mut state.fsm, StageId::Planning, 1, 41);

    {
        let mut ctx = build_ops_ctx(&mut state, &registry);
        LifecycleOps::restart(&mut ctx);
    }
    {
        let mut ctx = build_ops_ctx(&mut state, &registry);
        LifecycleOps::cancel(&mut ctx);
    }
    match state.fsm.view() {
        AgentState::Stopping { after, .. } => assert_eq!(after, &AfterStop::Cancel),
        other => panic!("expected Stopping, got {other:?}"),
    }
}

#[test]
fn latest_replaces_in_stop_precedence() {
    // Start a Restart, then a fresh Restart (different attempt). The
    // FSM's "latest non-Cancel wins" rule means the second Restart's
    // spec is what Stopping carries.
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    drive_running(&mut state.fsm, StageId::Planning, 5, 42);

    {
        let mut ctx = build_ops_ctx(&mut state, &registry);
        LifecycleOps::restart(&mut ctx);
    }
    // Bump prior_runs so the next restart sees attempt 5 already there.
    state
        .prior_runs
        .push(crate::lifecycle::stage::RunHistoryEntry {
            stage_id: StageId::Planning,
            task_id: None,
            round: 1,
            attempt: 5,
            outcome: Some(Outcome::Failed("forced".into())),
        });
    {
        let mut ctx = build_ops_ctx(&mut state, &registry);
        LifecycleOps::restart(&mut ctx);
    }
    match state.fsm.view() {
        AgentState::Stopping {
            after: AfterStop::Restart { spec },
            ..
        } => {
            // PlanningStage.build_spec() with prior_runs containing attempt
            // 5 yields a fresh attempt 6; with_attempt_plus_one bumps to 7.
            assert_eq!(spec.attempt, 7);
        }
        other => panic!("expected Stopping with Restart, got {other:?}"),
    }
}

// ─────────────────── resolution_to_action ───────────────────

#[test]
fn resolution_to_action_lifts_rewind_carry_back() {
    // The app's confirm-dead handler uses resolution_to_action to turn a
    // StopResolution into the same Immediate plan the idle path produced.
    // This test pins the mapping.
    let cleanup = CleanupPlan {
        delete: vec![PathBuf::from("/tmp/x")],
        restore_backups: vec![(PathBuf::from("/tmp/b"), PathBuf::from("/tmp/d"))],
    };
    let next_spec = StageSpec {
        stage_id: StageId::Planning,
        round: 1,
        task_id: None,
        attempt: 2,
        window_name: "[Planning]".into(),
    };
    let resolution = StopResolution {
        outcome: Outcome::Cancelled {
            by: crate::lifecycle::fsm::CancelledBy::Operator,
            reason: "rewind".into(),
        },
        next: AfterStop::Rewind {
            target: Phase::Plan,
            spec: Some(next_spec.clone()),
            cleanup: cleanup.clone(),
            clear_pending: true,
        },
        finalized: crate::lifecycle::fsm::FinalizedRun {
            run: ActiveRun {
                run_id: 1,
                spec: next_spec.clone(),
                started_at: chrono::Utc::now(),
            },
            outcome: Outcome::Cancelled {
                by: crate::lifecycle::fsm::CancelledBy::Operator,
                reason: "rewind".into(),
            },
            ended_at: chrono::Utc::now(),
        },
    };
    match resolution_to_action(resolution) {
        OpAction::Immediate {
            phase_change,
            cleanup: c,
            clear_paused,
            clear_pending,
            start_spec,
        } => {
            assert_eq!(phase_change, Some(Phase::Plan));
            assert_eq!(c, cleanup);
            assert!(clear_paused);
            assert!(clear_pending);
            assert_eq!(start_spec, Some(next_spec));
        }
        other => panic!("expected Immediate, got {other:?}"),
    }
}

#[test]
fn cleanup_paths_are_absolute_under_session_dir() {
    // Spot-check that build_rewind_cleanup joins session_dir to relative
    // stage paths.
    let registry = full_registry();
    let mut state = OpsState::new(Phase::Plan);
    let session_dir = state.session_dir.clone();
    let mut ctx = build_ops_ctx(&mut state, &registry);

    let outcome = LifecycleOps::rewind(&mut ctx, Phase::Idea);
    let cleanup = match outcome {
        OpOutcome::Staged(OpAction::Immediate { cleanup, .. }) => cleanup,
        other => panic!("expected Immediate, got {other:?}"),
    };
    for p in &cleanup.delete {
        assert!(
            p.starts_with(&session_dir),
            "delete path {p:?} not under {session_dir:?}"
        );
    }
    // ensure Path import is used so clippy keeps it
    let _ = Path::new("/tmp");
}
