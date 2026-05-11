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
use crate::state::Phase;

/// Where a runner launch may legitimately originate.
///
/// The shell rejects any launch attempt whose origin is not one of these
/// three. `Creation` covers the brainstorm auto-start when a new session is
/// created from the picker. `Retry` covers operator-driven retries (palette
/// commands, the stage-error modal's "Retry" action, the
/// stop-and-retry termination flow). `Scheduler` covers planning-lane
/// continuations and the implementation-lane head dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchOrigin {
    Creation,
    Retry,
    Scheduler,
}

/// Lane categorization for a [`Phase`]. The scheduler uses lane membership
/// to decide whether a session may run alongside others (Planning) or must
/// be the sole active session in its lane across the entire project
/// (Implementation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseLane {
    Planning,
    Implementation,
    /// Terminal or operator-blocked: not eligible for either lane.
    Other,
}

/// Return the lane a `Phase` belongs to.
///
/// `IdeaInput`, `SkipToImplPending`, `GitGuardPending`, `WaitingToImplement`,
/// `Done`, `Cancelled`, and `BlockedNeedsUser` are deliberately `Other` —
/// they are not active automation phases. `WaitingToImplement` is the
/// implementation lane's head-of-queue candidate, not an occupant.
pub fn phase_lane(phase: Phase) -> PhaseLane {
    match phase {
        Phase::BrainstormRunning
        | Phase::SpecReviewRunning
        | Phase::SpecReviewPaused
        | Phase::PlanningRunning
        | Phase::PlanReviewRunning
        | Phase::PlanReviewPaused => PhaseLane::Planning,
        Phase::RepoStateUpdateRunning
        | Phase::ShardingRunning
        | Phase::ImplementationRound(_)
        | Phase::ReviewRound(_)
        | Phase::BuilderRecovery(_)
        | Phase::BuilderRecoveryPlanReview(_)
        | Phase::BuilderRecoverySharding(_)
        | Phase::Simplification(_)
        | Phase::FinalValidation(_)
        | Phase::DreamingPending
        | Phase::Dreaming(_) => PhaseLane::Implementation,
        Phase::IdeaInput
        | Phase::WaitingToImplement
        | Phase::SkipToImplPending
        | Phase::GitGuardPending
        | Phase::Done
        | Phase::Cancelled
        | Phase::BlockedNeedsUser => PhaseLane::Other,
    }
}

#[inline]
pub fn is_planning_lane_phase(phase: Phase) -> bool {
    phase_lane(phase) == PhaseLane::Planning
}

#[inline]
pub fn is_implementation_lane_phase(phase: Phase) -> bool {
    phase_lane(phase) == PhaseLane::Implementation
}

/// Minimal session snapshot the scheduler consumes. Decoupled from
/// `SessionEntry`/`SessionState` so callers can build inputs in tests
/// without touching disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerSession {
    pub session_id: String,
    pub current_phase: Phase,
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
/// current phase is a planning-lane phase. Continuations are independent:
/// nothing else in the project blocks them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanningContinuation {
    pub session_id: String,
    pub phase: Phase,
}

