//! Shell-owned scan-derived queue scheduler.
//!
//! The scheduler is a pure decision module: given a creation-order-sorted
//! list of session snapshots (with per-entry load failures surfaced), it
//! produces planning-lane continuations and a single implementation-lane
//! decision. The shell turns that decision into a launch by consulting the
//! repo-state-update gating (task 6) and dispatching through the runner
//! supervisor. No persisted queue file lives anywhere.
//!
//! The module is intentionally IO-free so it can be unit-tested against
//! hand-built session lists without a tempdir.
use crate::state::Stage;

/// Lane categorization for a [`Stage`]. The scheduler uses lane membership
/// to decide whether a session may run alongside others (Planning) or must
/// be the sole active session in its lane across the entire project
/// (Implementation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageLane {
    Planning,
    Implementation,
    /// Terminal or operator-blocked: not eligible for either lane.
    Other,
}

/// Return the lane a `Stage` belongs to.
///
/// `IdeaInput`, `SkipToImplPending`, `GitGuardPending`, `WaitingToImplement`,
/// `Done`, `Cancelled`, and `BlockedNeedsUser` are deliberately `Other` —
/// they are not active automation stages. `WaitingToImplement` is the
/// implementation lane's head-of-queue candidate, not an occupant.
pub fn stage_lane(stage: Stage) -> StageLane {
    match stage.stage_lane() {
        crate::lifecycle::StageLane::Planning => StageLane::Planning,
        crate::lifecycle::StageLane::Implementation => StageLane::Implementation,
        crate::lifecycle::StageLane::Other => StageLane::Other,
    }
}

#[inline]
pub fn is_planning_lane_stage(stage: Stage) -> bool {
    stage_lane(stage) == StageLane::Planning
}

#[inline]
pub fn is_implementation_lane_stage(stage: Stage) -> bool {
    stage_lane(stage) == StageLane::Implementation
}

/// Minimal session snapshot the scheduler consumes. Decoupled from
/// `SessionEntry`/`SessionState` so callers can build inputs in tests
/// without touching disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerSession {
    pub session_id: String,
    pub current_stage: Stage,
}

/// One entry in a scan: either a loaded snapshot or a per-session load
/// error captured for operator-visible reporting.
#[derive(Debug, Clone)]
pub enum ScannedSession {
    Loaded(SchedulerSession),
    Corrupt { session_id: String, error: String },
}

impl ScannedSession {
    pub fn session_id(&self) -> &str {
        match self {
            Self::Loaded(s) => &s.session_id,
            Self::Corrupt { session_id, .. } => session_id,
        }
    }
}

/// A planning-lane continuation. The scheduler emits one per session whose
/// current stage is a planning-lane stage. Continuations are independent:
/// nothing else in the project blocks them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningContinuation {
    pub session_id: String,
    pub stage: Stage,
}

/// Outcome of the single implementation-lane decision per tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImplementationDecision {
    /// Some session is already in an implementation-lane stage. The shell
    /// must not start another implementation-lane stage anywhere.
    LaneOccupied { session_id: String, stage: Stage },
    /// Oldest unresolved session is `BlockedNeedsUser`. Lane is idle and
    /// remains idle until the operator resolves the block.
    BlockedByHead { session_id: String },
    /// Oldest unresolved session is still planning. Lane is idle this tick.
    PlanningHead { session_id: String, stage: Stage },
    /// Oldest unresolved session is `WaitingToImplement` and eligible. The
    /// shell consults the repo-state-update decider to choose between
    /// `RepoStateUpdateRunning` and `ShardingRunning`.
    DispatchWaiting { session_id: String },
    /// An earlier-than-head session could not be loaded. The lane is
    /// treated as blocked and an operator-visible error must surface.
    BlockedByCorruptEarlierSession { session_id: String, error: String },
    /// No unresolved session — every non-archived session is `Done` or
    /// `Cancelled`.
    NothingToDo,
}

/// Aggregate decision for a scheduler tick.
#[derive(Debug, Clone)]
pub struct SchedulerTick {
    /// Planning-lane sessions that the shell should keep running.
    pub planning: Vec<PlanningContinuation>,
    /// The single implementation-lane decision.
    pub implementation: ImplementationDecision,
    /// Corrupt sessions later than the head-of-queue. Logged for the
    /// operator but they do not block scheduling.
    pub skipped_corrupt_later_sessions: Vec<(String, String)>,
}

