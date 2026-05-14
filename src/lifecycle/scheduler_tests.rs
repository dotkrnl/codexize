//! Registry + scheduler integration tests.
//!
//! Lives alongside the default registry (instead of inside `scheduler.rs`)
//! because every test here exercises the registry-scheduler pair, not the
//! scheduler in isolation.
use crate::lifecycle::fsm::{AfterStop, AgentState, Outcome};
use crate::lifecycle::pending::{GitGuardData, PendingDecisions};
use crate::lifecycle::phase::Phase;
use crate::lifecycle::scheduler::{BlockReason, Scheduler, TickInput, TickOutcome};
use crate::lifecycle::spec::{ActiveRun, StageSpec};
use crate::lifecycle::stage::{RunHistoryEntry, StageCtx, StageRegistry};
use crate::lifecycle::stage_id::StageId;
use crate::lifecycle::stages::default_registry;
use std::path::Path;

fn ctx<'a>(phase: Phase, prior: &'a [RunHistoryEntry], pending: &'a [u32]) -> StageCtx<'a> {
    StageCtx {
        session_id: "s",
        session_dir: Path::new("/tmp"),
        phase,
        prior_runs: prior,
        pending_task_ids: pending,
        yolo: false,
        cheap: false,
        recovery_active: false,
        simplification_requested: false,
        dreaming_accepted: false,
    }
}

fn done_run(stage: StageId, task: Option<u32>, round: u32) -> RunHistoryEntry {
    RunHistoryEntry {
        stage_id: stage,
        task_id: task,
        round,
        attempt: 1,
        outcome: Some(Outcome::Done),
    }
}

fn baseline_input<'a>(
    agent: &'a AgentState,
    phase: Phase,
    pending: &'a PendingDecisions,
    ctx: StageCtx<'a>,
) -> TickInput<'a> {
    TickInput {
        agent,
        phase,
        paused_at_phase: None,
        pending_decisions: pending,
        project_lane_allows: true,
        ctx,
    }
}

// --- StageRegistry: next_stage_for_phase ----------------------------------

#[test]
fn empty_registry_returns_none_for_every_phase() {
    let reg = StageRegistry::new();
    for phase in [
        Phase::Idea,
        Phase::Spec,
        Phase::Plan,
        Phase::Implementation(1),
        Phase::Review(1),
        Phase::Finalization,
        Phase::Done,
        Phase::Cancelled,
    ] {
        assert_eq!(reg.next_stage_for_phase(phase, &ctx(phase, &[], &[])), None);
    }
}

#[test]
fn default_registry_returns_brainstorm_at_idea() {
    let reg = default_registry();
    assert_eq!(
        reg.next_stage_for_phase(Phase::Idea, &ctx(Phase::Idea, &[], &[])),
        Some(StageId::Brainstorm)
    );
}

#[test]
fn default_registry_returns_none_at_idea_when_brainstorm_done() {
    let reg = default_registry();
    let prior = [done_run(StageId::Brainstorm, None, 1)];
    // Phase Idea after Brainstorm succeeded: the registry honestly reports
    // None because no stage at Phase::Idea has more work. The FSM would
    // have already advanced the phase before reaching this state; the
    // test pins down the registry-level contract.
    assert_eq!(
        reg.next_stage_for_phase(Phase::Idea, &ctx(Phase::Idea, &prior, &[])),
        None
    );
}

