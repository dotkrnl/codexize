//! Integration tests for the queue scheduler.
//!
//! These exercise the scheduler against on-disk session state — the same
//! shape the shell will see in production — so the corrupt-session and
//! launch-origin invariants are covered end-to-end, not just at the pure
//! decision boundary.
use codexize::app::AppStartupOrigin;
use codexize::app_shell::AppShell;
use codexize::data::config::Config;
use codexize::data::picker_io::scan_sessions_for_scheduler;
use codexize::scheduler::{
    ImplementationDecision, ScannedSession, evaluate_tick, manual_retry_allowed,
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
            first.clone(),
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