/// Evaluate a single scheduler tick from a creation-order-sorted scan.
///
/// `scanned` must already be sorted by session-id creation order
/// (ascending). Archived sessions must already be excluded — the scanner is
/// the single source of truth for the archived filter.
pub fn evaluate_tick(scanned: &[ScannedSession]) -> SchedulerTick {
    // Planning-lane scan: every loaded planning-lane session contributes a
    // continuation regardless of position in the queue. This is the spec's
    // "earlier `BlockedNeedsUser`, `WaitingToImplement`, running
    // implementation, or any other state never blocks a later session's
    // planning lane" rule.
    let mut planning = Vec::new();
    let mut lane_occupied: Option<(String, Stage)> = None;
    for entry in scanned {
        if let ScannedSession::Loaded(session) = entry {
            if is_planning_lane_stage(session.current_stage) {
                planning.push(PlanningContinuation {
                    session_id: session.session_id.clone(),
                    stage: session.current_stage,
                });
            }
            if lane_occupied.is_none() && is_implementation_lane_stage(session.current_stage) {
                lane_occupied = Some((session.session_id.clone(), session.current_stage));
            }
        }
    }

    // Implementation-lane head selection. Walk in creation order and stop
    // at the first session that isn't Done or Cancelled. Corrupt entries
    // encountered before the head block the lane (spec §3:
    // "if an earlier session cannot be loaded, treat it as blocking for
    // implementation scheduling and surface an operator-visible error").
    let mut implementation = ImplementationDecision::NothingToDo;
    let mut skipped_corrupt_later_sessions = Vec::new();
    let mut head_chosen = false;

    for entry in scanned {
        if head_chosen {
            if let ScannedSession::Corrupt { session_id, error } = entry {
                skipped_corrupt_later_sessions.push((session_id.clone(), error.clone()));
            }
            continue;
        }
        match entry {
            ScannedSession::Corrupt { session_id, error } => {
                implementation = ImplementationDecision::BlockedByCorruptEarlierSession {
                    session_id: session_id.clone(),
                    error: error.clone(),
                };
                head_chosen = true;
            }
            ScannedSession::Loaded(session) => {
                // Done and Cancelled sessions never count as the head; the
                // scheduler must walk past them to find a live candidate.
                if matches!(session.current_stage, Stage::Done | Stage::Cancelled) {
                    continue;
                }
                head_chosen = true;
                implementation = match session.current_stage {
                    Stage::BlockedNeedsUser => ImplementationDecision::BlockedByHead {
                        session_id: session.session_id.clone(),
                    },
                    Stage::WaitingToImplement => ImplementationDecision::DispatchWaiting {
                        session_id: session.session_id.clone(),
                    },
                    stage if is_planning_lane_stage(stage) => {
                        ImplementationDecision::PlanningHead {
                            session_id: session.session_id.clone(),
                            stage,
                        }
                    }
                    stage if is_implementation_lane_stage(stage) => {
                        // The head itself occupies the lane — preserved as
                        // LaneOccupied so callers get a single canonical
                        // "lane busy" answer.
                        ImplementationDecision::LaneOccupied {
                            session_id: session.session_id.clone(),
                            stage,
                        }
                    }
                    // Other (IdeaInput / SkipToImplPending / GitGuardPending):
                    // the session is alive but is waiting on the operator,
                    // not on the scheduler. The lane stays idle and we do
                    // not advance past it — these sessions also block later
                    // ones from being treated as the head, the same way
                    // BlockedNeedsUser does.
                    _ => ImplementationDecision::BlockedByHead {
                        session_id: session.session_id.clone(),
                    },
                };
            }
        }
    }

    // If a different session is already in an impl-lane stage, the lane
    // is occupied even if the oldest-not-Done head suggests otherwise.
    if let Some((occupant_id, occupant_stage)) = lane_occupied {
        // Skip the override when the head itself was already that occupant
        // (LaneOccupied was emitted in the head walk).
        let already_reported = matches!(
            &implementation,
            ImplementationDecision::LaneOccupied { session_id, .. }
                if session_id == &occupant_id
        );
        if !already_reported {
            implementation = ImplementationDecision::LaneOccupied {
                session_id: occupant_id,
                stage: occupant_stage,
            };
        }
    }

    SchedulerTick {
        planning,
        implementation,
        skipped_corrupt_later_sessions,
    }
}