#[test]
fn default_registry_plan_phase_walks_candidates_in_order() {
    let reg = default_registry();
    // Nothing done yet → Planning wins.
    assert_eq!(
        reg.next_stage_for_phase(Phase::Plan, &ctx(Phase::Plan, &[], &[])),
        Some(StageId::Planning)
    );
    // After Planning Done → PlanReview wins.
    let after_planning = [done_run(StageId::Planning, None, 1)];
    assert_eq!(
        reg.next_stage_for_phase(Phase::Plan, &ctx(Phase::Plan, &after_planning, &[])),
        Some(StageId::PlanReview)
    );
    // PlanReview is multi-round-per-phase (advances the round counter on
    // every successful run), so a single round-1 Done doesn't satisfy it —
    // it queues round 2. Note: the round-loop in the FSM, not the
    // registry, decides when to stop launching plan-review rounds (the
    // operator's plan-approval modal lives in PendingDecisions). The
    // following assertion documents that contract.
    assert_eq!(
        reg.next_stage_for_phase(
            Phase::Plan,
            &ctx(
                Phase::Plan,
                &[
                    done_run(StageId::Planning, None, 1),
                    done_run(StageId::PlanReview, None, 1),
                ],
                &[]
            )
        ),
        Some(StageId::PlanReview)
    );
}

#[test]
fn default_registry_implementation_returns_coder_when_tasks_pending() {
    let reg = default_registry();
    let pending = [1u32, 2];
    assert_eq!(
        reg.next_stage_for_phase(
            Phase::Implementation(1),
            &ctx(Phase::Implementation(1), &[], &pending)
        ),
        Some(StageId::Coder)
    );
}

#[test]
fn default_registry_implementation_skips_recovery_when_gate_closed() {
    let reg = default_registry();
    let pending = [1u32];
    let prior = [done_run(StageId::Coder, Some(1), 1)];
    // Coder Done, no recovery gate → no stage to schedule at this phase.
    assert_eq!(
        reg.next_stage_for_phase(
            Phase::Implementation(1),
            &ctx(Phase::Implementation(1), &prior, &pending)
        ),
        None
    );
}

#[test]
fn default_registry_implementation_selects_recovery_when_gate_open() {
    let reg = default_registry();
    let pending = [1u32];
    let prior = [done_run(StageId::Coder, Some(1), 1)];
    let mut c = ctx(Phase::Implementation(1), &prior, &pending);
    c.recovery_active = true;
    assert_eq!(
        reg.next_stage_for_phase(Phase::Implementation(1), &c),
        Some(StageId::Recovery)
    );
}

#[test]
fn default_registry_review_phase_simplification_gated() {
    let reg = default_registry();
    let pending = [1u32];
    // Reviewer not done → Reviewer first.
    assert_eq!(
        reg.next_stage_for_phase(Phase::Review(1), &ctx(Phase::Review(1), &[], &pending)),
        Some(StageId::Reviewer)
    );
    // Reviewer done, no simplification request → nothing pending.
    let after_reviewer = [done_run(StageId::Reviewer, Some(1), 1)];
    assert_eq!(
        reg.next_stage_for_phase(
            Phase::Review(1),
            &ctx(Phase::Review(1), &after_reviewer, &pending)
        ),
        None
    );
    // Reviewer done + simplification requested → Simplification.
    let mut c = ctx(Phase::Review(1), &after_reviewer, &pending);
    c.simplification_requested = true;
    assert_eq!(
        reg.next_stage_for_phase(Phase::Review(1), &c),
        Some(StageId::Simplification)
    );
}

#[test]
fn default_registry_finalization_dreaming_gated() {
    let reg = default_registry();
    // FinalValidation first.
    assert_eq!(
        reg.next_stage_for_phase(Phase::Finalization, &ctx(Phase::Finalization, &[], &[])),
        Some(StageId::FinalValidation)
    );
    let after_fv = [done_run(StageId::FinalValidation, None, 1)];
    // Without dreaming_accepted → None.
    assert_eq!(
        reg.next_stage_for_phase(
            Phase::Finalization,
            &ctx(Phase::Finalization, &after_fv, &[])
        ),
        None
    );
    // With dreaming_accepted → Dreaming.
    let mut c = ctx(Phase::Finalization, &after_fv, &[]);
    c.dreaming_accepted = true;
    assert_eq!(
        reg.next_stage_for_phase(Phase::Finalization, &c),
        Some(StageId::Dreaming)
    );
}

