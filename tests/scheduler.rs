//! Integration tests for the queue scheduler.
//!
//! These exercise the scheduler against on-disk session state — the same
//! shape the shell will see in production — so the corrupt-session and
//! launch-origin invariants are covered end-to-end, not just at the pure
//! decision boundary.
use codexize::app::AppStartupOrigin;
use codexize::app_shell::AppShell;
use codexize::data::config::Config;
use codexize::data::picker_io::{newest_earlier_done_baseline, scan_sessions_for_scheduler};
use codexize::scheduler::{
    ImplementationDecision, ScannedSession, WaitingDispatch, decide_waiting_dispatch,
    evaluate_tick, manual_retry_allowed,
};
use codexize::state::{Phase, SessionState};
use serial_test::serial;
use std::path::PathBuf;
use std::sync::Arc;

fn with_temp_root<T>(f: impl FnOnce(PathBuf) -> T) -> T {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path().join(".codexize");
    let prev = std::env::var_os("CODEXIZE_ROOT");
    unsafe {
        std::env::set_var("CODEXIZE_ROOT", &root);
    }
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(root.join("sessions"))));
    unsafe {
        match prev {
            Some(value) => std::env::set_var("CODEXIZE_ROOT", value),
            None => std::env::remove_var("CODEXIZE_ROOT"),
        }
    }
    result.expect("test panicked")
}

fn save_session(id: &str, phase: Phase) -> SessionState {
    let mut state = SessionState::new(id.to_string());
    state.idea_text = Some(format!("idea for {id}"));
    state.current_phase = phase;
    state.save().expect("save session");
    state
}

#[test]
#[serial]
fn scheduler_picks_oldest_waiting_to_implement_session() {
    with_temp_root(|sessions_root| {
        save_session("20260511-090000-000000001", Phase::Done);
        save_session("20260511-091000-000000001", Phase::WaitingToImplement);
        save_session("20260511-092000-000000001", Phase::WaitingToImplement);

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        let tick = evaluate_tick(&scan);
        assert_eq!(
            tick.implementation,
            ImplementationDecision::DispatchWaiting {
                session_id: "20260511-091000-000000001".into()
            }
        );
        assert!(tick.planning.is_empty());
    });
}

#[test]
#[serial]
fn scheduler_planning_lane_independent_when_head_blocked() {
    with_temp_root(|sessions_root| {
        save_session("20260511-090000-000000001", Phase::BlockedNeedsUser);
        save_session("20260511-091000-000000001", Phase::PlanningRunning);
        save_session("20260511-092000-000000001", Phase::BrainstormRunning);

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        let tick = evaluate_tick(&scan);

        // Head blocks the implementation lane.
        assert_eq!(
            tick.implementation,
            ImplementationDecision::BlockedByHead {
                session_id: "20260511-090000-000000001".into()
            }
        );

        // Both later planning-lane sessions still get continuations.
        let planning_ids: Vec<&str> = tick
            .planning
            .iter()
            .map(|p| p.session_id.as_str())
            .collect();
        assert_eq!(
            planning_ids,
            vec!["20260511-091000-000000001", "20260511-092000-000000001"]
        );
    });
}

#[test]
#[serial]
fn scheduler_does_not_start_second_implementation_when_lane_occupied() {
    with_temp_root(|sessions_root| {
        save_session("20260511-090000-000000001", Phase::ShardingRunning);
        save_session("20260511-091000-000000001", Phase::WaitingToImplement);
        save_session("20260511-092000-000000001", Phase::BrainstormRunning);

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        let tick = evaluate_tick(&scan);

        match tick.implementation {
            ImplementationDecision::LaneOccupied { session_id, phase } => {
                assert_eq!(session_id, "20260511-090000-000000001");
                assert_eq!(phase, Phase::ShardingRunning);
            }
            other => panic!("expected LaneOccupied, got {other:?}"),
        }

        // The later brainstorm session still appears in planning continuations.
        let planning_ids: Vec<&str> = tick
            .planning
            .iter()
            .map(|p| p.session_id.as_str())
            .collect();
        assert_eq!(planning_ids, vec!["20260511-092000-000000001"]);
    });
}

#[test]
#[serial]
fn scheduler_skips_cancelled_session_and_picks_later_waiting() {
    with_temp_root(|sessions_root| {
        save_session("20260511-090000-000000001", Phase::Cancelled);
        save_session("20260511-091000-000000001", Phase::WaitingToImplement);

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        let tick = evaluate_tick(&scan);
        assert_eq!(
            tick.implementation,
            ImplementationDecision::DispatchWaiting {
                session_id: "20260511-091000-000000001".into()
            }
        );
    });
}