/// Outcome of the single implementation-lane decision per tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImplementationDecision {
    /// Some session is already in an implementation-lane phase. The shell
    /// must not start another implementation-lane stage anywhere.
    LaneOccupied { session_id: String, phase: Phase },
    /// Oldest unresolved session is `BlockedNeedsUser`. Lane is idle and
    /// remains idle until the operator resolves the block.
    BlockedByHead { session_id: String },
    /// Oldest unresolved session is still planning. Lane is idle this tick.
    PlanningHead { session_id: String, phase: Phase },
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
    let mut lane_occupied: Option<(String, Phase)> = None;
    for entry in scanned {
        if let ScannedSession::Loaded(session) = entry {
            if is_planning_lane_phase(session.current_phase) {
                planning.push(PlanningContinuation {
                    session_id: session.session_id.clone(),
                    phase: session.current_phase,
                });
            }
            if lane_occupied.is_none() && is_implementation_lane_phase(session.current_phase) {
                lane_occupied = Some((session.session_id.clone(), session.current_phase));
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
                if matches!(session.current_phase, Phase::Done | Phase::Cancelled) {
                    continue;
                }
                head_chosen = true;
                implementation = match session.current_phase {
                    Phase::BlockedNeedsUser => ImplementationDecision::BlockedByHead {
                        session_id: session.session_id.clone(),
                    },
                    Phase::WaitingToImplement => ImplementationDecision::DispatchWaiting {
                        session_id: session.session_id.clone(),
                    },
                    phase if is_planning_lane_phase(phase) => {
                        ImplementationDecision::PlanningHead {
                            session_id: session.session_id.clone(),
                            phase,
                        }
                    }
                    phase if is_implementation_lane_phase(phase) => {
                        // The head itself occupies the lane — preserved as
                        // LaneOccupied so callers get a single canonical
                        // "lane busy" answer.
                        ImplementationDecision::LaneOccupied {
                            session_id: session.session_id.clone(),
                            phase,
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

    // If a different session is already in an impl-lane phase, the lane
    // is occupied even if the oldest-not-Done head suggests otherwise.
    if let Some((occupant_id, occupant_phase)) = lane_occupied {
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
                phase: occupant_phase,
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
/// idle (other sessions are not in an impl-lane phase). Retries of
/// non-impl-lane stages are always allowed; the focused session may be
/// the one whose run just stopped, so its own (now-terminated) impl-lane
/// phase does not count as occupancy.
pub fn manual_retry_allowed(
    target_phase: Phase,
    focused_session_id: &str,
    scanned: &[ScannedSession],
) -> bool {
    if !is_implementation_lane_phase(target_phase) {
        return true;
    }
    !scanned.iter().any(|entry| match entry {
        ScannedSession::Loaded(session) => {
            session.session_id != focused_session_id
                && is_implementation_lane_phase(session.current_phase)
        }
        ScannedSession::Corrupt { .. } => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(id: &str, phase: Phase) -> ScannedSession {
        ScannedSession::Loaded(SchedulerSession {
            session_id: id.to_string(),
            current_phase: phase,
        })
    }

    fn corrupt(id: &str, error: &str) -> ScannedSession {
        ScannedSession::Corrupt {
            session_id: id.to_string(),
            error: error.to_string(),
        }
    }

    #[test]
    fn phase_lane_classifies_each_phase() {
        // Planning lane: explicit list from spec § Queue scheduler.
        for phase in [
            Phase::BrainstormRunning,
            Phase::SpecReviewRunning,
            Phase::SpecReviewPaused,
            Phase::PlanningRunning,
            Phase::PlanReviewRunning,
            Phase::PlanReviewPaused,
        ] {
            assert_eq!(phase_lane(phase), PhaseLane::Planning, "phase: {:?}", phase);
        }
        // Implementation lane: explicit list from spec § Queue scheduler.
        for phase in [
            Phase::RepoStateUpdateRunning,
            Phase::ShardingRunning,
            Phase::ImplementationRound(1),
            Phase::ReviewRound(2),
            Phase::BuilderRecovery(1),
            Phase::BuilderRecoveryPlanReview(1),
            Phase::BuilderRecoverySharding(1),
            Phase::Simplification(1),
            Phase::FinalValidation(1),
            Phase::DreamingPending,
            Phase::Dreaming(1),
        ] {
            assert_eq!(
                phase_lane(phase),
                PhaseLane::Implementation,
                "phase: {:?}",
                phase
            );
        }
        // Not-eligible: terminal, operator-blocking, idle-head phases.
        for phase in [
            Phase::IdeaInput,
            Phase::WaitingToImplement,
            Phase::SkipToImplPending,
            Phase::GitGuardPending,
            Phase::Done,
            Phase::Cancelled,
            Phase::BlockedNeedsUser,
        ] {
            assert_eq!(phase_lane(phase), PhaseLane::Other, "phase: {:?}", phase);
        }
    }

    #[test]
    fn planning_lane_continues_independently_of_other_sessions() {
        // Two planning-lane sessions and one BlockedNeedsUser head: the
        // block must not stop either planning continuation, satisfying
        // "earlier BlockedNeedsUser never blocks a later session's
        // planning lane."
        let scan = vec![
            loaded("01-blocked", Phase::BlockedNeedsUser),
            loaded("02-brainstorm", Phase::BrainstormRunning),
            loaded("03-plan-review-paused", Phase::PlanReviewPaused),
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
            loaded("01-sharding", Phase::ShardingRunning),
            loaded("02-waiting", Phase::WaitingToImplement),
        ];
        let tick = evaluate_tick(&scan);
        match tick.implementation {
            ImplementationDecision::LaneOccupied { session_id, phase } => {
                assert_eq!(session_id, "01-sharding");
                assert_eq!(phase, Phase::ShardingRunning);
            }
            other => panic!("expected LaneOccupied, got {other:?}"),
        }
    }

    #[test]
    fn implementation_lane_picks_oldest_waiting_when_idle() {
        let scan = vec![
            loaded("01-done", Phase::Done),
            loaded("02-cancelled", Phase::Cancelled),
            loaded("03-waiting", Phase::WaitingToImplement),
            loaded("04-waiting", Phase::WaitingToImplement),
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
            loaded("01-blocked", Phase::BlockedNeedsUser),
            loaded("02-waiting", Phase::WaitingToImplement),
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
            loaded("01-cancelled", Phase::Cancelled),
            loaded("02-waiting", Phase::WaitingToImplement),
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
            loaded("01-planning", Phase::PlanningRunning),
            loaded("02-waiting", Phase::WaitingToImplement),
        ];
        let tick = evaluate_tick(&scan);
        match &tick.implementation {
            ImplementationDecision::PlanningHead { session_id, phase } => {
                assert_eq!(session_id, "01-planning");
                assert_eq!(*phase, Phase::PlanningRunning);
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
            loaded("01-waiting", Phase::WaitingToImplement),
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
            loaded("02-waiting", Phase::WaitingToImplement),
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
            loaded("01-done", Phase::Done),
            loaded("02-cancelled", Phase::Cancelled),
        ];
        let tick = evaluate_tick(&scan);
        assert_eq!(tick.implementation, ImplementationDecision::NothingToDo);
        assert!(tick.planning.is_empty());
    }

    #[test]
    fn manual_retry_allowed_for_non_impl_lane_targets() {
        // A retry of a planning-lane stage is always allowed regardless
        // of what other sessions are doing.
        let scan = vec![loaded("01-sharding", Phase::ShardingRunning)];
        assert!(manual_retry_allowed(
            Phase::BrainstormRunning,
            "02-other",
            &scan
        ));
    }

    #[test]
    fn manual_retry_rejected_when_other_session_occupies_impl_lane() {
        let scan = vec![
            loaded("01-sharding", Phase::ShardingRunning),
            loaded("02-waiting", Phase::WaitingToImplement),
        ];
        // A retry of Sharding for session 02 is rejected because session 01
        // owns the implementation lane.
        assert!(!manual_retry_allowed(
            Phase::ShardingRunning,
            "02-waiting",
            &scan
        ));
    }

    #[test]
    fn manual_retry_allowed_when_only_focused_session_was_in_impl_lane() {
        // The focused session's own previous impl-lane phase doesn't count
        // as occupancy — it has been stopped and is being retried.
        let scan = vec![loaded("01-sharding", Phase::ShardingRunning)];
        assert!(manual_retry_allowed(
            Phase::ShardingRunning,
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
        // Cover the rest of the impl-lane phases the dispatch table can
        // retry (Recovery / Coder / Reviewer / FinalValidation /
        // Dreaming) to ensure they all gate on lane occupancy.
        let scan = vec![loaded("01-busy", Phase::ImplementationRound(1))];
        for target in [
            Phase::ShardingRunning,
            Phase::BuilderRecovery(2),
            Phase::ImplementationRound(2),
            Phase::ReviewRound(2),
            Phase::Simplification(2),
            Phase::FinalValidation(2),
            Phase::Dreaming(2),
        ] {
            assert!(
                !manual_retry_allowed(target, "02-other", &scan),
                "target {target:?}"
            );
        }
    }
}