#[test]
fn default_registry_terminal_phases_return_none() {
    let reg = default_registry();
    for phase in [Phase::Done, Phase::Cancelled] {
        assert_eq!(
            reg.next_stage_for_phase(phase, &ctx(phase, &[], &[])),
            None
        );
    }
}

// --- StageRegistry: stages_after ------------------------------------------

#[test]
fn stages_after_plan_includes_every_implementation_review_and_finalization_stage() {
    let reg = default_registry();
    let got = reg.stages_after(Phase::Plan);
    // Set membership: every stage whose canonical phase_when_running > Plan.
    let expected: std::collections::HashSet<StageId> = [
        StageId::Coder,
        StageId::Recovery,
        StageId::RecoveryPlanReview,
        StageId::RecoverySharding,
        StageId::Reviewer,
        StageId::Simplification,
        StageId::FinalValidation,
        StageId::Dreaming,
    ]
    .into_iter()
    .collect();
    let got_set: std::collections::HashSet<_> = got.iter().copied().collect();
    assert_eq!(got_set, expected, "stages_after(Plan) membership");

    // Ordering invariant: phases must be non-increasing through the result.
    // Use each stage's `phase_when_running()` for the comparison; equal
    // phases (e.g. Reviewer / Simplification) may appear in either order
    // since the contract doesn't pin the tie-break.
    let phases: Vec<Phase> = got
        .iter()
        .map(|id| reg.get(*id).expect("registered").phase_when_running())
        .collect();
    for window in phases.windows(2) {
        let a = window[0];
        let b = window[1];
        // a should be >= b; partial_cmp must not be Less.
        assert!(
            !matches!(a.partial_cmp(&b), Some(std::cmp::Ordering::Less)),
            "stages_after must be sorted descending: {a:?} appeared before {b:?}"
        );
    }
}

#[test]
fn stages_after_finalization_is_empty() {
    let reg = default_registry();
    assert!(reg.stages_after(Phase::Finalization).is_empty());
}

#[test]
fn stages_after_cancelled_is_empty() {
    // Cancelled is incomparable with every other phase → no hits.
    let reg = default_registry();
    assert!(reg.stages_after(Phase::Cancelled).is_empty());
}

// --- Scheduler::plan ------------------------------------------------------

fn active_run() -> ActiveRun {
    ActiveRun {
        run_id: 1,
        spec: StageSpec {
            stage_id: StageId::Brainstorm,
            round: 1,
            task_id: None,
            attempt: 1,
            window_name: "[Brainstorm]".into(),
        },
        started_at: chrono::Utc::now(),
    }
}

#[test]
fn plan_blocks_when_agent_running() {
    let sched = Scheduler::new(default_registry());
    let agent = AgentState::Running { run: active_run() };
    let pending = PendingDecisions::default();
    let outcome = sched.plan(baseline_input(
        &agent,
        Phase::Idea,
        &pending,
        ctx(Phase::Idea, &[], &[]),
    ));
    assert_eq!(outcome, TickOutcome::Blocked(BlockReason::AgentBusy));
}

#[test]
fn plan_blocks_terminal_phase_before_paused() {
    let sched = Scheduler::new(default_registry());
    let agent = AgentState::Idle;
    let pending = PendingDecisions::default();
    let outcome = sched.plan(baseline_input(
        &agent,
        Phase::Done,
        &pending,
        ctx(Phase::Done, &[], &[]),
    ));
    assert_eq!(outcome, TickOutcome::Blocked(BlockReason::Terminal));
}

#[test]
fn plan_blocks_when_paused_matches_current_phase() {
    let sched = Scheduler::new(default_registry());
    let agent = AgentState::Idle;
    let pending = PendingDecisions::default();
    let mut input = baseline_input(&agent, Phase::Plan, &pending, ctx(Phase::Plan, &[], &[]));
    input.paused_at_phase = Some(Phase::Plan);
    assert_eq!(sched.plan(input), TickOutcome::Blocked(BlockReason::Paused));
}