#[test]
#[serial]
fn scheduler_excludes_archived_session_from_lane_decision() {
    with_temp_root(|sessions_root| {
        let mut archived = SessionState::new("20260511-090000-000000001".into());
        archived.current_phase = Phase::ShardingRunning;
        archived.archived = true;
        archived.save().expect("save archived");
        save_session("20260511-091000-000000001", Phase::WaitingToImplement);

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        // Archived session must not appear in the scan at all.
        assert!(
            scan.iter()
                .all(|entry| entry.session_id() != "20260511-090000-000000001")
        );
        let tick = evaluate_tick(&scan);
        // Even though disk has a ShardingRunning archived session, the
        // implementation lane is considered idle and the later waiting
        // session dispatches.
        assert_eq!(
            tick.implementation,
            ImplementationDecision::DispatchWaiting {
                session_id: "20260511-091000-000000001".into()
            }
        );
    });
}

#[test]
#[serial]
fn scheduler_corrupt_earlier_session_blocks_implementation_lane() {
    with_temp_root(|sessions_root| {
        // Write a malformed session.toml under an "earlier" id.
        let bad_dir = sessions_root.join("20260511-090000-000000001");
        std::fs::create_dir_all(&bad_dir).expect("mkdir");
        std::fs::write(bad_dir.join("session.toml"), "not = valid = toml").expect("write");
        save_session("20260511-091000-000000001", Phase::WaitingToImplement);

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        let tick = evaluate_tick(&scan);
        match tick.implementation {
            ImplementationDecision::BlockedByCorruptEarlierSession { session_id, error } => {
                assert_eq!(session_id, "20260511-090000-000000001");
                assert!(!error.is_empty());
            }
            other => panic!("expected BlockedByCorruptEarlierSession, got {other:?}"),
        }
    });
}

#[test]
#[serial]
fn scheduler_corrupt_later_session_logged_not_blocking() {
    with_temp_root(|sessions_root| {
        save_session("20260511-090000-000000001", Phase::WaitingToImplement);
        let bad_dir = sessions_root.join("20260511-091000-000000001");
        std::fs::create_dir_all(&bad_dir).expect("mkdir");
        std::fs::write(bad_dir.join("session.toml"), "not = valid = toml").expect("write");

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        let tick = evaluate_tick(&scan);
        assert_eq!(
            tick.implementation,
            ImplementationDecision::DispatchWaiting {
                session_id: "20260511-090000-000000001".into()
            }
        );
        assert_eq!(tick.skipped_corrupt_later_sessions.len(), 1);
        assert_eq!(
            tick.skipped_corrupt_later_sessions[0].0,
            "20260511-091000-000000001"
        );
    });
}

#[test]
#[serial]
fn opening_or_focusing_session_does_not_launch_or_mutate_phase() {
    // AC-4 / AC-9: launches originate only from creation, explicit retry, or
    // the scheduler. The shell's open/focus paths must not change a session's
    // phase, current_run_id, or run-launched state on their own.
    with_temp_root(|_sessions_root| {
        let first = save_session("20260511-090000-000000001", Phase::WaitingToImplement);
        save_session("20260511-091000-000000001", Phase::WaitingToImplement);

        let mut shell = AppShell::new(
            first,
            AppStartupOrigin::Default,
            Arc::new(Config::baked_defaults()),
        )
        .expect("shell");

        // Open the second session; phases on disk and in the workspace must
        // be unchanged — no auto-launch ran.
        shell
            .open_session("20260511-091000-000000001")
            .expect("open");

        assert_eq!(shell.running_session_id(), None);
        let opened = shell
            .workspace("20260511-091000-000000001")
            .expect("workspace");
        assert_eq!(opened.phase(), Phase::WaitingToImplement);
        assert_eq!(opened.current_run_id(), None);

        // Focusing back to the original does not start a run either.
        shell
            .focus_session("20260511-090000-000000001")
            .expect("focus");
        assert_eq!(shell.running_session_id(), None);
        assert_eq!(
            shell
                .workspace("20260511-090000-000000001")
                .expect("workspace")
                .current_run_id(),
            None
        );
    });
}

#[test]
#[serial]
fn creating_session_lands_in_brainstorm_so_scheduler_can_continue() {
    // AC-9: "creating a new session automatically starts brainstorm". At the
    // scheduler boundary that means the freshly created session enters
    // `BrainstormRunning` and the next planning-lane scan picks it up so the
    // auto-launch path can fire — no implementation-lane dispatch should
    // occur from creation alone.
    with_temp_root(|sessions_root| {
        let session_id =
            codexize::picker::create_session("explore caching layer", Default::default(), None)
                .expect("create");
        let state = SessionState::load(&session_id).expect("load");
        assert_eq!(state.current_phase, Phase::BrainstormRunning);

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        let tick = evaluate_tick(&scan);
        let ids: Vec<&str> = tick
            .planning
            .iter()
            .map(|p| p.session_id.as_str())
            .collect();
        assert_eq!(ids, vec![session_id.as_str()]);
        // The head is also a planning session — implementation lane idle.
        match tick.implementation {
            ImplementationDecision::PlanningHead { session_id: id, .. } => {
                assert_eq!(id, session_id);
            }
            other => panic!("expected PlanningHead, got {other:?}"),
        }
    });
}