/// Decision for a `WaitingToImplement` head when the implementation lane is
/// idle: should the scheduler launch a repo-state update first, or dispatch
/// straight to sharding?
///
/// Spec § Repo-state update stage: compare the session's recorded
/// `planned_after_session_id` with the newest-earlier-`Done` baseline at
/// scheduler-tick time. They are considered to match when they are equal
/// (including both `None`); otherwise the update must run. A baseline
/// recorded against a session that no longer exists looks the same as
/// "different from current baseline" — both fall to `RepoStateUpdate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitingDispatch {
    /// Baselines agree — go straight to sharding.
    Sharding,
    /// Baselines disagree — run the non-interactive repo-state update first.
    RepoStateUpdate,
}

/// Pure repo-state-update gating decision.
///
/// `planned_after_session_id` is what the session has persisted; `current_baseline`
/// is the newest earlier non-archived `Done` session at this tick (or `None`
/// when no such session exists yet). Both `None` is the "fresh queue, no
/// earlier work to reconcile against" case and routes to `Sharding`.
pub fn decide_waiting_dispatch(
    planned_after_session_id: Option<&str>,
    current_baseline: Option<&str>,
) -> WaitingDispatch {
    if planned_after_session_id == current_baseline {
        WaitingDispatch::Sharding
    } else {
        WaitingDispatch::RepoStateUpdate
    }
}