#[test]
fn plan_ignores_pause_at_other_phase() {
    let sched = Scheduler::new(default_registry());
    let agent = AgentState::Idle;
    let pending = PendingDecisions::default();
    let mut input = baseline_input(&agent, Phase::Plan, &pending, ctx(Phase::Plan, &[], &[]));
    input.paused_at_phase = Some(Phase::Spec); // not current
    match sched.plan(input) {
        TickOutcome::Dispatch(spec) => assert_eq!(spec.stage_id, StageId::Planning),
        other => panic!("expected Dispatch, got {other:?}"),
    }
}

#[test]
fn plan_blocks_on_pending_decision() {
    let sched = Scheduler::new(default_registry());
    let agent = AgentState::Idle;
    let pending = PendingDecisions {
        git_guard: Some(GitGuardData),
        ..Default::default()
    };
    let outcome = sched.plan(baseline_input(
        &agent,
        Phase::Plan,
        &pending,
        ctx(Phase::Plan, &[], &[]),
    ));
    assert_eq!(outcome, TickOutcome::Blocked(BlockReason::PendingDecision));
}

#[test]
fn plan_blocks_on_project_lane() {
    let sched = Scheduler::new(default_registry());
    let agent = AgentState::Idle;
    let pending = PendingDecisions::default();
    let mut input = baseline_input(&agent, Phase::Plan, &pending, ctx(Phase::Plan, &[], &[]));
    input.project_lane_allows = false;
    assert_eq!(sched.plan(input), TickOutcome::Blocked(BlockReason::ProjectLane));
}

#[test]
fn plan_dispatches_planning_at_plan_with_no_prior() {
    let sched = Scheduler::new(default_registry());
    let agent = AgentState::Idle;
    let pending = PendingDecisions::default();
    match sched.plan(baseline_input(
        &agent,
        Phase::Plan,
        &pending,
        ctx(Phase::Plan, &[], &[]),
    )) {
        TickOutcome::Dispatch(spec) => {
            assert_eq!(spec.stage_id, StageId::Planning);
            assert_eq!(spec.round, 1);
            assert_eq!(spec.attempt, 1);
        }
        other => panic!("expected Dispatch, got {other:?}"),
    }
}

#[test]
fn plan_dispatches_plan_review_after_planning_done() {
    let sched = Scheduler::new(default_registry());
    let agent = AgentState::Idle;
    let pending = PendingDecisions::default();
    let prior = [done_run(StageId::Planning, None, 1)];
    match sched.plan(baseline_input(
        &agent,
        Phase::Plan,
        &pending,
        ctx(Phase::Plan, &prior, &[]),
    )) {
        TickOutcome::Dispatch(spec) => assert_eq!(spec.stage_id, StageId::PlanReview),
        other => panic!("expected Dispatch, got {other:?}"),
    }
}

#[test]
fn plan_returns_idle_when_no_candidate_has_pending_work() {
    let sched = Scheduler::new(default_registry());
    let agent = AgentState::Idle;
    let pending = PendingDecisions::default();
    // Brainstorm already done → next_stage_for_phase returns None.
    let prior = [done_run(StageId::Brainstorm, None, 1)];
    assert_eq!(
        sched.plan(baseline_input(
            &agent,
            Phase::Idea,
            &pending,
            ctx(Phase::Idea, &prior, &[]),
        )),
        TickOutcome::Idle
    );
}

#[test]
fn plan_block_precedence_agent_busy_beats_terminal() {
    let sched = Scheduler::new(default_registry());
    let agent = AgentState::Stopping {
        run: active_run(),
        after: AfterStop::GoIdle,
    };
    let pending = PendingDecisions::default();
    // Even on Done phase, AgentBusy wins.
    let outcome = sched.plan(baseline_input(
        &agent,
        Phase::Done,
        &pending,
        ctx(Phase::Done, &[], &[]),
    ));
    assert_eq!(outcome, TickOutcome::Blocked(BlockReason::AgentBusy));
}