#[test]
#[serial]
fn manual_retry_rejected_for_sharding_when_other_session_occupies_lane() {
    with_temp_root(|sessions_root| {
        save_session("20260511-090000-000000001", Phase::ShardingRunning);
        save_session("20260511-091000-000000001", Phase::WaitingToImplement);

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        // Operator focused session 02 and asked to retry sharding: rejected
        // because session 01 owns the implementation lane.
        assert!(!manual_retry_allowed(
            Phase::ShardingRunning,
            "20260511-091000-000000001",
            &scan
        ));
        // Planning-lane retries (e.g. brainstorm) are always allowed.
        assert!(manual_retry_allowed(
            Phase::BrainstormRunning,
            "20260511-091000-000000001",
            &scan
        ));
    });
}

fn save_session_with_baseline(id: &str, phase: Phase, planned_after: Option<&str>) -> SessionState {
    let mut state = SessionState::new(id.to_string());
    state.idea_text = Some(format!("idea for {id}"));
    state.current_phase = phase;
    state.planned_after_session_id = planned_after.map(str::to_string);
    state.save().expect("save session");
    state
}

#[test]
#[serial]
fn dispatch_waiting_routes_to_sharding_when_baseline_matches_recorded() {
    // Spec § Repo-state update: when `planned_after_session_id` equals the
    // current newest-earlier-Done baseline, the WaitingToImplement head
    // skips repo-state update and goes straight to sharding.
    with_temp_root(|sessions_root| {
        save_session("20260511-090000-000000001", Phase::Done);
        save_session_with_baseline(
            "20260511-091000-000000001",
            Phase::WaitingToImplement,
            Some("20260511-090000-000000001"),
        );

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        let tick = evaluate_tick(&scan);
        let session_id = match tick.implementation {
            ImplementationDecision::DispatchWaiting { session_id } => session_id,
            other => panic!("expected DispatchWaiting, got {other:?}"),
        };
        let entries = codexize::data::picker_io::scan_sessions_by_creation_order(&sessions_root)
            .expect("scan by creation order");
        let baseline = newest_earlier_done_baseline(&session_id, &entries);
        let state = SessionState::load(&session_id).expect("load");
        assert_eq!(
            decide_waiting_dispatch(
                state.planned_after_session_id.as_deref(),
                baseline.as_deref(),
            ),
            WaitingDispatch::Sharding
        );
    });
}

#[test]
#[serial]
fn dispatch_waiting_routes_to_repo_state_update_when_baseline_advanced() {
    // A newer Done session has landed since the head was planned, so the
    // baseline-comparison says "different" and the stage must run before
    // sharding.
    with_temp_root(|sessions_root| {
        save_session("20260511-088000-000000001", Phase::Done);
        save_session("20260511-090000-000000001", Phase::Done);
        save_session_with_baseline(
            "20260511-091000-000000001",
            Phase::WaitingToImplement,
            Some("20260511-088000-000000001"),
        );

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        let tick = evaluate_tick(&scan);
        let session_id = match tick.implementation {
            ImplementationDecision::DispatchWaiting { session_id } => session_id,
            other => panic!("expected DispatchWaiting, got {other:?}"),
        };
        let entries = codexize::data::picker_io::scan_sessions_by_creation_order(&sessions_root)
            .expect("scan by creation order");
        let baseline = newest_earlier_done_baseline(&session_id, &entries);
        let state = SessionState::load(&session_id).expect("load");
        assert_eq!(
            decide_waiting_dispatch(
                state.planned_after_session_id.as_deref(),
                baseline.as_deref(),
            ),
            WaitingDispatch::RepoStateUpdate
        );
    });
}