/// Manual-retry gating per spec § Auto-launch rules: a retry of an
/// implementation-lane stage is allowed only when the lane is otherwise
/// idle (other sessions are not in an impl-lane stage). Retries of
/// non-impl-lane stages are always allowed; the focused session may be
/// the one whose run just stopped, so its own (now-terminated) impl-lane
/// stage does not count as occupancy.
pub fn manual_retry_allowed(
    target_stage: Stage,
    focused_session_id: &str,
    scanned: &[ScannedSession],
) -> bool {
    if !is_implementation_lane_stage(target_stage) {
        return true;
    }
    !scanned.iter().any(|entry| match entry {
        ScannedSession::Loaded(session) => {
            session.session_id != focused_session_id
                && is_implementation_lane_stage(session.current_stage)
        }
        ScannedSession::Corrupt { .. } => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(id: &str, stage: Stage) -> ScannedSession {
        ScannedSession::Loaded(SchedulerSession {
            session_id: id.to_string(),
            current_stage: stage,
        })
    }

    fn corrupt(id: &str, error: &str) -> ScannedSession {
        ScannedSession::Corrupt {
            session_id: id.to_string(),
            error: error.to_string(),
        }
    }

    #[test]
    fn stage_lane_classifies_each_stage() {
        // Planning lane: explicit list from spec § Queue scheduler.
        for stage in [
            Stage::BrainstormRunning,
            Stage::SpecReviewRunning,
            Stage::SpecReviewPaused,
            Stage::PlanningRunning,
            Stage::PlanReviewRunning,
            Stage::PlanReviewPaused,
        ] {
            assert_eq!(stage_lane(stage), StageLane::Planning, "stage: {stage:?}");
        }
        // Implementation lane: explicit list from spec § Queue scheduler.
        for stage in [
            Stage::RepoStateUpdateRunning,
            Stage::ShardingRunning,
            Stage::Implementation(1),
            Stage::Review(2),
            Stage::Implementation(1),
            Stage::Implementation(1),
            Stage::Implementation(1),
            Stage::Review(1),
            Stage::FinalValidation(1),
            Stage::DreamingPending,
            Stage::Dreaming(1),
        ] {
            assert_eq!(
                stage_lane(stage),
                StageLane::Implementation,
                "stage: {stage:?}"
            );
        }
        // Not-eligible: terminal, operator-blocking, idle-head stages.
        for stage in [
            Stage::IdeaInput,
            Stage::WaitingToImplement,
            Stage::SkipToImplPending,
            Stage::GitGuardPending,
            Stage::Done,
            Stage::Cancelled,
            Stage::BlockedNeedsUser,
        ] {
            assert_eq!(stage_lane(stage), StageLane::Other, "stage: {stage:?}");
        }
    }

    #[test]
    fn planning_lane_continues_independently_of_other_sessions() {
        // Two planning-lane sessions and one BlockedNeedsUser head: the
        // block must not stop either planning continuation, satisfying
        // "earlier BlockedNeedsUser never blocks a later session's
        // planning lane."
        let scan = vec![
            loaded("01-blocked", Stage::BlockedNeedsUser),
            loaded("02-brainstorm", Stage::BrainstormRunning),
            loaded("03-plan-review-paused", Stage::PlanReviewPaused),
        ];
        let tick = evaluate_tick(&scan);
        let ids: Vec<&str> = tick
            .planning
            .iter()
            .map(|p| p.session_id.as_str())
            .collect();
        assert_eq!(ids, vec!["02-brainstorm", "03-plan-review-paused"]);
    }

    #[test]
    fn implementation_lane_single_occupancy_blocks_other_starts() {
        let scan = vec![
            loaded("01-sharding", Stage::ShardingRunning),
            loaded("02-waiting", Stage::WaitingToImplement),
        ];
        let tick = evaluate_tick(&scan);
        match tick.implementation {
            ImplementationDecision::LaneOccupied { session_id, stage } => {
                assert_eq!(session_id, "01-sharding");
                assert_eq!(stage, Stage::ShardingRunning);
            }
            other => panic!("expected LaneOccupied, got {other:?}"),
        }
    }

    #[test]
    fn implementation_lane_picks_oldest_waiting_when_idle() {
        let scan = vec![
            loaded("01-done", Stage::Done),
            loaded("02-cancelled", Stage::Cancelled),
            loaded("03-waiting", Stage::WaitingToImplement),
            loaded("04-waiting", Stage::WaitingToImplement),
        ];
        let tick = evaluate_tick(&scan);
        assert_eq!(
            tick.implementation,
            ImplementationDecision::DispatchWaiting {
                session_id: "03-waiting".into()
            }
        );
    }

    #[test]
    fn implementation_lane_blocked_when_head_is_blocked_needs_user() {
        // Head BlockedNeedsUser blocks the lane even when later sessions
        // are eligible WaitingToImplement.
        let scan = vec![
            loaded("01-blocked", Stage::BlockedNeedsUser),
            loaded("02-waiting", Stage::WaitingToImplement),
        ];
        let tick = evaluate_tick(&scan);
        assert_eq!(
            tick.implementation,
            ImplementationDecision::BlockedByHead {
                session_id: "01-blocked".into()
            }
        );
    }

    #[test]
    fn cancelled_session_does_not_block_later_implementation() {
        let scan = vec![
            loaded("01-cancelled", Stage::Cancelled),
            loaded("02-waiting", Stage::WaitingToImplement),
        ];
        let tick = evaluate_tick(&scan);
        assert_eq!(
            tick.implementation,
            ImplementationDecision::DispatchWaiting {
                session_id: "02-waiting".into()
            }
        );
    }

    #[test]
    fn planning_head_keeps_lane_idle_without_blocking_planning() {
        // Head is still planning — implementation lane idle this tick.
        // The same session is also reported in planning continuations.
        let scan = vec![
            loaded("01-planning", Stage::PlanningRunning),
            loaded("02-waiting", Stage::WaitingToImplement),
        ];
        let tick = evaluate_tick(&scan);
        match &tick.implementation {
            ImplementationDecision::PlanningHead { session_id, stage } => {
                assert_eq!(session_id, "01-planning");
                assert_eq!(*stage, Stage::PlanningRunning);
            }
            other => panic!("expected PlanningHead, got {other:?}"),
        }
        let planning_ids: Vec<&str> = tick
            .planning
            .iter()
            .map(|p| p.session_id.as_str())
            .collect();
        assert_eq!(planning_ids, vec!["01-planning"]);
    }

    #[test]
    fn corrupt_later_session_is_logged_and_skipped() {
        // Earlier waiting head is eligible; later corrupt entry is
        // captured for operator visibility but does not change the
        // decision.
        let scan = vec![
            loaded("01-waiting", Stage::WaitingToImplement),
            corrupt("02-corrupt", "broken toml"),
        ];
        let tick = evaluate_tick(&scan);
        assert_eq!(
            tick.implementation,
            ImplementationDecision::DispatchWaiting {
                session_id: "01-waiting".into()
            }
        );
        assert_eq!(
            tick.skipped_corrupt_later_sessions,
            vec![("02-corrupt".to_string(), "broken toml".to_string())]
        );
    }

    #[test]
    fn corrupt_earlier_session_blocks_implementation_lane() {
        let scan = vec![
            corrupt("01-corrupt", "fs error"),
            loaded("02-waiting", Stage::WaitingToImplement),
        ];
        let tick = evaluate_tick(&scan);
        match tick.implementation {
            ImplementationDecision::BlockedByCorruptEarlierSession { session_id, error } => {
                assert_eq!(session_id, "01-corrupt");
                assert_eq!(error, "fs error");
            }
            other => panic!("expected BlockedByCorruptEarlierSession, got {other:?}"),
        }
        // Later sessions can still run planning regardless of an earlier
        // corrupt entry — corruption only blocks the implementation lane.
        assert!(tick.skipped_corrupt_later_sessions.is_empty());
    }

    #[test]
    fn nothing_to_do_when_all_sessions_terminal() {
        let scan = vec![
            loaded("01-done", Stage::Done),
            loaded("02-cancelled", Stage::Cancelled),
        ];
        let tick = evaluate_tick(&scan);
        assert_eq!(tick.implementation, ImplementationDecision::NothingToDo);
        assert!(tick.planning.is_empty());
    }

    #[test]
    fn manual_retry_allowed_for_non_impl_lane_targets() {
        // A retry of a planning-lane stage is always allowed regardless
        // of what other sessions are doing.
        let scan = vec![loaded("01-sharding", Stage::ShardingRunning)];
        assert!(manual_retry_allowed(
            Stage::BrainstormRunning,
            "02-other",
            &scan
        ));
    }

    #[test]
    fn manual_retry_rejected_when_other_session_occupies_impl_lane() {
        let scan = vec![
            loaded("01-sharding", Stage::ShardingRunning),
            loaded("02-waiting", Stage::WaitingToImplement),
        ];
        // A retry of Sharding for session 02 is rejected because session 01
        // owns the implementation lane.
        assert!(!manual_retry_allowed(
            Stage::ShardingRunning,
            "02-waiting",
            &scan
        ));
    }

    #[test]
    fn manual_retry_allowed_when_only_focused_session_was_in_impl_lane() {
        // The focused session's own previous impl-lane stage doesn't count
        // as occupancy — it has been stopped and is being retried.
        let scan = vec![loaded("01-sharding", Stage::ShardingRunning)];
        assert!(manual_retry_allowed(
            Stage::ShardingRunning,
            "01-sharding",
            &scan
        ));
    }

    #[test]
    fn waiting_dispatch_routes_to_sharding_when_baselines_match() {
        assert_eq!(
            decide_waiting_dispatch(None, None),
            WaitingDispatch::Sharding
        );
        assert_eq!(
            decide_waiting_dispatch(
                Some("20260511-090000-000000001"),
                Some("20260511-090000-000000001")
            ),
            WaitingDispatch::Sharding
        );
    }

    #[test]
    fn waiting_dispatch_routes_to_repo_state_update_when_baselines_differ() {
        // Recorded baseline is older than current — new Done sessions landed
        // since planning, so the update must reconcile.
        assert_eq!(
            decide_waiting_dispatch(
                Some("20260511-090000-000000001"),
                Some("20260511-091000-000000001"),
            ),
            WaitingDispatch::RepoStateUpdate
        );
        // Planned with no prior baseline; a Done session has since appeared.
        assert_eq!(
            decide_waiting_dispatch(None, Some("20260511-091000-000000001")),
            WaitingDispatch::RepoStateUpdate
        );
        // Planned against a session that has since disappeared (e.g. archived);
        // current baseline is None — still a divergence.
        assert_eq!(
            decide_waiting_dispatch(Some("20260511-090000-000000001"), None),
            WaitingDispatch::RepoStateUpdate
        );
    }

    #[test]
    fn manual_retry_rejected_for_any_impl_lane_target_when_lane_busy() {
        // Cover the rest of the impl-lane stages the dispatch table can
        // retry (Recovery / Coder / Reviewer / FinalValidation /
        // Dreaming) to ensure they all gate on lane occupancy.
        let scan = vec![loaded("01-busy", Stage::Implementation(1))];
        for target in [
            Stage::ShardingRunning,
            Stage::Implementation(2),
            Stage::Implementation(2),
            Stage::Review(2),
            Stage::Review(2),
            Stage::FinalValidation(2),
            Stage::Dreaming(2),
        ] {
            assert!(
                !manual_retry_allowed(target, "02-other", &scan),
                "target {target:?}"
            );
        }
    }
}