#[test]
#[serial]
fn dispatch_waiting_routes_to_repo_state_update_when_recorded_baseline_missing() {
    // The recorded baseline points at a session that has since been deleted
    // or archived (here, simply never present); the stage must still run
    // against the current state of the queue.
    with_temp_root(|sessions_root| {
        save_session("20260511-090000-000000001", Phase::Done);
        save_session_with_baseline(
            "20260511-091000-000000001",
            Phase::WaitingToImplement,
            Some("20260511-089000-000000001"), // never existed
        );

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        let tick = evaluate_tick(&scan);
        let ImplementationDecision::DispatchWaiting { session_id } = tick.implementation else {
            panic!("expected DispatchWaiting");
        };
        let entries = codexize::data::picker_io::scan_sessions_by_creation_order(&sessions_root)
            .expect("scan by creation order");
        let baseline = newest_earlier_done_baseline(&session_id, &entries);
        let state = SessionState::load(&session_id).expect("load");
        assert_eq!(
            decide_waiting_dispatch(
                state.planned_after_session_id.as_deref(),
                baseline.as_deref(),
            ),
            WaitingDispatch::RepoStateUpdate
        );
    });
}

#[test]
#[serial]
fn dispatch_waiting_skips_repo_state_update_when_no_earlier_done_exists() {
    // Both baselines are None — no prior Done session has been observed and
    // none exists now, so direct sharding is the right call.
    with_temp_root(|sessions_root| {
        save_session_with_baseline("20260511-090000-000000001", Phase::WaitingToImplement, None);

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        let tick = evaluate_tick(&scan);
        let ImplementationDecision::DispatchWaiting { session_id } = tick.implementation else {
            panic!("expected DispatchWaiting");
        };
        let entries = codexize::data::picker_io::scan_sessions_by_creation_order(&sessions_root)
            .expect("scan by creation order");
        let baseline = newest_earlier_done_baseline(&session_id, &entries);
        let state = SessionState::load(&session_id).expect("load");
        assert_eq!(baseline, None);
        assert_eq!(state.planned_after_session_id, None);
        assert_eq!(
            decide_waiting_dispatch(
                state.planned_after_session_id.as_deref(),
                baseline.as_deref(),
            ),
            WaitingDispatch::Sharding
        );
    });
}

#[test]
#[serial]
fn repo_state_update_policy_restricts_allowed_writes() {
    // Spec § Repo-state update: outputs are restricted to current session
    // spec.md, plan.md, the repo-state-update.toml report, the live summary,
    // and bounded memory updates. The policy's shell allowlist must allow
    // only read-only git inspection commands. Workspace must stay read-only.
    use codexize::acp::{AcpLaunchPolicy, AcpShellCommandPolicy};
    let policy = AcpLaunchPolicy::repo_state_update(
        std::path::Path::new("/s/artifacts/spec.md"),
        std::path::Path::new("/s/artifacts/plan.md"),
        std::path::Path::new("/s/artifacts/repo-state-update.toml"),
        std::path::Path::new("/s/artifacts/live_summary.txt"),
    );
    assert!(policy.enforce_readonly_workspace);
    let allowed: Vec<&str> = policy
        .allowed_write_paths
        .iter()
        .map(|p| p.to_str().unwrap_or(""))
        .collect();
    assert!(allowed.iter().any(|p| p.ends_with("spec.md")));
    assert!(allowed.iter().any(|p| p.ends_with("plan.md")));
    assert!(
        allowed
            .iter()
            .any(|p| p.ends_with("repo-state-update.toml"))
    );
    assert!(allowed.iter().any(|p| p.ends_with("live_summary.txt")));
    // No tasks.toml or other-session paths leaked into the write set.
    assert!(
        !allowed.iter().any(|p| p.ends_with("tasks.toml")),
        "tasks.toml must be read-only for the reconciliation agent"
    );
    let shell = match policy.shell_policy {
        AcpShellCommandPolicy::Allowlist(list) => list,
        AcpShellCommandPolicy::FullAccess => panic!("repo-state update must allowlist shell"),
    };
    assert!(shell.iter().any(|cmd| cmd == "git status"));
    assert!(shell.iter().any(|cmd| cmd == "git diff"));
    assert!(shell.iter().any(|cmd| cmd == "git rev-parse"));
    assert!(
        !shell.iter().any(|cmd| cmd == "git commit"),
        "shell allowlist must not include mutating git commands"
    );
}

#[test]
#[serial]
fn scan_propagates_session_id_creation_order_to_scheduler() {
    with_temp_root(|sessions_root| {
        // Write in arbitrary file-system order; scanner must sort ascending.
        save_session("20260511-093000-000000001", Phase::WaitingToImplement);
        save_session("20260511-090000-000000001", Phase::WaitingToImplement);
        save_session("20260511-092000-000000001", Phase::Cancelled);

        let scan = scan_sessions_for_scheduler(&sessions_root).expect("scan");
        let ids: Vec<&str> = scan.iter().map(ScannedSession::session_id).collect();
        assert_eq!(
            ids,
            vec![
                "20260511-090000-000000001",
                "20260511-092000-000000001",
                "20260511-093000-000000001",
            ]
        );
    });
}
