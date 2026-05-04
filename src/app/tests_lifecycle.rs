// tests_lifecycle.rs
use super::tree::node_at_path;
use super::*;
use super::{prompts::*, test_harness::*};
use crate::{
    adapters::EffortLevel,
    selection::{self},
    state::{
        self as session_state, Message, MessageKind, MessageSender, PendingGuardDecision, Phase,
        PipelineItem, PipelineItemStatus, RunRecord, RunStatus, SessionState,
    },
};

#[test]
fn startup_refresh_remains_fetching_when_quotas_expired() {
    let loaded = cache::LoadedCache {
        dashboard: Some(cache::LoadedSection {
            data: Vec::new(),
            expired: false,
        }),
        quotas: Some(cache::LoadedSection {
            data: std::collections::BTreeMap::new(),
            expired: true,
        }),
        quota_resets: None,
    };

    assert!(startup_cache_has_expired_section(&loaded));
}

#[test]
fn previous_stage_stays_expanded_after_phase_advance() {
    with_temp_root(|| {
        // Mid-Brainstorm: Brainstorm row is the current stage so it auto-expands.
        let mut state = SessionState::new("phase-keep".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Brainstorm]".to_string(),
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
        let bs_idx = row_index(&app, "Brainstorm");
        assert!(app.is_expanded(bs_idx), "precondition: Brainstorm expanded");
        // Simulate a render cycle: any visible expanded row gets latched
        // as an explicit Expanded override so it survives later state
        // shifts (run rollup, current_node moving forward).
        app.latch_visible_expansions();

        // Mark Brainstorm Done and advance phase.
        if let Some(run) = app
            .state
            .agent_runs
            .iter_mut()
            .find(|r| r.stage == "brainstorm")
        {
            run.status = RunStatus::Done;
            run.ended_at = Some(chrono::Utc::now());
        }
        app.transition_to_phase(Phase::SpecReviewRunning).unwrap();

        let bs_idx = row_index(&app, "Brainstorm");
        assert!(
            app.is_expanded(bs_idx),
            "Brainstorm should stay expanded after phase advance"
        );
    });
}

#[test]
fn current_stage_is_always_expanded() {
    let app = mk_app(mk_state_with_runs());
    let current = app.current_row();
    assert!(app.is_expanded(current));
}

#[test]
fn toggle_expand_adds_then_removes_by_node_key() {
    let mut app = mk_app(mk_state_with_runs());
    let bs_idx = row_index(&app, "Brainstorm");
    let bs_key = app.visible_rows[bs_idx].key.clone();
    app.selected = bs_idx;
    assert!(app.is_expanded(bs_idx));
    app.toggle_expand_focused();
    assert!(!app.is_expanded(bs_idx));
    assert_eq!(
        app.collapsed_overrides.get(&bs_key),
        Some(&ExpansionOverride::Collapsed)
    );
    app.toggle_expand_focused();
    assert!(app.is_expanded(bs_idx));
    assert!(!app.collapsed_overrides.contains_key(&bs_key));
}

#[test]
fn active_current_stage_collapse_override_collapses_row() {
    let mut app = mk_app(mk_state_with_runs());
    let current = app.current_row();
    let current_key = app.visible_rows[current].key.clone();
    app.selected = current;
    app.toggle_expand_focused();
    assert_eq!(
        app.collapsed_overrides.get(&current_key),
        Some(&ExpansionOverride::Collapsed)
    );
    assert!(!app.is_expanded(current));
}

#[test]
fn active_path_respects_collapsed_ancestors() {
    let mut state = SessionState::new("active-path".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    state.builder.current_task = Some(7);
    state.agent_runs.push(RunRecord {
        id: 10,
        stage: "coder".to_string(),
        task_id: Some(7),
        round: 1,
        attempt: 1,
        model: "claude".to_string(),
        vendor: "anthropic".to_string(),
        window_name: "[Builder]".to_string(),
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
    let task_idx = row_index(&app, "Task 7");
    let coder_idx = row_index(&app, "Builder");
    let task_key = app.visible_rows[task_idx].key.clone();
    let coder_key = app.visible_rows[coder_idx].key.clone();
    app.collapsed_overrides
        .insert(task_key.clone(), ExpansionOverride::Collapsed);
    app.collapsed_overrides
        .insert(coder_key.clone(), ExpansionOverride::Collapsed);

    app.rebuild_tree_view(None);

    assert!(row_index_opt(&app, "Task 7").is_some());
    let task_idx = row_index(&app, "Task 7");
    assert!(!app.is_expanded(task_idx));
    assert!(row_index_opt(&app, "Builder").is_none());
}

#[test]
fn selection_restores_same_key_after_reorder() {
    let mut state = SessionState::new("restore-same-key".to_string());
    state.current_phase = Phase::ImplementationRound(4);
    state.builder.done = vec![3];
    state.builder.current_task = Some(9);
    state.builder.pending = vec![8];
    let mut app = mk_app(state.clone());
    let task_idx = row_index(&app, "Task 9");
    let task_key = app.visible_rows[task_idx].key.clone();
    app.selected = task_idx;
    app.selected_key = Some(task_key.clone());

    state.current_phase = Phase::BuilderRecovery(4);
    state.agent_runs.push(RunRecord {
        id: 77,
        stage: "recovery".to_string(),
        task_id: None,
        round: 4,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Recovery]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    });
    app.state = state;

    app.rebuild_tree_view(None);

    assert_eq!(app.selected_key, Some(task_key));
    assert_eq!(row_label(&app, app.selected), "Task 9");
}

#[test]
fn selection_falls_back_to_nearest_visible_ancestor() {
    let mut state = SessionState::new("fallback-ancestor".to_string());
    state.current_phase = Phase::ReviewRound(1);
    state.builder.current_task = Some(7);
    for (id, stage) in [(1, "coder"), (2, "reviewer")] {
        state.agent_runs.push(RunRecord {
            id,
            stage: stage.to_string(),
            task_id: Some(7),
            round: 1,
            attempt: 1,
            model: stage.to_string(),
            vendor: "test".to_string(),
            window_name: format!("[{stage}]"),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: if stage == "reviewer" {
                RunStatus::Running
            } else {
                RunStatus::Done
            },
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
    }
    let mut app = mk_app(state.clone());
    let reviewer_idx = row_index(&app, "Reviewer");
    let reviewer_key = app.visible_rows[reviewer_idx].key.clone();
    app.selected = reviewer_idx;
    app.selected_key = Some(reviewer_key);

    state.current_phase = Phase::ImplementationRound(1);
    state.agent_runs.retain(|run| run.stage == "coder");
    app.state = state;
    app.rebuild_tree_view(None);

    assert_eq!(row_label(&app, app.selected), "Task 7");
}

#[test]
fn progress_follow_focuses_running_run_at_startup() {
    let mut state = coder_round_state("pf-startup");
    state.agent_runs.push(make_coder_run(10, 1, 1));

    let app = build_progress_follow_app(state, 10);

    let row_node = app.node_for_row(app.selected).expect("selected row exists");
    assert_eq!(
        row_node.run_id.or(row_node.leaf_run_id),
        Some(10),
        "startup with a running run focuses that run's deepest visible row"
    );
    assert!(app.progress_follow_active);
}

#[test]
fn progress_follow_focuses_newer_attempt_after_retry() {
    let mut state = coder_round_state("pf-retry");
    let mut first = make_coder_run(10, 1, 1);
    first.status = RunStatus::Failed;
    first.ended_at = Some(chrono::Utc::now());
    state.agent_runs.push(first);
    state.agent_runs.push(make_coder_run(11, 1, 2));

    let mut app = build_progress_follow_app(state, 10);
    let first_idx = app.selected;
    let first_path = app.visible_rows[first_idx].path.clone();

    // Simulate `start_run_tracking` running for the second attempt: a new
    // run record is already in `agent_runs`, the runtime updates
    // `current_run_id`, rebuilds the tree, and re-enables progress follow.
    app.current_run_id = Some(11);
    app.rebuild_tree_view(None);
    app.enable_progress_follow_and_refocus();

    let row_node = app
        .node_for_row(app.selected)
        .expect("selected row exists after retry");
    assert_eq!(
        row_node.run_id.or(row_node.leaf_run_id),
        Some(11),
        "retry refocuses to the newer attempt's row"
    );
    assert_ne!(
        app.visible_rows[app.selected].path, first_path,
        "focus actually moved to a different row"
    );
    assert!(app.progress_follow_active);
}

#[test]
fn progress_follow_live_summary_refocuses_while_enabled() {
    let mut state = coder_round_state("pf-live-summary-on");
    state.agent_runs.push(make_coder_run(10, 1, 1));

    let mut app = build_progress_follow_app(state, 10);
    let target_idx = app.selected;
    assert!(app.progress_follow_active);

    // Pretend an unrelated tree rebuild parked focus on a different row;
    // a live-summary tick is the next refocus event.
    let alt_idx = if target_idx == 0 {
        app.visible_rows.len() - 1
    } else {
        0
    };
    assert_ne!(alt_idx, target_idx);
    app.selected = alt_idx;
    app.selected_key = app.visible_rows.get(alt_idx).map(|row| row.key.clone());

    app.maybe_refocus_to_progress();

    assert_eq!(
        app.selected, target_idx,
        "live-summary refocus while enabled snaps focus back to the run row"
    );
}

#[test]
fn live_summary_process_polls_even_when_watcher_has_no_event() {
    with_temp_root(|| {
        let session_id = "live-summary-watch-delete";
        let mut state = coder_round_state(session_id);
        state.agent_runs.push(make_coder_run(10, 1, 1));
        let mut app = build_progress_follow_app(state, 10);
        let path = app.live_summary_path_for(&app.state.agent_runs[0]);
        app.live_summary_path = Some(path);
        app.live_summary_cached_text = "old summary".to_string();
        app.live_summary_cached_mtime = Some(std::time::SystemTime::now());
        let (_tx, rx) = std::sync::mpsc::channel();
        app.live_summary_change_rx = Some(rx);

        app.process_live_summary_changes();

        assert_eq!(app.live_summary_cached_text, "");
        assert_eq!(app.live_summary_cached_mtime, None);
    });
}

#[test]
fn progress_follow_disabled_by_manual_focus_movement() {
    let mut state = coder_round_state("pf-manual-up");
    state.agent_runs.push(make_coder_run(10, 1, 1));

    let mut app = build_progress_follow_app(state, 10);
    let target_idx = app.selected;
    assert!(target_idx > 0, "expected a row above the focused run row");
    let follow_tail_before = app.follow_tail;

    // Up arrow at the top of the focused section moves focus instead of
    // scrolling, which is the operator's "I'm browsing now" signal.
    app.scroll_or_move_focus(-1);
    assert_ne!(app.selected, target_idx, "Up moved focus");
    assert!(!app.progress_follow_active, "manual nav opted out");

    // Subsequent live-summary tick must not yank the arrow back.
    let manual_idx = app.selected;
    app.maybe_refocus_to_progress();
    assert_eq!(
        app.selected, manual_idx,
        "live-summary update does not refocus while disabled"
    );

    // follow_tail is owned by `set_follow_tail` and is independent of the
    // progress-follow flag; opting out via Up disengages tail-follow as
    // before, but re-enabling progress follow must not replace that
    // mechanism.
    assert!(!app.follow_tail);
    assert_ne!(
        app.follow_tail, follow_tail_before,
        "Up still disengages tail-follow as before"
    );
}

#[test]
fn progress_follow_disabled_by_explicit_viewport_paging() {
    let mut state = coder_round_state("pf-page-down");
    state.agent_runs.push(make_coder_run(10, 1, 1));

    let mut app = build_progress_follow_app(state, 10);
    assert!(app.progress_follow_active);

    // PageUp / PageDown call `scroll_viewport(_, true)`. The opt-out is
    // tied to the explicit flag, not to whether the viewport actually
    // moved, so calling it directly mirrors the keymap path.
    app.scroll_viewport(1, true);
    assert!(
        !app.progress_follow_active,
        "explicit paging opts out of progress follow"
    );
}

#[test]
fn progress_follow_re_enables_on_phase_transition() {
    with_temp_root(|| {
        let mut state = SessionState::new("pf-phase-reset".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Brainstorm]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let mut app = build_progress_follow_app(state, 1);

        // Operator opts out and disengages tail-follow.
        app.progress_follow_active = false;
        app.set_follow_tail(false);
        assert!(!app.follow_tail);

        // Brainstorm completes and the runtime advances to the next phase.
        if let Some(run) = app.state.agent_runs.iter_mut().find(|r| r.id == 1) {
            run.status = RunStatus::Done;
            run.ended_at = Some(chrono::Utc::now());
        }
        app.current_run_id = None;
        app.transition_to_phase(Phase::SpecReviewRunning).unwrap();

        assert!(
            app.progress_follow_active,
            "phase transition re-enables progress follow"
        );
        assert_eq!(
            row_label(&app, app.selected),
            "Spec Review",
            "focus snaps to the new running stage when no run is active yet"
        );
        assert!(
            app.follow_tail,
            "phase transition keeps re-engaging tail-follow"
        );
    });
}

#[test]
fn progress_follow_re_enables_on_run_launch() {
    let mut state = coder_round_state("pf-run-launch-reset");
    state.agent_runs.push(make_coder_run(10, 1, 1));

    let mut app = build_progress_follow_app(state, 10);

    // Operator manually navigates away.
    app.scroll_or_move_focus(-1);
    let after_manual = app.selected;
    assert!(!app.progress_follow_active);

    // A retry pushes a newer attempt and the runtime calls
    // `enable_progress_follow_and_refocus` after `rebuild_tree_view`.
    app.state.agent_runs.push(make_coder_run(11, 1, 2));
    app.current_run_id = Some(11);
    app.rebuild_tree_view(None);
    app.enable_progress_follow_and_refocus();

    let row_node = app
        .node_for_row(app.selected)
        .expect("selected row exists after run launch");
    assert_eq!(
        row_node.run_id.or(row_node.leaf_run_id),
        Some(11),
        "run launch refocuses to the new run row"
    );
    assert!(
        app.progress_follow_active,
        "run launch re-enables progress follow"
    );
    assert_ne!(
        app.selected, after_manual,
        "focus actually moved off the manually-selected row"
    );
}

#[test]
fn progress_follow_uses_collapsed_ancestor_when_run_row_hidden() {
    let mut state = coder_round_state("pf-collapsed");
    state.agent_runs.push(make_coder_run(10, 1, 1));

    let mut app = build_progress_follow_app(state, 10);

    // Collapse the focused row's parent task so the run row is no longer
    // visible. The next refocus event must land on that nearest visible
    // ancestor instead of trying to expand the tree or losing selection.
    let task_idx = row_index(&app, "Task 1");
    let task_key = app.visible_rows[task_idx].key.clone();
    app.collapsed_overrides
        .insert(task_key.clone(), ExpansionOverride::Collapsed);
    app.rebuild_visible_rows();

    // Pretend focus drifted before the next refocus tick.
    app.selected = 0;
    app.selected_key = app.visible_rows.first().map(|row| row.key.clone());

    app.maybe_refocus_to_progress();

    assert_eq!(
        row_label(&app, app.selected),
        "Task 1",
        "collapsed ancestor of the run row receives focus"
    );
}

#[test]
fn progress_focus_key_targets_idle_top_level_stage_when_no_run() {
    let state = SessionState::new("pf-idle-stage".to_string());
    let mut app = mk_app(state);
    app.current_run_id = None;
    app.rebuild_tree_view(None);

    // Default phase `IdeaInput` puts the Idea row in `WaitingUser`. The
    // helper falls back to that current pipeline position when no run is
    // active. Manual selection on a different row is what leaves focus
    // alone, not this resolver.
    let target = app
        .progress_focus_key()
        .expect("idle pipeline has a target");
    let row = app
        .visible_rows
        .iter()
        .find(|row| row.key == target)
        .expect("target row visible");
    assert_eq!(
        app.node_for_row(
            app.visible_rows
                .iter()
                .position(|r| r.key == row.key)
                .unwrap()
        )
        .map(|n| n.label.as_str()),
        Some("Idea")
    );
}

#[test]
fn progress_focus_key_is_none_when_pipeline_terminal() {
    let mut state = SessionState::new("pf-terminal".to_string());
    state.current_phase = Phase::Done;
    let mut app = mk_app(state);
    app.current_run_id = None;
    app.rebuild_tree_view(None);

    // After `Phase::Done` every stage rolls up to `Done`, so there's no
    // live stage to follow and the helper preserves whatever the operator
    // had selected.
    assert!(app.progress_focus_key().is_none());
}

#[test]
fn progress_focus_key_skips_finalized_run_when_id_still_set() {
    with_temp_root(|| {
        // Regression: `go_back` finalizes the active run before
        // `transition_to_phase` clears `current_run_id`. The refocus inside
        // `transition_to_phase` would otherwise see a non-running run id
        // and pin focus on the just-aborted row instead of the rewound
        // stage. The status check belongs in `progress_focus_key` so any
        // future call site that finalizes-then-transitions stays correct.
        let mut state = coder_round_state("pf-finalized-stale");
        state.agent_runs.push(make_coder_run(10, 1, 1));

        let mut app = build_progress_follow_app(state, 10);
        let coder_node = app
            .node_for_row(app.selected)
            .expect("baseline coder row exists");
        assert_eq!(
            coder_node.run_id.or(coder_node.leaf_run_id),
            Some(10),
            "baseline: progress focus lands on the running coder row"
        );

        // Mirror the first half of `go_back`: finalize the run while
        // `current_run_id` still points at it.
        app.finalize_run_record(10, false, Some("aborted by user".to_string()));
        assert_eq!(app.state.agent_runs[0].status, RunStatus::Failed);
        assert_eq!(
            app.current_run_id,
            Some(10),
            "stale id intentionally retained to mirror the rewind window"
        );

        // The next refocus event must skip the just-aborted run.
        let target = app
            .progress_focus_key()
            .expect("falls back to the active top-level stage");
        let target_idx = app
            .visible_rows
            .iter()
            .position(|row| row.key == target)
            .expect("target row visible");
        let target_node = app.node_for_row(target_idx).expect("target node");
        assert_ne!(
            target_node.run_id.or(target_node.leaf_run_id),
            Some(10),
            "stale current_run_id pointing at a non-Running run must not steer focus"
        );
    });
}

#[test]
fn progress_follow_back_during_running_agent_focuses_new_stage() {
    // End-to-end regression: invoking `go_back` while a run is active
    // must leave focus on the rewound stage, not on the just-aborted
    // run row that lingers in the tree because `agent_runs` history
    // is preserved across rewinds.
    with_temp_root(|| {
        let session_id = "pf-go-back-running";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::SpecReviewRunning;
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Brainstorm]".to_string(),
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
            id: 2,
            stage: "spec-review".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Spec Review 1]".to_string(),
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
        app.current_run_id = Some(2);
        app.rebuild_tree_view(None);

        app.go_back();

        assert_eq!(app.state.current_phase, Phase::BrainstormRunning);
        assert_eq!(app.current_run_id, None, "go_back clears current_run_id");
        assert_eq!(
            app.state.agent_runs[1].status,
            RunStatus::Failed,
            "spec-review run was finalized as failed"
        );
        let row_node = app.node_for_row(app.selected).expect("selected row");
        assert_ne!(
            row_node.run_id.or(row_node.leaf_run_id),
            Some(2),
            "rewind must not refocus to the just-aborted spec-review run"
        );
    });
}

#[test]
fn up_at_top_of_section_moves_focus_to_previous_row() {
    let mut app = mk_app(mk_state_with_runs());
    let sr_idx = row_index(&app, "Spec Review");
    app.selected = sr_idx;
    app.scroll_or_move_focus(-1);
    assert!(app.selected < sr_idx);
}

#[test]
fn space_binding_does_not_affect_input_mode() {
    let mut app = mk_app(mk_state_with_runs());
    app.input_mode = true;
    let before = app.collapsed_overrides.clone();
    // Directly test the guard: toggle_expand_focused shouldn't be reached via
    // input-mode keys. Sanity: toggle itself still works outside input mode.
    app.input_mode = false;
    app.selected = row_index(&app, "Brainstorm");
    app.toggle_expand_focused();
    assert_ne!(app.collapsed_overrides, before);
}

#[test]
fn down_boundary_handoff_moves_to_next_visible_row_even_when_collapsed() {
    let mut app = mk_app(SessionState::new("boundary-visible-row".to_string()));
    app.nodes = vec![Node {
        label: "Root".to_string(),
        kind: crate::state::NodeKind::Stage,
        status: crate::state::NodeStatus::Running,
        summary: String::new(),
        children: vec![
            Node {
                label: "Collapsed Task".to_string(),
                kind: crate::state::NodeKind::Task,
                status: crate::state::NodeStatus::Done,
                summary: String::new(),
                children: Vec::new(),
                run_id: None,
                leaf_run_id: Some(11),
            },
            Node {
                label: "Expanded Task".to_string(),
                kind: crate::state::NodeKind::Task,
                status: crate::state::NodeStatus::Done,
                summary: String::new(),
                children: Vec::new(),
                run_id: None,
                leaf_run_id: Some(12),
            },
        ],
        run_id: None,
        leaf_run_id: None,
    }];
    app.rebuild_visible_rows();
    let expanded_idx = row_index(&app, "Expanded Task");
    let expanded_key = app.visible_rows[expanded_idx].key.clone();
    app.collapsed_overrides
        .insert(expanded_key, ExpansionOverride::Expanded);
    app.rebuild_visible_rows();

    app.selected = row_index(&app, "Root");
    app.scroll_or_move_focus(1);

    assert_eq!(row_label(&app, app.selected), "Collapsed Task");
}

#[test]
fn space_does_not_toggle_pending_rows() {
    let mut state = SessionState::new("pending-toggle".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    state.builder.pending = vec![4];
    let mut app = mk_app(state);
    let pending_idx = row_index(&app, "Task 4");
    app.selected = pending_idx;

    app.toggle_expand_focused();

    assert!(app.collapsed_overrides.is_empty());
    assert!(!app.is_expanded(pending_idx));
}

#[test]
fn space_collapse_override_collapses_active_path_row() {
    let mut state = SessionState::new("active-space".to_string());
    state.current_phase = Phase::ImplementationRound(1);
    state.builder.current_task = Some(7);
    state.agent_runs.push(RunRecord {
        id: 88,
        stage: "coder".to_string(),
        task_id: Some(7),
        round: 1,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Builder]".to_string(),
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
    let coder_idx = row_index(&app, "Builder");
    let coder_key = app.visible_rows[coder_idx].key.clone();
    app.selected = coder_idx;

    app.toggle_expand_focused();

    assert_eq!(
        app.collapsed_overrides.get(&coder_key),
        Some(&ExpansionOverride::Collapsed)
    );
    let coder_idx = row_index(&app, "Builder");
    assert!(!app.is_expanded(coder_idx));
}

#[test]
fn enter_does_not_toggle_expansion_for_focused_row() {
    let mut app = mk_app(mk_state_with_runs());
    let brainstorm_idx = row_index(&app, "Brainstorm");
    let before = app.collapsed_overrides.clone();
    app.selected = brainstorm_idx;

    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));

    assert_eq!(app.collapsed_overrides, before);
    assert!(app.is_expanded(brainstorm_idx));
}

#[test]
fn builder_task_row_can_be_focused_and_expanded_to_transcript_descendant() {
    let mut state = SessionState::new("builder-drilldown".to_string());
    state.current_phase = Phase::ImplementationRound(2);
    state.builder.done = vec![7];
    state.builder.current_task = Some(8);
    state.agent_runs.push(RunRecord {
        id: 71,
        stage: "coder".to_string(),
        task_id: Some(7),
        round: 1,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Builder 7]".to_string(),
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
        id: 81,
        stage: "coder".to_string(),
        task_id: Some(8),
        round: 2,
        attempt: 1,
        model: "gpt-5".to_string(),
        vendor: "openai".to_string(),
        window_name: "[Builder 8]".to_string(),
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
    let task_idx = row_index(&app, "Task 7");
    app.selected = task_idx;

    app.toggle_expand_focused();

    assert_eq!(row_label(&app, app.selected), "Task 7");
    assert!(row_index_opt(&app, "Builder").is_some());
}

#[test]
fn repeated_attempt_labels_keep_independent_expansion_state() {
    let mut state = SessionState::new("attempt-identity".to_string());
    state.current_phase = Phase::ReviewRound(1);
    state.builder.current_task = Some(5);
    for (id, stage, attempt, status) in [
        (41, "coder", 1, RunStatus::Failed),
        (42, "coder", 2, RunStatus::Done),
        (43, "reviewer", 1, RunStatus::Failed),
        (44, "reviewer", 2, RunStatus::Running),
    ] {
        state.agent_runs.push(RunRecord {
            id,
            stage: stage.to_string(),
            task_id: Some(5),
            round: 1,
            attempt,
            model: "gpt-5".to_string(),
            vendor: "openai".to_string(),
            window_name: format!("[{stage}]"),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
    }
    let mut app = mk_app(state);
    let attempt_rows = app
        .visible_rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            node_at_path(&app.nodes, &row.path).is_some_and(|node| node.label == "Attempt 1")
        })
        .map(|(index, row)| (index, row.key.clone()))
        .collect::<Vec<_>>();
    assert_eq!(attempt_rows.len(), 2);
    assert_ne!(attempt_rows[0].1, attempt_rows[1].1);

    app.selected = attempt_rows[0].0;
    app.toggle_expand_focused();

    assert_eq!(
        app.collapsed_overrides.get(&attempt_rows[0].1),
        Some(&ExpansionOverride::Collapsed)
    );
    assert!(!app.collapsed_overrides.contains_key(&attempt_rows[1].1));
}

#[test]
fn on_frame_drawn_advances_spinner_tick_without_agent_changes() {
    let mut app = idle_app(SessionState::new("on-frame-drawn".to_string()));
    let before = app.spinner_tick;

    for _ in 0..97 {
        app.on_frame_drawn();
    }

    assert_eq!(app.spinner_tick, before.wrapping_add(97));
    assert_eq!(app.agent_content_hash, 0);
    assert!(app.agent_last_change.is_none());
}

#[test]
fn event_poll_duration_uses_fast_cadence_only_for_visible_live_summary_spinner() {
    let mut app = idle_app(SessionState::new("frame-poll-duration".to_string()));

    app.live_summary_spinner_visible = false;
    assert_eq!(app.event_poll_duration(), Duration::from_millis(250));

    app.live_summary_spinner_visible = true;
    assert_eq!(app.event_poll_duration(), Duration::from_millis(50));
}

#[test]
fn picker_created_startup_draws_before_auto_launch() {
    with_temp_root(|| {
        let session_id = "picker-created-first-frame";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.idea_text = Some("Ship the picker handoff".to_string());
        state.save().expect("save session");

        let mut app = App::new_with_startup_origin(
            SessionState::load(session_id).expect("load session"),
            AppStartupOrigin::PickerCreated,
        );
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            10,
            10,
            1,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: Some("# Spec\n".to_string()),
                    launch_error: None,
                }]),
            },
        )));

        app.maybe_auto_launch();
        assert!(
            app.state.agent_runs.is_empty(),
            "picker-created startup must wait for the first visible frame"
        );

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw");

        assert_eq!(app.state.current_phase, Phase::BrainstormRunning);
        assert!(
            app.state.agent_runs.is_empty(),
            "successful draw alone must not backdoor a launch"
        );
    });
}

#[test]
fn update_agent_progress_reloads_persisted_interactive_agent_text() {
    with_temp_root(|| {
        let session_id = "interactive-output-reload";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run.clone());
        state.save().expect("save state");
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        let msg = Message {
            ts: chrono::Utc::now(),
            run_id: 7,
            kind: MessageKind::AgentText,
            sender: crate::state::MessageSender::Agent {
                model: run.model,
                vendor: run.vendor,
            },
            text: "question for operator".to_string(),
        };
        SessionState::load(session_id)
            .expect("load state")
            .append_message(&msg)
            .expect("append message");

        app.update_agent_progress();

        assert!(app.messages.iter().any(|message| {
            message.run_id == 7
                && message.kind == MessageKind::AgentText
                && message.text == "question for operator"
        }));
    });
}

#[test]
fn update_agent_progress_reloads_in_place_message_text_changes() {
    with_temp_root(|| {
        let session_id = "interactive-output-upsert-reload";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run.clone());
        state.save().expect("save state");
        let mut app = idle_app(state.clone());
        app.current_run_id = Some(7);

        let ts = chrono::Utc::now();
        let msg = Message {
            ts,
            run_id: 7,
            kind: MessageKind::AgentThought,
            sender: crate::state::MessageSender::Agent {
                model: run.model,
                vendor: run.vendor,
            },
            text: "partial".to_string(),
        };
        state.append_message(&msg).expect("append message");
        app.update_agent_progress();
        assert!(app.messages.iter().any(|message| message.text == "partial"));

        state
            .update_message_text(ts, "partial plus more")
            .expect("update message");
        app.update_agent_progress();

        assert!(app.messages.iter().any(|message| {
            message.run_id == 7
                && message.kind == MessageKind::AgentThought
                && message.text == "partial plus more"
        }));
    });
}

#[test]
fn app_new_rebuilds_failed_models_without_force_retry_runs() {
    with_temp_root(|| {
        let session_id = "rebuild-failed-models";
        let mut state = SessionState::new(session_id.to_string());
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(7),
            round: 3,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Builder r3]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.agent_runs.push(RunRecord {
            id: 2,
            stage: "coder".to_string(),
            task_id: Some(7),
            round: 3,
            attempt: 2,
            model: "gemini-2.5-pro".to_string(),
            vendor: "gemini".to_string(),
            window_name: "[Builder r3]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("artifact_missing".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.agent_runs.push(RunRecord {
            id: 3,
            stage: "coder".to_string(),
            task_id: Some(7),
            round: 3,
            attempt: 3,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Builder r3]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("user_forced_retry".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.save().expect("save session");

        let app = App::new(SessionState::load(session_id).expect("load session"));

        let key = ("coder".to_string(), Some(7), 3);
        let failed = app
            .failed_models
            .get(&key)
            .expect("expected failed model set");
        assert!(failed.contains(&(selection::VendorKind::Claude, "claude-sonnet".to_string())));
        assert!(failed.contains(&(selection::VendorKind::Gemini, "gemini-2.5-pro".to_string())));
        assert!(!failed.contains(&(selection::VendorKind::Codex, "gpt-5".to_string())));
        assert!(app.current_run_id.is_none());
    });
}

#[test]
fn non_coder_missing_stamp_warns_and_still_retries_after_timeout() {
    with_temp_root(|| {
        let session_id = "planning-missing-stamp-warning";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::PlanningRunning;
        let mut app = idle_app(state);
        app.models = vec![
            ranked_model(selection::VendorKind::Codex, "gpt-5", 1, 10, 10),
            ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 2, 10, 10),
        ];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![
                    TestLaunchOutcome {
                        exit_code: 1,
                        artifact_contents: None,
                        launch_error: None,
                    },
                    TestLaunchOutcome {
                        exit_code: 1,
                        artifact_contents: None,
                        launch_error: None,
                    },
                ]),
            },
        )));

        app.launch_planning();
        let first_id = app.current_run_id.expect("first planning run id");
        let first = app
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == first_id)
            .cloned()
            .expect("first run");
        let _ = std::fs::remove_file(app.finish_stamp_path_for(&first));
        let _ = std::fs::remove_file(app.live_summary_path_for(&first));

        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));
        app.poll_agent_run();

        let warn = app
            .messages
            .iter()
            .find(|message| {
                message.run_id == first.id
                    && message.kind == MessageKind::SummaryWarn
                    && message.text.contains("finish_stamp_missing")
            })
            .expect("missing-stamp warning");
        assert!(warn.text.contains("planning"));
        assert!(
            app.state
                .agent_runs
                .iter()
                .any(|run| run.stage == "planning"
                    && run.attempt == 2
                    && run.status == RunStatus::Running)
        );
    });
}

#[test]
fn non_builder_retry_exhaustion_still_blocks() {
    with_temp_root(|| {
        let mut state = SessionState::new("non-builder-retry".to_string());
        state.current_phase = Phase::PlanningRunning;
        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Claude,
            "claude-sonnet",
            1,
            10,
            10,
        )];
        let failed = RunRecord {
            id: 11,
            stage: "planning".to_string(),
            task_id: None,
            round: 1,
            attempt: 3,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Planning]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        };
        let handled = app.maybe_auto_retry(&failed);
        assert!(handled);
        assert_eq!(app.state.current_phase, Phase::BlockedNeedsUser);
        assert!(!matches!(
            app.state.current_phase,
            Phase::BuilderRecovery(_)
        ));
    });
}

#[test]
fn app_new_rebuild_failed_models_skips_builder_failures_before_retry_reset_cutoff() {
    with_temp_root(|| {
        let session_id = "failed-model-retry-reset";
        let mut state = SessionState::new(session_id.to_string());
        state.builder.retry_reset_run_id_cutoff = Some(10);
        state.agent_runs.push(RunRecord {
            id: 9,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Builder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.agent_runs.push(RunRecord {
            id: 11,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 2,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Builder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.save().expect("save");
        let app = App::new(SessionState::load(session_id).expect("load"));
        let key = ("coder".to_string(), Some(1), 1);
        let failed = app.failed_models.get(&key).expect("failed set");
        assert_eq!(failed.len(), 1);
        assert!(failed.contains(&(selection::VendorKind::Codex, "gpt-5".to_string())));
        assert!(!failed.contains(&(selection::VendorKind::Claude, "claude-sonnet".to_string())));
    });
}

#[test]
fn go_back_from_impl_round_one_on_skip_path_returns_to_brainstorm() {
    with_temp_root(|| {
        let session_id = "skip-back-nav";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.skip_to_impl_rationale = Some("trivial change".to_string());
        // Seed a non-default BuilderState so we can detect that the skip branch
        // preserves it (unlike the normal-path branch, which resets).
        state.builder.pending = vec![1];
        state.builder.task_titles.insert(1, "t".to_string());

        let mut app = idle_app(state);
        app.go_back();

        assert_eq!(app.state.current_phase, Phase::BrainstormRunning);
        // Skip-path back-nav should not clobber BuilderState the way the
        // ShardingRunning branch does.
        assert_eq!(app.state.builder.pending, vec![1]);
    });
}

#[test]
fn go_back_from_impl_round_one_without_skip_resets_to_sharding() {
    with_temp_root(|| {
        let session_id = "normal-back-nav";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.skip_to_impl_rationale = None;
        state.builder.pending = vec![1];

        let mut app = idle_app(state);
        app.go_back();

        assert_eq!(app.state.current_phase, Phase::ShardingRunning);
        assert!(app.state.builder.pending.is_empty());
    });
}

#[test]
fn skip_modal_decline_enters_spec_review() {
    with_temp_root(|| {
        let session_id = "skip-decline";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::SkipToImplPending;
        state.skip_to_impl_rationale = Some("rationale".to_string());

        let mut app = idle_app(state);
        app.decline_skip_to_implementation()
            .expect("decline should succeed");

        assert_eq!(app.state.current_phase, Phase::SpecReviewRunning);
        assert!(app.state.skip_to_impl_rationale.is_none());
    });
}

#[test]
fn skip_modal_accept_generates_artifacts_and_enters_impl_round_one() {
    with_temp_root(|| {
        let session_id = "skip-accept";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::SkipToImplPending;
        state.skip_to_impl_rationale = Some("trivial".to_string());

        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("mk artifacts dir");
        std::fs::write(artifacts.join("spec.md"), "# Spec\n\nA trivial feature.\n")
            .expect("write spec");

        let mut app = idle_app(state);
        app.accept_skip_to_implementation()
            .expect("accept should succeed");

        assert_eq!(app.state.current_phase, Phase::ImplementationRound(1));
        assert!(artifacts.join("plan.md").exists());
        assert!(artifacts.join("tasks.toml").exists());
        assert!(!artifacts.join("implementation.json").exists());
        assert_eq!(app.state.builder.pending, vec![1]);
        assert!(app.state.builder.current_task.is_none());
    });
}

#[test]
fn skip_modal_accept_nothing_to_do_bypasses_final_validation_and_finishes() {
    with_temp_root(|| {
        let session_id = "skip-accept-nothing-to-do";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::SkipToImplPending;
        state.skip_to_impl_rationale = Some("already complete".to_string());
        state.skip_to_impl_kind = Some(crate::artifacts::SkipToImplKind::NothingToDo);

        let mut app = idle_app(state);
        app.accept_skip_to_implementation()
            .expect("accept should succeed");

        assert_eq!(app.state.current_phase, Phase::Done);
        assert_eq!(app.state.validation_attempts, 0);
    });
}

#[test]
fn enter_builder_recovery_sets_interactive_for_human_blocked() {
    with_temp_root(|| {
        let mut state = SessionState::new("recovery-interactive".to_string());
        state.current_phase = Phase::ReviewRound(1);
        state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: Some(1),
            status: PipelineItemStatus::Running,
            title: Some("Task 1".to_string()),
            mode: None,
            trigger: None,
            interactive: None,
        });
        state.builder.sync_legacy_queue_views();
        let session_dir = session_state::session_dir("recovery-interactive");
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        let mut app = idle_app(state);
        app.enter_builder_recovery(1, Some(1), Some("needs human".to_string()), "human_blocked");

        // The recovery pipeline item should be interactive=true for human_blocked
        let recovery_items: Vec<_> = app
            .state
            .builder
            .pipeline_items
            .iter()
            .filter(|i| i.stage == "recovery")
            .collect();
        assert_eq!(recovery_items.len(), 1);
        assert_eq!(recovery_items[0].interactive, Some(true));
        assert_eq!(recovery_items[0].trigger.as_deref(), Some("human_blocked"));
        assert_eq!(app.state.current_phase, Phase::BuilderRecovery(1));
    });
}

#[test]
fn enter_builder_recovery_sets_non_interactive_for_agent_pivot() {
    with_temp_root(|| {
        let mut state = SessionState::new("recovery-non-interactive".to_string());
        state.current_phase = Phase::ReviewRound(2);
        let session_dir = session_state::session_dir("recovery-non-interactive");
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 2\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

        let mut app = idle_app(state);
        app.enter_builder_recovery(2, None, None, "agent_pivot");

        let recovery_items: Vec<_> = app
            .state
            .builder
            .pipeline_items
            .iter()
            .filter(|i| i.stage == "recovery")
            .collect();
        assert_eq!(recovery_items.len(), 1);
        assert_eq!(recovery_items[0].interactive, Some(false));
        assert_eq!(recovery_items[0].trigger.as_deref(), Some("agent_pivot"));
    });
}

#[test]
fn pending_guard_reset_finalizes_as_forbidden_head_advance() {
    with_temp_root(|| {
        let session_id = "pending-guard-reset";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::GitGuardPending;
        let run = make_brainstorm_run(10);
        state.agent_runs.push(run.clone());
        state.pending_guard_decision = Some(PendingGuardDecision {
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            run_id: 10,
            captured_head: "abc123".to_string(),
            current_head: "def456".to_string(),
            warnings: vec!["some guard warning".to_string()],
        });
        let mut app = mk_app(state);

        app.accept_guard_reset().expect("accept_guard_reset ok");

        assert!(
            app.state.pending_guard_decision.is_none(),
            "pending_guard_decision must be cleared after reset"
        );
        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 10)
            .expect("run");
        assert_eq!(finalized.status, RunStatus::Failed);
        assert_eq!(
            finalized.error.as_deref(),
            Some("forbidden_head_advance"),
            "run error must be forbidden_head_advance"
        );
        let warned = app
            .messages
            .iter()
            .any(|m| m.kind == MessageKind::SummaryWarn && m.text.contains("some guard warning"));
        assert!(warned, "guard warning must be replayed as SummaryWarn");
    });
}

#[test]
fn pending_guard_keep_preserves_normal_semantics() {
    with_temp_root(|| {
        let session_id = "pending-guard-keep";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::GitGuardPending;
        let run = make_brainstorm_run(20);
        state.agent_runs.push(run.clone());
        state.pending_guard_decision = Some(PendingGuardDecision {
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            run_id: 20,
            captured_head: "abc123".to_string(),
            current_head: "def456".to_string(),
            warnings: vec!["kept-warning".to_string()],
        });
        let mut app = mk_app(state);
        std::fs::write(session_dir.join("artifacts").join("spec.md"), "# Spec\n")
            .expect("write spec");

        app.accept_guard_keep().expect("accept_guard_keep ok");

        assert!(
            app.state.pending_guard_decision.is_none(),
            "pending_guard_decision must be cleared after keep"
        );
        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 20)
            .expect("run");
        assert_eq!(
            finalized.status,
            RunStatus::Done,
            "run must succeed on keep"
        );
        let kept_warn = app.messages.iter().any(|m| {
            m.kind == MessageKind::SummaryWarn
                && m.text.contains("operator kept unauthorized commit")
        });
        assert!(kept_warn, "operator-kept warning must be emitted");
        assert_ne!(
            app.state.current_phase,
            Phase::GitGuardPending,
            "phase must advance after keep"
        );
    });
}

#[test]
fn pending_guard_modal_reset_key_dispatches_to_reset() {
    with_temp_root(|| {
        let mut app = mk_app(make_pending_guard_state("pending-guard-key-reset", 30));

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert!(app.state.pending_guard_decision.is_none());
        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 30)
            .expect("run");
        assert_eq!(finalized.status, RunStatus::Failed);
        assert_eq!(finalized.error.as_deref(), Some("forbidden_head_advance"));
    });
}

#[test]
fn pending_guard_modal_keep_key_dispatches_to_keep() {
    with_temp_root(|| {
        let session_id = "pending-guard-key-keep";
        let session_dir = session_state::session_dir(session_id);
        std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
        std::fs::write(session_dir.join("artifacts").join("spec.md"), "# Spec\n")
            .expect("write spec");
        let mut app = mk_app(make_pending_guard_state(session_id, 31));

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Char('K')));

        assert!(!should_quit);
        assert!(app.state.pending_guard_decision.is_none());
        let finalized = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == 31)
            .expect("run");
        assert_eq!(finalized.status, RunStatus::Done);
        assert_ne!(app.state.current_phase, Phase::GitGuardPending);
    });
}

#[test]
fn palette_texts_command_toggles_persisted_noninteractive_text_visibility() {
    with_temp_root(|| {
        let session_id = "palette-texts-toggle";
        let state = SessionState::new(session_id.to_string());
        state.save().expect("save initial state");
        let mut app = idle_app(state);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "text".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        assert!(app.state.show_noninteractive_texts);
        let saved = SessionState::load(session_id).expect("load saved state");
        assert!(saved.show_noninteractive_texts);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "messages".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        assert!(!app.state.show_noninteractive_texts);
        let saved = SessionState::load(session_id).expect("load saved state");
        assert!(!saved.show_noninteractive_texts);
    });
}

#[test]
fn palette_verbose_command_toggles_persisted_thinking_visibility() {
    with_temp_root(|| {
        let session_id = "palette-verbose-toggle";
        let state = SessionState::new(session_id.to_string());
        state.save().expect("save initial state");
        let mut app = idle_app(state);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "verbose".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        assert!(app.state.show_thinking_texts);
        let saved = SessionState::load(session_id).expect("load saved state");
        assert!(saved.show_thinking_texts);
    });
}

#[test]
fn interactive_palette_command_closes_after_execution() {
    with_temp_root(|| {
        let session_id = "interactive-palette-command-close";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run);
        state.save().expect("save initial state");
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "verbose".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        assert!(app.state.show_thinking_texts);
        assert!(
            !app.palette.open,
            "executed commands should close the : box"
        );
        assert!(app.palette.buffer.is_empty());
    });
}

#[test]
fn interactive_exit_is_handled_locally_without_quitting_tui() {
    with_temp_root(|| {
        let mut state = SessionState::new("interactive-exit-local".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "/exit".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert_eq!(app.current_run_id, Some(7));
        assert!(!app.input_mode);
        assert!(!app.palette.open);
        assert!(app.input_buffer.is_empty());
    });
}

#[test]
fn agent_exit_suggestion_opens_requests_modal() {
    with_temp_root(|| {
        let (app, _window_name) = app_waiting_on_agent_exit("agent-exit-modal");

        assert_eq!(app.active_modal(), Some(ModalKind::InteractiveExitPrompt));
    });
}

#[test]
fn agent_exit_suggestion_enter_exits_interactive_run() {
    with_temp_root(|| {
        let (mut app, window_name) = app_waiting_on_agent_exit("agent-exit-enter");

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert!(!crate::runner::run_label_is_waiting_for_input(&window_name));
        assert_eq!(app.active_modal(), None);
    });
}

#[test]
fn agent_exit_suggestion_typing_starts_request_input() {
    with_temp_root(|| {
        let (mut app, window_name) = app_waiting_on_agent_exit("agent-exit-type");

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Char('f')));

        assert!(!should_quit);
        assert!(app.input_mode);
        assert_eq!(app.input_buffer, "f");
        assert!(crate::runner::run_label_is_waiting_for_input(&window_name));
        assert_eq!(app.active_modal(), None);
    });
}

#[test]
fn idea_input_leading_colon_enters_command_mode() {
    with_temp_root(|| {
        let mut state = SessionState::new("idea-leading-colon".to_string());
        state.current_phase = Phase::IdeaInput;
        let mut app = idle_app(state);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));

        assert!(app.palette.open);
        assert!(app.palette.buffer.is_empty());
        assert_eq!(
            app.command_return_target,
            Some(super::CommandReturnTarget::Idea)
        );
    });
}

#[test]
fn footer_interactive_leading_colon_enters_command_mode() {
    with_temp_root(|| {
        let mut state = SessionState::new("footer-interactive-leading-colon".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        let window_name = run.window_name.clone();
        state.agent_runs.push(run);
        crate::runner::request_run_label_interactive_input_for_test(&window_name);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        assert!(app.palette.open);
        assert!(app.palette.buffer.is_empty());
        assert_eq!(
            app.command_return_target,
            Some(super::CommandReturnTarget::FooterInteractive)
        );
        assert!(!app.input_mode);
    });
}

#[test]
fn leading_colon_from_paste_enters_command_mode() {
    with_temp_root(|| {
        let mut state = SessionState::new("idea-paste-leading-colon".to_string());
        state.current_phase = Phase::IdeaInput;
        let mut app = idle_app(state);
        app.handle_paste(":cheap");

        assert!(app.palette.open);
        assert_eq!(app.palette.buffer, "cheap");
        assert_eq!(
            app.command_return_target,
            Some(super::CommandReturnTarget::Idea)
        );
    });
}

#[test]
fn edit_derived_leading_colon_enters_command_mode() {
    with_temp_root(|| {
        let mut state = SessionState::new("footer-edit-derived-leading-colon".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        let window_name = run.window_name.clone();
        state.agent_runs.push(run);
        crate::runner::request_run_label_interactive_input_for_test(&window_name);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char('c')));
        app.handle_key(key(crossterm::event::KeyCode::Char('h')));
        app.handle_key(key(crossterm::event::KeyCode::Char('e')));
        app.handle_key(key(crossterm::event::KeyCode::Char('a')));
        app.handle_key(key(crossterm::event::KeyCode::Char('p')));
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Home,
            crossterm::event::KeyModifiers::NONE,
        ));
        app.handle_key(key(crossterm::event::KeyCode::Char(':')));

        assert!(app.palette.open);
        assert_eq!(app.palette.buffer, "cheap");
        assert_eq!(
            app.command_return_target,
            Some(super::CommandReturnTarget::FooterInteractive)
        );
    });
}

#[test]
fn idea_input_treats_q_as_text_before_global_quit() {
    with_temp_root(|| {
        let mut state = SessionState::new("idea-input-q-priority".to_string());
        state.current_phase = Phase::IdeaInput;
        let mut app = idle_app(state);

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Char('q')));

        assert!(!should_quit, "q should be consumed by the idea input box");
        assert!(app.input_mode, "typing should focus the input box");
        assert_eq!(app.input_buffer, "q");
    });
}

#[test]
fn command_mode_esc_restores_split_interactive_input() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-command-esc-restore".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        let window_name = run.window_name.clone();
        state.agent_runs.push(run);
        crate::runner::request_run_label_interactive_input_for_test(&window_name);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);
        app.open_split_target(super::split::SplitTarget::Run(7));
        app.input_mode = true;

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        assert!(app.palette.open);
        assert_eq!(
            app.command_return_target,
            Some(super::CommandReturnTarget::SplitInteractive)
        );

        app.handle_key(key(crossterm::event::KeyCode::Esc));

        assert!(!app.palette.open);
        assert!(app.input_mode);
    });
}

#[test]
fn command_mode_backspace_on_empty_buffer_restores_footer_input() {
    with_temp_root(|| {
        let mut state = SessionState::new("footer-command-backspace-restore".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        let window_name = run.window_name.clone();
        state.agent_runs.push(run);
        crate::runner::request_run_label_interactive_input_for_test(&window_name);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        assert!(app.palette.open);
        assert!(app.palette.buffer.is_empty());

        app.handle_key(key(crossterm::event::KeyCode::Backspace));

        assert!(!app.palette.open);
        assert!(app.input_mode);
        assert!(app.input_buffer.is_empty());
    });
}

#[test]
fn unknown_command_in_waiting_interactive_mode_is_sent_as_user_input() {
    with_temp_root(|| {
        let mut state = SessionState::new("unknown-command-waiting".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        let window_name = run.window_name.clone();
        state.agent_runs.push(run);
        crate::runner::request_run_label_interactive_input_for_test(&window_name);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "unknown-cmd".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(
            app.messages
                .iter()
                .any(|m| { m.kind == MessageKind::UserInput && m.text == "unknown-cmd" })
        );
    });
}

#[test]
fn interrupt_command_interrupts_active_interactive_turn_and_echoes_user_input() {
    with_temp_root(|| {
        let mut state = SessionState::new("interrupt-command-active".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        run.window_name = "[Interrupt Active]".to_string();
        let window_name = run.window_name.clone();
        state.agent_runs.push(run);
        crate::runner::request_run_label_active_for_test(&window_name);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "interrupt please stop and do this instead".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert!(app.messages.iter().any(|message| {
            message.kind == MessageKind::UserInput
                && message.text == "please stop and do this instead"
        }));
        assert!(!crate::runner::run_label_is_waiting_for_input(&window_name));
        crate::runner::shutdown_all_runs();
    });
}

#[test]
fn unknown_command_outside_waiting_mode_sets_status_and_is_not_persisted() {
    with_temp_root(|| {
        let mut state = SessionState::new("unknown-command-not-waiting".to_string());
        state.current_phase = Phase::IdeaInput;
        let mut app = idle_app(state);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "unknown-cmd".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(
            !app.messages
                .iter()
                .any(|m| m.kind == MessageKind::UserInput)
        );
        let status = app.status_line.borrow().render().expect("status flash");
        assert!(
            status
                .to_string()
                .contains("palette: unknown command \"unknown-cmd\"")
        );
    });
}

#[test]
fn interactive_run_arrows_navigate_when_input_is_not_active() {
    with_temp_root(|| {
        let mut state = SessionState::new("interactive-run-arrows".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);
        app.input_mode = false;
        let start = app.selected;

        app.handle_key(key(crossterm::event::KeyCode::Down));

        assert!(
            app.selected > start,
            "Down should move focus while the textbox is inactive"
        );
        assert!(!app.input_mode);
    });
}

#[test]
fn pending_guard_modal_ctrl_c_stops_running_agent_without_quitting() {
    with_temp_root(|| {
        let mut app = mk_app(make_pending_guard_state("pending-guard-key-quit", 32));

        let ctrl_c = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        );
        assert!(!app.handle_key(ctrl_c));
        assert!(app.state.pending_guard_decision.is_some());
        let events_path = session_state::session_dir(&app.state.session_id).join("events.toml");
        let events = std::fs::read_to_string(events_path).expect("events log");
        assert!(
            events.contains("agent_stopped_by_user: run_id=32"),
            "Ctrl+C should always route through stop_running_agent while a run is active"
        );
    });
}

#[test]
fn idle_ctrl_c_quits_when_no_agent_is_running() {
    with_temp_root(|| {
        let mut app = idle_app(SessionState::new("idle-ctrl-c-quits".to_string()));
        let ctrl_c = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        );

        assert!(app.handle_key(ctrl_c));
    });
}

#[test]
fn paused_review_modal_ctrl_c_quits_without_running_agent() {
    with_temp_root(|| {
        let mut state = SessionState::new("paused-modal-ctrl-c-quits".to_string());
        state.current_phase = Phase::SpecReviewPaused;
        let mut app = idle_app(state);
        let ctrl_c = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        );

        assert!(app.handle_key(ctrl_c));
    });
}

#[test]
fn pending_guard_modal_q_still_follows_quit_path() {
    with_temp_root(|| {
        let mut app = mk_app(make_pending_guard_state("pending-guard-key-q-quit", 32));

        assert!(app.handle_key(key(crossterm::event::KeyCode::Char('q'))));
        assert!(app.state.pending_guard_decision.is_some());
    });
}

#[test]
fn pending_guard_modal_escape_matches_q_quit_path() {
    with_temp_root(|| {
        let mut app = mk_app(make_pending_guard_state("pending-guard-key-esc", 34));

        assert!(app.handle_key(key(crossterm::event::KeyCode::Esc)));
        assert!(app.state.pending_guard_decision.is_some());
    });
}

#[test]
fn pending_guard_modal_consumes_unrelated_keys() {
    with_temp_root(|| {
        let mut app = mk_app(make_pending_guard_state("pending-guard-key-consume", 33));
        app.confirm_back = true;

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Char('x')));

        assert!(!should_quit);
        assert!(app.state.pending_guard_decision.is_some());
    });
}

#[test]
fn palette_back_rewinds_without_second_confirmation() {
    with_temp_root(|| {
        let mut app = mk_app(mk_state_with_runs());

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "back".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert_eq!(app.state.current_phase, Phase::BrainstormRunning);
        assert!(!app.confirm_back);
    });
}

#[test]
fn palette_retry_clears_selected_task_attempt_logs_and_relaunches() {
    with_temp_root(|| {
        let session_id = "palette-retry-selected-task";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        state.builder.recovery_trigger_task_id = Some(1);
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Round 1 Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.agent_runs.push(RunRecord {
            id: 2,
            stage: "reviewer".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "gemini-2.5-pro".to_string(),
            vendor: "gemini".to_string(),
            window_name: "[Round 1 Reviewer]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        state.agent_runs.push(RunRecord {
            id: 3,
            stage: "recovery".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Recovery]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        let removed_run = state.agent_runs[0].clone();
        state.save().expect("save");
        state
            .append_message(&crate::state::Message {
                ts: chrono::Utc::now(),
                run_id: 1,
                kind: MessageKind::End,
                sender: crate::state::MessageSender::System,
                text: "attempt 1 failed".to_string(),
            })
            .expect("append message");

        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            10,
            1,
            10,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        write_finish_stamp_for_run(&app, &removed_run, 1, "");
        std::fs::write(app.live_summary_path_for(&removed_run), "old summary").expect("summary");
        app.rebuild_tree_view(None);
        app.selected = row_index(&app, "Task 1");

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "retry".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert_eq!(app.state.agent_runs.len(), 1);
        let fresh = &app.state.agent_runs[0];
        assert_eq!(fresh.stage, "coder");
        assert_eq!(fresh.task_id, Some(1));
        assert_eq!(fresh.attempt, 1);
        assert_eq!(fresh.status, RunStatus::Running);
        assert!(!app.live_summary_path_for(&removed_run).exists());
        let messages = SessionState::load_messages(session_id).expect("messages");
        assert!(
            messages
                .iter()
                .all(|message| message.text != "attempt 1 failed")
        );
    });
}

#[test]
fn palette_retry_is_available_from_builder_loop_focus() {
    with_temp_root(|| {
        let session_id = "palette-retry-loop-focus";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Round 1 Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });

        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            10,
            1,
            10,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));
        app.rebuild_tree_view(None);
        app.selected = row_index(&app, "Loop");

        assert!(
            app.palette_commands()
                .iter()
                .any(|command| command.name == "retry"),
            ":retry should be available when the current builder task is selected by context"
        );

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "retry".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert_eq!(app.state.agent_runs.len(), 1);
        assert_eq!(app.state.agent_runs[0].attempt, 1);
        assert_eq!(app.state.agent_runs[0].status, RunStatus::Running);
    });
}

#[test]
fn palette_retry_clears_brainstorm_attempt_logs_and_relaunches() {
    with_temp_root(|| {
        let session_id = "palette-retry-brainstorm";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.idea_text = Some("draft the spec".to_string());
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Brainstorm] gpt-5".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        let removed_run = state.agent_runs[0].clone();
        state.save().expect("save");
        state
            .append_message(&crate::state::Message {
                ts: chrono::Utc::now(),
                run_id: 1,
                kind: MessageKind::End,
                sender: crate::state::MessageSender::System,
                text: "brainstorm failed".to_string(),
            })
            .expect("append message");

        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            1,
            10,
            10,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: Some("# Spec\n".to_string()),
                    launch_error: None,
                }]),
            },
        )));

        write_finish_stamp_for_run(&app, &removed_run, 1, "");
        std::fs::write(app.live_summary_path_for(&removed_run), "old summary").expect("summary");
        app.rebuild_tree_view(None);
        app.selected = row_index(&app, "Brainstorm");

        assert!(
            app.palette_commands()
                .iter()
                .any(|command| command.name == "retry"),
            ":retry should be available for Brainstorm focus"
        );

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "retry".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert_eq!(app.state.agent_runs.len(), 1);
        let fresh = &app.state.agent_runs[0];
        assert_eq!(fresh.stage, "brainstorm");
        assert_eq!(fresh.attempt, 1);
        assert_eq!(fresh.status, RunStatus::Running);
        assert!(!app.live_summary_path_for(&removed_run).exists());
        let messages = SessionState::load_messages(session_id).expect("messages");
        assert!(
            messages
                .iter()
                .all(|message| message.text != "brainstorm failed")
        );
    });
}

#[test]
fn palette_retry_is_available_from_non_task_stage_focus() {
    with_temp_root(|| {
        let mut state = SessionState::new("palette-retry-stage-focus".to_string());
        for (id, stage) in [
            (1, "brainstorm"),
            (2, "spec-review"),
            (3, "planning"),
            (4, "plan-review"),
            (5, "sharding"),
        ] {
            state.agent_runs.push(RunRecord {
                id,
                stage: stage.to_string(),
                task_id: None,
                round: 1,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: format!("[{stage}]"),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Failed,
                error: Some("exit(1)".to_string()),
                effort: EffortLevel::Normal,
                modes: crate::state::LaunchModes::default(),
                hostname: None,
                mount_device_id: None,
            });
        }
        let mut app = idle_app(state);

        for label in [
            "Brainstorm",
            "Spec Review",
            "Planning",
            "Plan Review",
            "Sharding",
        ] {
            app.selected = row_index(&app, label);
            assert!(
                app.palette_commands()
                    .iter()
                    .any(|command| command.name == "retry"),
                ":retry should be available for {label} focus"
            );
        }
    });
}

#[test]
fn running_palette_shows_stop_retry_and_no_legacy_aliases() {
    with_temp_root(|| {
        let mut state = SessionState::new("running-palette-commands".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let app = mk_app(state);

        let stop = app
            .palette_commands()
            .into_iter()
            .find(|command| command.name == "stop")
            .expect("stop command");
        assert_eq!(stop.help, "Stop the running agent without retry");
        assert!(
            stop.aliases.is_empty(),
            "legacy stop aliases should be removed"
        );

        let retry = app
            .palette_commands()
            .into_iter()
            .find(|command| command.name == "retry")
            .expect("retry command");
        assert_eq!(retry.help, "Stop and retry the running agent");

        let commands = app.palette_commands();
        let names = commands
            .iter()
            .flat_map(|command| {
                std::iter::once(command.name).chain(command.aliases.iter().copied())
            })
            .collect::<Vec<_>>();
        assert!(!names.contains(&"kill"));
        assert!(!names.contains(&"cancel"));
    });
}

#[test]
fn running_palette_retry_stops_current_run_with_retry_marker() {
    with_temp_root(|| {
        let mut state = SessionState::new("running-palette-retry".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let run = make_brainstorm_run(7);
        state.agent_runs.push(run);
        let mut app = mk_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "retry".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }

        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        let events_path = session_state::session_dir(&app.state.session_id).join("events.toml");
        let events = std::fs::read_to_string(events_path).expect("events log");
        assert!(
            events.contains("agent_retry_requested_by_user: run_id=7"),
            "running :retry should log the forced-retry marker"
        );
    });
}

#[test]
fn conflicting_running_termination_request_keeps_first_intent_and_surfaces_status() {
    with_temp_root(|| {
        let mut state = SessionState::new("running-termination-first-wins".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = mk_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "stop".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "retry".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        assert_eq!(
            app.pending_termination,
            Some(PendingTermination::new_stop_only(7))
        );
        let status = app.status_line.borrow().render().expect("status flash");
        assert!(
            status
                .to_string()
                .contains("Termination already pending: keeping stop without retry.")
        );

        let events_path = session_state::session_dir(&app.state.session_id).join("events.toml");
        let events = std::fs::read_to_string(events_path).expect("events log");
        assert!(events.contains("agent_stopped_by_user: run_id=7"));
        assert!(!events.contains("agent_retry_requested_by_user: run_id=7"));
    });
}

#[test]
fn idle_enter_retries_selected_target() {
    with_temp_root(|| {
        let session_id = "idle-enter-retry-selected-task";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ImplementationRound(1);
        state.builder.current_task = Some(1);
        state.agent_runs.push(RunRecord {
            id: 1,
            stage: "coder".to_string(),
            task_id: Some(1),
            round: 1,
            attempt: 1,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Round 1 Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("exit(1)".to_string()),
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        });
        let removed_run = state.agent_runs[0].clone();
        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            10,
            1,
            10,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        write_finish_stamp_for_run(&app, &removed_run, 1, "");
        app.rebuild_tree_view(None);
        app.selected = row_index(&app, "Task 1");

        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        assert_eq!(app.state.agent_runs.len(), 1);
        assert_eq!(app.state.agent_runs[0].status, RunStatus::Running);
        assert_eq!(app.state.agent_runs[0].stage, "coder");
    });
}

#[test]
fn bare_enter_while_running_does_not_trigger_retry() {
    with_temp_root(|| {
        let mut state = SessionState::new("running-enter-no-retry".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = mk_app(state);
        app.current_run_id = Some(7);
        let before = app
            .state
            .agent_runs
            .iter()
            .filter(|run| run.stage == "brainstorm")
            .count();

        assert!(!app.handle_key(key(crossterm::event::KeyCode::Enter)));

        let after = app
            .state
            .agent_runs
            .iter()
            .filter(|run| run.stage == "brainstorm")
            .count();
        assert_eq!(after, before, "bare Enter must not trigger running retry");
    });
}

#[test]
fn quit_command_with_running_agent_opens_confirmation_modal() {
    with_temp_root(|| {
        let mut state = SessionState::new("quit-running-modal".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = mk_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        for c in "quit".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit, "quit should wait for post-stop finalization");
        assert_eq!(app.active_modal(), Some(ModalKind::QuitRunningAgent));
    });
}

#[test]
fn quit_confirmation_cancel_leaves_run_active() {
    with_temp_root(|| {
        let mut state = SessionState::new("quit-running-modal-cancel".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = mk_app(state);
        app.current_run_id = Some(7);
        app.pending_quit_confirmation_run_id = Some(7);

        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Char('q')));

        assert!(!should_quit);
        assert_eq!(app.active_modal(), None);
        assert!(app.has_running_agent());
    });
}

#[test]
fn pending_guard_resume_fail_closed_when_decision_missing() {
    with_temp_root(|| {
        let session_id = "pending-guard-resume-fail";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::GitGuardPending;
        state.builder.recovery_trigger_task_id = Some(2);
        state.builder.recovery_prev_max_task_id = Some(4);
        state.builder.recovery_prev_task_ids = vec![1, 2, 3, 4];
        state.builder.recovery_trigger_summary = Some("stale guard context".to_string());
        state.save().expect("save");

        let app = App::new(SessionState::load(session_id).expect("load session"));
        assert_eq!(
            app.state.current_phase,
            Phase::BlockedNeedsUser,
            "must fail closed to BlockedNeedsUser"
        );
        assert!(
            app.state.agent_error.is_some(),
            "agent_error must be set on fail-closed"
        );
        assert_eq!(app.state.builder.recovery_trigger_task_id, None);
        assert_eq!(app.state.builder.recovery_prev_max_task_id, None);
        assert!(app.state.builder.recovery_prev_task_ids.is_empty());
        assert_eq!(app.state.builder.recovery_trigger_summary, None);
    });
}

#[test]
fn pending_guard_resume_restores_modal_when_decision_present() {
    with_temp_root(|| {
        let session_id = "pending-guard-resume-ok";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::GitGuardPending;
        state.pending_guard_decision = Some(PendingGuardDecision {
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            run_id: 99,
            captured_head: "abc".to_string(),
            current_head: "def".to_string(),
            warnings: vec![],
        });
        state.save().expect("save");

        let app = App::new(SessionState::load(session_id).expect("load session"));
        assert_eq!(app.state.current_phase, Phase::GitGuardPending);
        assert!(app.state.pending_guard_decision.is_some());
    });
}

#[test]
fn pending_guard_stale_decision_cleared_on_resume() {
    with_temp_root(|| {
        let session_id = "pending-guard-stale";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.pending_guard_decision = Some(PendingGuardDecision {
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            run_id: 77,
            captured_head: "aaa".to_string(),
            current_head: "bbb".to_string(),
            warnings: vec![],
        });
        state.save().expect("save");

        let app = App::new(SessionState::load(session_id).expect("load session"));
        assert!(
            app.state.pending_guard_decision.is_none(),
            "stale pending_guard_decision must be cleared on resume"
        );
        assert_eq!(app.state.current_phase, Phase::BrainstormRunning);
    });
}

#[test]
fn non_yolo_prompts_keep_interactive_operator_cues() {
    with_temp_root(|| {
        let session_dir = session_state::session_dir("stage-completion-prompts");
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let recovery_path = artifacts.join("recovery.toml");
        let summary_path = artifacts.join("session_summary.toml");
        let live_summary = artifacts.join("live_summary.txt");
        std::fs::create_dir_all(&artifacts).unwrap();

        let brainstorm = brainstorm_prompt(
            "add a feature",
            &spec_path.display().to_string(),
            &summary_path.display().to_string(),
            &live_summary.display().to_string(),
            false,
        );
        assert!(!brainstorm.contains("You have the operator's full trust."));
        assert!(brainstorm.contains("Operator IS available for design questions"));
        assert!(
            brainstorm
                .contains("Stage completion — ONLY once all pending design questions are resolved")
        );
        assert!(brainstorm.contains(
            "While you are\nstill waiting for the operator's input, never include this cue."
        ));
        assert!(!brainstorm.contains("End your final message"));

        let planning = planning_prompt(&spec_path, &[], &plan_path, &live_summary, false);
        assert!(!planning.contains("You have the operator's full trust."));
        assert!(planning.contains("Escalation rules — ask the operator when:"));
        assert!(
            planning.contains("The feedback affects end-user-facing design (UI/UX, CLI behavior,")
        );
        assert!(planning.contains("The feedback is an internal design decision"));
        assert!(planning.contains("Cosmetic / trivial (typos, naming nits, formatting,"));
        assert!(
            !planning.contains("If a real trade-off exceeds your\nconfidence, ASK the operator")
        );
        assert!(
            planning.contains(
                "Stage completion — ONLY once all pending trade-off decisions are resolved"
            )
        );
        assert!(planning.contains(
            "While you are still waiting\nfor the operator's input, never include this cue."
        ));
        assert!(!planning.contains("End your final message"));

        let recovery = recovery_prompt(
            &spec_path,
            &plan_path,
            &tasks_path,
            Some(1),
            Some("needs confirmation"),
            &[],
            &[1],
            &live_summary,
            &recovery_path,
            true,
        );
        assert!(recovery.contains(
            "Stage completion — ONLY once all pending confirmation decisions are resolved"
        ));
        assert!(recovery.contains(
            "While you are\nstill waiting for the operator's confirmation, never include this cue."
        ));
        assert!(!recovery.contains("End your final message"));
    });
}

#[test]
fn spec_review_paused_enter_advances_regardless_of_selection() {
    with_temp_root(|| {
        let mut state = SessionState::new("test".into());
        state.current_phase = Phase::SpecReviewPaused;
        let mut app = idle_app(state);
        app.selected = 999;
        app.handle_key(key(crossterm::event::KeyCode::Enter));
        assert_eq!(app.state.current_phase, Phase::PlanningRunning);
    });
}

#[test]
fn plan_review_paused_n_reruns_plan_review() {
    with_temp_root(|| {
        let mut state = SessionState::new("test".into());
        state.current_phase = Phase::PlanReviewPaused;
        let mut app = idle_app(state);
        app.selected = 999;
        app.handle_key(key(crossterm::event::KeyCode::Char('n')));
        assert_eq!(app.state.current_phase, Phase::PlanReviewRunning);
    });
}

#[test]
fn modal_up_down_space_no_state_mutation() {
    with_temp_root(|| {
        let mut state = SessionState::new("test".into());
        state.current_phase = Phase::SpecReviewPaused;
        let mut app = idle_app(state);
        app.selected = 0;

        for k in [
            crossterm::event::KeyCode::Up,
            crossterm::event::KeyCode::Down,
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyCode::Char('b'),
            crossterm::event::KeyCode::Char('e'),
        ] {
            app.handle_key(key(k));
            assert_eq!(app.state.current_phase, Phase::SpecReviewPaused);
            assert_eq!(app.selected, 0); // No scroll occurred
        }
    });
}

#[test]
fn stage_error_enter_relaunches_from_non_current_row() {
    with_temp_root(|| {
        let mut state = SessionState::new("test".into());
        state.current_phase = Phase::SpecReviewRunning;
        state.agent_error = Some("something went wrong".to_string());
        let mut app = idle_app(state);
        app.selected = 999;
        app.models = vec![ranked_model(
            selection::VendorKind::Codex,
            "gpt-5",
            1,
            10,
            10,
        )];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        app.handle_key(key(crossterm::event::KeyCode::Enter));
        assert!(app.state.agent_error.is_none());
        assert!(app.current_run_id.is_some());
        assert_eq!(app.state.current_phase, Phase::SpecReviewRunning);
    });
}

// ---------------------------------------------------------------------------
// Split target ownership tests
// ---------------------------------------------------------------------------

#[test]
fn resolve_split_target_run_row() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-run".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        let bs_idx = row_index(&app, "Brainstorm");
        app.selected = bs_idx;

        let target = app.resolve_split_target_for_selected_row();
        assert_eq!(target, Some(super::split::SplitTarget::Run(7)));
    });
}

#[test]
fn resolve_split_target_idea_row() {
    with_temp_root(|| {
        let state = SessionState::new("split-idea".to_string());
        let mut app = idle_app(state);
        let idea_idx = row_index(&app, "Idea");
        app.selected = idea_idx;

        let target = app.resolve_split_target_for_selected_row();
        assert_eq!(target, Some(super::split::SplitTarget::Idea));
    });
}

#[test]
fn resolve_split_target_other_row() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-none".to_string());
        state.current_phase = Phase::SpecReviewRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        // Select "Spec Review" stage row (no run_id directly on it in this setup)
        let sr_idx = row_index(&app, "Spec Review");
        app.selected = sr_idx;

        let target = app.resolve_split_target_for_selected_row();
        assert_eq!(target, None);
    });
}

#[test]
fn enter_opens_run_split_target() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-enter-run".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        let bs_idx = row_index(&app, "Brainstorm");
        app.selected = bs_idx;

        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert_eq!(app.split_target, Some(super::split::SplitTarget::Run(7)));
    });
}

#[test]
fn enter_opens_idea_split_target() {
    with_temp_root(|| {
        let state = SessionState::new("split-enter-idea".to_string());
        let mut app = idle_app(state);
        let idea_idx = row_index(&app, "Idea");
        app.selected = idea_idx;

        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert_eq!(app.split_target, Some(super::split::SplitTarget::Idea));
    });
}

#[test]
fn enter_does_not_toggle_close_same_target() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-no-toggle".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        let bs_idx = row_index(&app, "Brainstorm");
        app.selected = bs_idx;
        app.split_target = Some(super::split::SplitTarget::Run(7));
        app.split_scroll_offset = 42;

        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert_eq!(app.split_target, Some(super::split::SplitTarget::Run(7)));
        assert_eq!(
            app.split_scroll_offset, 42,
            "scroll must be preserved on same-target Enter"
        );
    });
}

#[test]
fn enter_does_not_switch_target_when_split_is_already_open() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-switch".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        let bs_idx = row_index(&app, "Brainstorm");
        app.selected = bs_idx;
        app.split_target = Some(super::split::SplitTarget::Idea);
        app.split_scroll_offset = 42;

        app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert_eq!(app.split_target, Some(super::split::SplitTarget::Idea));
        assert_eq!(
            app.split_scroll_offset, 42,
            "split-open Enter should be consumed before tree target resolution"
        );
    });
}

#[test]
fn split_new_target_clamps_to_tail_position() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-tail-default".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        app.body_inner_height = 9;
        for idx in 0..10 {
            app.messages.push(Message {
                ts: chrono::Utc::now(),
                run_id: 7,
                kind: MessageKind::UserInput,
                sender: MessageSender::System,
                text: format!("line {idx}"),
            });
        }

        app.open_split_target(super::split::SplitTarget::Run(7));
        let content_height = app.current_split_content_height();
        let expected_tail = crate::app::chat_widget_view_model::max_chat_scroll_offset(
            content_height,
            app.split_viewport_height(),
        );
        app.clamp_split_scroll(content_height);

        assert_eq!(
            app.split_scroll_offset, expected_tail,
            "new run targets should open at the tail view, not the transcript top"
        );
    });
}

#[test]
fn split_scroll_detach_preserves_offset_across_new_content() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-tail-detach".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        app.body_inner_height = 9;
        for idx in 0..10 {
            app.messages.push(Message {
                ts: chrono::Utc::now(),
                run_id: 7,
                kind: MessageKind::UserInput,
                sender: MessageSender::System,
                text: format!("line {idx}"),
            });
        }
        app.open_split_target(super::split::SplitTarget::Run(7));
        let content_height = app.current_split_content_height();
        let expected_tail = crate::app::chat_widget_view_model::max_chat_scroll_offset(
            content_height,
            app.split_viewport_height(),
        );
        app.clamp_split_scroll(content_height);

        app.handle_key(key(crossterm::event::KeyCode::Up));
        assert_eq!(
            app.split_scroll_offset,
            expected_tail.saturating_sub(1),
            "Up should detach from the tail"
        );
        let detached_offset = app.split_scroll_offset;

        app.messages.push(Message {
            ts: chrono::Utc::now(),
            run_id: 7,
            kind: MessageKind::UserInput,
            sender: MessageSender::System,
            text: "line 10".to_string(),
        });
        app.clamp_split_scroll(app.current_split_content_height());

        assert_eq!(
            app.split_scroll_offset, detached_offset,
            "new transcript content must not yank a detached split viewport back toward the tail"
        );
    });
}

#[test]
fn split_scroll_clamps_after_viewport_growth() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-tail-clamp-grow".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        app.body_inner_height = 9;
        for idx in 0..15 {
            app.messages.push(Message {
                ts: chrono::Utc::now(),
                run_id: 7,
                kind: MessageKind::UserInput,
                sender: MessageSender::System,
                text: format!("line {idx}"),
            });
        }
        app.open_split_target(super::split::SplitTarget::Run(7));
        let content_height = app.current_split_content_height();
        let expected_tail = crate::app::chat_widget_view_model::max_chat_scroll_offset(
            content_height,
            app.split_viewport_height(),
        );
        app.clamp_split_scroll(content_height);
        app.handle_key(key(crossterm::event::KeyCode::Up));
        app.handle_key(key(crossterm::event::KeyCode::Up));
        assert_eq!(app.split_scroll_offset, expected_tail.saturating_sub(2));

        app.body_inner_height = 18;
        let clamped_tail = crate::app::chat_widget_view_model::max_chat_scroll_offset(
            app.current_split_content_height(),
            app.split_viewport_height(),
        );
        app.clamp_split_scroll(app.current_split_content_height());

        assert_eq!(
            app.split_scroll_offset, clamped_tail,
            "viewport changes should clamp detached offsets into the new valid range"
        );
    });
}

#[test]
fn split_open_space_does_not_toggle_tree_expansion() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-space-consumed".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        let bs_idx = row_index(&app, "Brainstorm");
        app.selected = bs_idx;
        let expanded_before = app.is_expanded(bs_idx);
        app.open_split_target(super::split::SplitTarget::Run(7));

        app.handle_key(key(crossterm::event::KeyCode::Char(' ')));

        assert_eq!(
            app.is_expanded(bs_idx),
            expanded_before,
            "split-open transcript mode should consume Space before tree expansion logic"
        );
    });
}

#[test]
fn esc_closes_split_when_open() {
    with_temp_root(|| {
        let mut app = idle_app(SessionState::new("split-esc".to_string()));
        app.split_target = Some(super::split::SplitTarget::Idea);
        app.split_scroll_offset = 5;

        let quit = app.handle_key(key(crossterm::event::KeyCode::Esc));

        assert!(!quit, "Esc must not quit while split is open");
        assert_eq!(app.split_target, None);
        assert_eq!(app.split_scroll_offset, 0);
    });
}

#[test]
fn poll_agent_run_closes_matching_interactive_run_split_on_exit() {
    with_temp_root(|| {
        let mut state = SessionState::new("interactive-close-matching-split".to_string());
        state.current_phase = Phase::PlanningRunning;
        state.agent_runs.push(RunRecord {
            id: 42,
            stage: "planning".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Planning]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes {
                interactive: true,
                ..Default::default()
            },
            hostname: None,
            mount_device_id: None,
        });

        let mut app = idle_app(state);
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness::default(),
        )));
        app.current_run_id = Some(42);
        app.run_launched = true;
        app.split_target = Some(super::split::SplitTarget::Run(42));
        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));

        app.poll_agent_run();

        assert_eq!(app.split_target, None);
    });
}

#[test]
fn poll_agent_run_preserves_switched_split_target_on_interactive_exit() {
    with_temp_root(|| {
        let mut state = SessionState::new("interactive-preserve-switched-split".to_string());
        state.current_phase = Phase::PlanningRunning;
        state.agent_runs.push(RunRecord {
            id: 42,
            stage: "planning".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Planning]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: crate::state::LaunchModes {
                interactive: true,
                ..Default::default()
            },
            hostname: None,
            mount_device_id: None,
        });

        let mut app = idle_app(state);
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness::default(),
        )));
        app.current_run_id = Some(42);
        app.run_launched = true;
        app.split_target = Some(super::split::SplitTarget::Idea);
        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));

        app.poll_agent_run();

        assert_eq!(app.split_target, Some(super::split::SplitTarget::Idea));
    });
}

#[test]
fn esc_quits_when_split_closed_and_no_agent_running() {
    with_temp_root(|| {
        let mut app = idle_app(SessionState::new("split-esc-quit".to_string()));
        app.split_target = None;

        let quit = app.handle_key(key(crossterm::event::KeyCode::Esc));

        assert!(
            quit,
            "Esc should quit when split is closed and no agent running"
        );
    });
}

#[test]
fn rebuild_closes_invalid_run_target() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-rebuild".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        app.split_target = Some(super::split::SplitTarget::Run(7));
        app.split_scroll_offset = 3;

        // Remove the run without explicitly closing the split.
        app.state.agent_runs.retain(|run| run.id != 7);
        app.rebuild_tree_view(None);

        assert_eq!(
            app.split_target, None,
            "split must close when run disappears"
        );
        assert_eq!(app.split_scroll_offset, 0);
    });
}

#[test]
fn rebuild_preserves_idea_target() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-idea-preserved".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        app.split_target = Some(super::split::SplitTarget::Idea);
        app.split_scroll_offset = 3;

        app.rebuild_tree_view(None);

        assert_eq!(app.split_target, Some(super::split::SplitTarget::Idea));
        assert_eq!(
            app.split_scroll_offset, 0,
            "Idea split scroll clamps because Idea content is currently non-scrollable"
        );
    });
}

#[test]
fn split_follow_tail_reaches_latest_message_lines() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-tail-visible-latest".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        if let Some(run) = state.agent_runs.iter_mut().find(|run| run.id == 7) {
            run.status = RunStatus::Done;
            run.ended_at = Some(chrono::Utc::now());
        }
        let mut app = idle_app(state);
        app.body_inner_height = 9;
        app.body_inner_width = 80;
        app.open_split_target(super::split::SplitTarget::Run(7));

        for idx in 0..10 {
            app.messages.push(Message {
                ts: chrono::Utc::now(),
                run_id: 7,
                kind: MessageKind::UserInput,
                sender: MessageSender::System,
                text: format!("line {idx}"),
            });
        }

        app.clamp_split_scroll(app.current_split_content_height());
        let content_height = app.current_split_content_height();
        let window = crate::app::chat_widget_view_model::chat_scroll_window(
            content_height,
            app.split_viewport_height(),
            app.split_scroll_offset,
        )
        .expect("scroll window");

        assert_eq!(
            window.visible_end, content_height,
            "tail-follow should keep the newest transcript line in view"
        );
        assert!(
            window.offset > 0,
            "tail-follow should not reset new targets to the transcript top when content overflows"
        );
    });
}

#[test]
fn split_viewport_height_accounts_for_separator_row() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-viewport-separator".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(RunRecord {
            id: 7,
            stage: "brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "m".to_string(),
            vendor: "v".to_string(),
            window_name: "[Brainstorm]".to_string(),
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
        app.split_target = Some(super::split::SplitTarget::Run(7));
        app.body_inner_height = 10;
        app.split_fullscreen = false;

        assert_eq!(
            app.split_viewport_height(),
            6,
            "non-fullscreen split viewport should match render allocation after the separator row"
        );
    });
}

#[test]
fn split_follow_tail_keeps_live_running_tail_visible() {
    with_temp_root(|| {
        let mut state = SessionState::new("split-tail-visible-running".to_string());
        state.current_phase = Phase::BrainstormRunning;
        state.agent_runs.push(make_brainstorm_run(7));
        let mut app = idle_app(state);
        app.body_inner_height = 9;
        app.body_inner_width = 80;
        app.selected = row_index(&app, "Brainstorm");
        app.open_split_target(super::split::SplitTarget::Run(7));

        for idx in 0..10 {
            app.messages.push(Message {
                ts: chrono::Utc::now(),
                run_id: 7,
                kind: MessageKind::UserInput,
                sender: MessageSender::System,
                text: format!("line {idx}"),
            });
        }

        let run = app
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == 7)
            .expect("run");
        let local_offset = chrono::Local::now().fixed_offset().offset().to_owned();
        let rendered_total = crate::app::chat_widget::message_lines(
            &app.messages,
            run,
            &local_offset,
            Some(ratatui::text::Line::from("LIVE-TAIL")),
            app.body_inner_width.max(1),
            0,
            true,
        )
        .len();

        app.clamp_split_scroll(app.current_split_content_height());
        let window = crate::app::chat_widget_view_model::chat_scroll_window(
            rendered_total,
            app.split_viewport_height(),
            app.split_scroll_offset,
        )
        .expect("scroll window");

        assert_eq!(
            window.visible_end, rendered_total,
            "tail-follow should keep the rendered live tail visible for running transcripts"
        );
        assert!(
            !window.show_below_indicator,
            "follow-tail should not leave newer rendered transcript lines below the split viewport"
        );
    });
}

fn make_non_interactive_run(id: u64, window_name: &str) -> RunRecord {
    RunRecord {
        id,
        stage: "planning".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "m".to_string(),
        vendor: "v".to_string(),
        window_name: window_name.to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes {
            interactive: false,
            ..Default::default()
        },
        hostname: None,
        mount_device_id: None,
    }
}

fn app_waiting_on_agent_exit(session_id: &str) -> (App, String) {
    let mut state = SessionState::new(session_id.to_string());
    state.current_phase = Phase::BrainstormRunning;
    let mut run = make_brainstorm_run(7);
    run.window_name = format!("[Brainstorm {session_id}]");
    run.modes.interactive = true;
    let window_name = run.window_name.clone();
    let model = run.model.clone();
    let vendor = run.vendor.clone();
    state.agent_runs.push(run);
    crate::runner::request_run_label_interactive_input_for_test(&window_name);
    let mut app = idle_app(state);
    app.current_run_id = Some(7);
    app.messages.push(Message {
        ts: chrono::Utc::now(),
        run_id: 7,
        kind: MessageKind::AgentText,
        sender: MessageSender::Agent { model, vendor },
        text: "Done. Enter /exit if there are no further requests.".to_string(),
    });
    (app, window_name)
}

#[test]
fn synchronize_split_target_does_not_auto_open_for_non_interactive_run() {
    with_temp_root(|| {
        let mut state = SessionState::new("non-interactive-no-auto-open".to_string());
        state.current_phase = Phase::PlanningRunning;
        state
            .agent_runs
            .push(make_non_interactive_run(42, "non-int-1"));

        // Even with the runner label flagged as waiting for input, a
        // non-interactive run must not trigger auto-open, auto-switch, or
        // forced input focus.
        crate::runner::request_run_label_interactive_input_for_test("non-int-1");

        let mut app = idle_app(state);
        app.current_run_id = Some(42);

        assert!(app.split_target.is_none());
        assert!(!app.input_mode);

        app.synchronize_split_target();

        assert!(
            app.split_target.is_none(),
            "non-interactive run must not auto-open the split"
        );
        assert!(
            !app.input_mode,
            "non-interactive run must not force input focus"
        );
    });
}

#[test]
fn synchronize_split_target_does_not_force_focus_for_non_interactive_open_split() {
    with_temp_root(|| {
        let mut state = SessionState::new("non-interactive-manual-split".to_string());
        state.current_phase = Phase::PlanningRunning;
        state
            .agent_runs
            .push(make_non_interactive_run(42, "non-int-2"));

        crate::runner::request_run_label_interactive_input_for_test("non-int-2");

        let mut app = idle_app(state);
        app.current_run_id = Some(42);
        // Operator manually opened the split for this non-interactive run.
        app.split_target = Some(super::split::SplitTarget::Run(42));

        app.synchronize_split_target();

        assert_eq!(
            app.split_target,
            Some(super::split::SplitTarget::Run(42)),
            "manually opened split for a non-interactive run must remain open"
        );
        assert!(
            !app.input_mode,
            "non-interactive run must not gain forced input focus from sync"
        );
    });
}

#[test]
fn poll_agent_run_does_not_close_split_for_non_interactive_run_on_exit() {
    with_temp_root(|| {
        let mut state = SessionState::new("non-interactive-exit-keep-split".to_string());
        state.current_phase = Phase::PlanningRunning;
        state
            .agent_runs
            .push(make_non_interactive_run(42, "[Planning]"));

        let mut app = idle_app(state);
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness::default(),
        )));
        app.current_run_id = Some(42);
        app.run_launched = true;
        // Operator opened the split manually; lifecycle exit must not close it.
        app.split_target = Some(super::split::SplitTarget::Run(42));
        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));

        app.poll_agent_run();

        assert_eq!(
            app.split_target,
            Some(super::split::SplitTarget::Run(42)),
            "non-interactive exit must not auto-close a manually opened split"
        );
    });
}

#[test]
fn skip_to_impl_round_entry_writes_review_scope() {
    with_temp_root(|| {
        let session_id = "skip-to-impl-review-scope";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        // Synthetic-artifacts generation expects spec.md and tasks.toml.
        std::fs::write(artifacts.join("spec.md"), "# spec\n").expect("spec");
        std::fs::write(
            artifacts.join("tasks.toml"),
            "[[tasks]]\nid = 1\ntitle = \"Task 1\"\ndescription = \"d\"\ntest = \"cargo test\"\nestimated_tokens = 100\n",
        )
        .expect("tasks");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::SkipToImplPending;
        state.skip_to_impl_rationale = Some("small change".to_string());
        state.skip_to_impl_kind = Some(crate::artifacts::SkipToImplKind::SkipToImpl);

        let mut app = idle_app(state);
        app.accept_skip_to_implementation()
            .expect("skip-to-impl accept");

        // Round-entry hook in `transition_to_phase` must produce
        // `review_scope.toml` even though no reviewer ever runs on this path.
        let scope_path = session_dir
            .join("rounds")
            .join("001")
            .join("review_scope.toml");
        assert!(
            scope_path.exists(),
            "skip-to-impl entry into ImplementationRound(1) must pin review_scope.toml so the simplifier has a base SHA",
        );
        assert_eq!(app.state.current_phase, Phase::ImplementationRound(1));
    });
}

#[test]
fn final_validation_goal_gap_round_entry_writes_review_scope() {
    with_temp_root(|| {
        let session_id = "goal-gap-review-scope";
        let session_dir = session_state::session_dir(session_id);
        let artifacts = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");

        let mut state = SessionState::new(session_id.to_string());
        // The state graph allows FinalValidation(R) -> ImplementationRound(R+1)
        // for goal-gap reruns; jumping directly there exercises the round-entry
        // hook in transition_to_phase.
        state.current_phase = Phase::FinalValidation(2);
        state.validation_attempts = 2;
        let mut app = idle_app(state);

        app.transition_to_phase(Phase::ImplementationRound(3))
            .expect("goal-gap rerun transition");

        let scope_path = session_dir
            .join("rounds")
            .join("003")
            .join("review_scope.toml");
        assert!(
            scope_path.exists(),
            "goal-gap rerun entry into ImplementationRound(R+1) must pin review_scope.toml for the next simplifier pass",
        );
    });
}

#[test]
fn impl_round_entry_preserves_existing_review_scope() {
    with_temp_root(|| {
        let session_id = "impl-round-scope-idempotent";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        std::fs::create_dir_all(&round_dir).expect("round dir");
        // Pin the file with a sentinel SHA before transitioning so we can
        // confirm the round-entry hook is idempotent on resume.
        std::fs::write(
            round_dir.join("review_scope.toml"),
            "base_sha = \"already-pinned\"\n",
        )
        .expect("seed scope");

        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::ShardingRunning;
        let mut app = idle_app(state);

        app.transition_to_phase(Phase::ImplementationRound(1))
            .expect("sharding -> impl transition");

        let contents =
            std::fs::read_to_string(round_dir.join("review_scope.toml")).expect("read scope");
        assert!(
            contents.contains("already-pinned"),
            "round entry must not overwrite an existing review_scope.toml; got {contents:?}",
        );
    });
}

// ---------------------------------------------------------------------------
// Watchdog AC1–AC8 integration coverage (spec §5).
//
// These tests drive the App's `tick_watchdog` loop with synthetic
// `WatchdogState` entries. Idle elapsed is simulated by rewinding
// `last_live_summary_event` rather than sleeping, so each scenario runs in
// constant wall-clock time. The runner side is stubbed via
// `request_run_label_active_for_test` so `force_interrupt_run_label` and
// `terminate_run_label` reach a real `mpsc` channel that the test can drain.
// ---------------------------------------------------------------------------

const WATCHDOG_TEST_PROMPT_BODY: &str =
    "Original coder prompt — keep this file current until you exit.";

fn write_watchdog_test_prompt(session_id: &str, name: &str) -> std::path::PathBuf {
    let dir = session_state::session_dir(session_id).join("prompts");
    std::fs::create_dir_all(&dir).expect("prompts dir");
    let path = dir.join(name);
    std::fs::write(&path, WATCHDOG_TEST_PROMPT_BODY).expect("write prompt");
    path
}

fn install_watchdog_run(
    app: &mut App,
    run_id: u64,
    window_name: &str,
    prompt_path: std::path::PathBuf,
    effort: EffortLevel,
) {
    app.watchdog.register(
        run_id,
        effort,
        window_name.to_string(),
        prompt_path,
        std::time::Instant::now(),
    );
}

/// Rewind `last_live_summary_event` so `idle_elapsed(now) >= shift`. Mirrors
/// "wait `shift` real seconds without writing a summary" without sleeping.
fn fast_forward_idle(app: &mut App, run_id: u64, shift: Duration) {
    let state = app
        .watchdog
        .get_mut(run_id)
        .expect("watchdog state registered");
    state.last_live_summary_event = std::time::Instant::now() - shift - Duration::from_millis(1);
}

#[test]
fn watchdog_warning_emits_summarywarn_and_verbatim_prompt_interrupt() {
    with_temp_root(|| {
        let session_id = "watchdog-warn-ac1-ac7";
        let mut state = coder_round_state(session_id);
        let run = make_coder_run(10, 1, 1);
        let window_name = run.window_name.clone();
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;

        crate::runner::request_run_label_active_for_test(&window_name);
        let prompt_path = write_watchdog_test_prompt(session_id, "coder-r1.md");

        install_watchdog_run(
            &mut app,
            run.id,
            &window_name,
            prompt_path,
            EffortLevel::Normal,
        );
        fast_forward_idle(&mut app, run.id, super::watchdog::WARN_AFTER_NORMAL);

        app.tick_watchdog();

        let inputs = crate::runner::drain_test_input_receiver_for(&window_name);
        assert_eq!(
            inputs.len(),
            1,
            "AC1: exactly one watchdog interrupt should have been queued",
        );
        let (kind, body) = &inputs[0];
        assert_eq!(
            *kind, "interrupt",
            "AC1: warning must use AcpInput::Interrupt"
        );
        assert!(
            body.contains("Liveness warning from codexize watchdog"),
            "AC1: warning preamble missing"
        );
        assert!(
            body.contains("ORIGINAL PROMPT"),
            "AC7: warning body must contain ORIGINAL PROMPT marker"
        );
        assert!(
            body.contains(WATCHDOG_TEST_PROMPT_BODY),
            "AC7: warning body must contain the verbatim prompt text"
        );
        assert!(
            body.contains("10 minutes"),
            "AC1: remaining-minutes count must read from unscaled spec constants"
        );

        let summary_warn_count = app
            .messages
            .iter()
            .filter(|m| {
                m.run_id == run.id
                    && m.kind == MessageKind::SummaryWarn
                    && m.text.contains("watchdog warning")
            })
            .count();
        assert_eq!(
            summary_warn_count, 1,
            "AC1: exactly one SummaryWarn for the warning",
        );

        // Idempotent: a second tick at the same elapsed must not re-send.
        app.tick_watchdog();
        let inputs_after = crate::runner::drain_test_input_receiver_for(&window_name);
        assert!(
            inputs_after.is_empty(),
            "AC1: warning must not re-arm; got {inputs_after:?}",
        );
        let summary_warn_count_after = app
            .messages
            .iter()
            .filter(|m| m.kind == MessageKind::SummaryWarn && m.text.contains("watchdog warning"))
            .count();
        assert_eq!(
            summary_warn_count_after, 1,
            "AC1: SummaryWarn must not duplicate"
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_kill_sends_terminate_and_drops_state() {
    with_temp_root(|| {
        let session_id = "watchdog-kill-ac2-partial";
        let mut state = coder_round_state(session_id);
        let run = make_coder_run(20, 1, 1);
        let window_name = run.window_name.clone();
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;

        crate::runner::request_run_label_active_for_test(&window_name);
        let prompt_path = write_watchdog_test_prompt(session_id, "coder-r1.md");

        install_watchdog_run(
            &mut app,
            run.id,
            &window_name,
            prompt_path,
            EffortLevel::Normal,
        );
        // Push elapsed past the kill threshold without ever crossing warn —
        // mirrors a starved poll loop (spec §3.3) so AC2's "kill without prior
        // warning" branch is exercised.
        fast_forward_idle(&mut app, run.id, super::watchdog::KILL_AFTER_NORMAL);

        app.tick_watchdog();

        let cancels = crate::runner::drain_test_cancel_receiver_for(&window_name);
        assert_eq!(cancels, vec!["terminate"], "AC2: kill must send Terminate");
        let inputs = crate::runner::drain_test_input_receiver_for(&window_name);
        assert!(
            inputs.is_empty(),
            "AC2: kill path must not also enqueue a warning interrupt"
        );
        let kill_summary = app
            .messages
            .iter()
            .filter(|m| {
                m.run_id == run.id
                    && m.kind == MessageKind::SummaryWarn
                    && m.text.contains("watchdog kill")
            })
            .count();
        assert_eq!(kill_summary, 1, "AC2: exactly one kill SummaryWarn");
        assert!(
            app.watchdog.get(run.id).is_none(),
            "AC2: kill must drop the per-run watchdog state",
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_kill_finalizes_failed_run_and_relaunches_with_different_vendor() {
    with_temp_root(|| {
        let session_id = "watchdog-kill-ac2-failover";
        let session_dir = session_state::session_dir(session_id);
        let round_dir = session_dir.join("rounds").join("001");
        write_review_scope(&round_dir, "base123");

        let mut state = coder_round_state(session_id);
        let run = make_coder_run(30, 1, 1);
        let window_name = run.window_name.clone();
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;
        app.models = vec![
            ranked_model(selection::VendorKind::Codex, "gpt-5", 10, 1, 10),
            ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 10, 2, 10),
        ];
        // The retry attempt #2 will go through the test-launch harness; let
        // it succeed so the relaunch sticks and we can assert the vendor.
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: None,
                    launch_error: None,
                }]),
            },
        )));

        crate::runner::request_run_label_active_for_test(&window_name);
        let prompt_path = write_watchdog_test_prompt(session_id, "coder-r1.md");
        install_watchdog_run(
            &mut app,
            run.id,
            &window_name,
            prompt_path,
            EffortLevel::Normal,
        );
        fast_forward_idle(&mut app, run.id, super::watchdog::KILL_AFTER_NORMAL);

        app.tick_watchdog();

        let cancels = crate::runner::drain_test_cancel_receiver_for(&window_name);
        assert_eq!(cancels, vec!["terminate"], "AC2: Terminate on cancel_tx");

        // Simulate the runner thread reacting to Terminate: the active map
        // entry is dropped and a finish stamp lands with exit_code 143.
        crate::runner::cancel_run_labels_matching(&window_name);
        let stamp = crate::runner::FinishStamp {
            finished_at: chrono::Utc::now().to_rfc3339(),
            exit_code: 143,
            head_before: "base123".to_string(),
            head_after: "base123".to_string(),
            head_state: "stable".to_string(),
            signal_received: "TERM".to_string(),
            working_tree_clean: true,
        };
        let stamp_path = app.finish_stamp_path_for(&run);
        std::fs::create_dir_all(stamp_path.parent().unwrap()).expect("stamp dir");
        crate::runner::write_finish_stamp(&stamp_path, &stamp).expect("write stamp");
        app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));

        app.poll_agent_run();

        let failed = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == run.id)
            .expect("original run record");
        assert_eq!(failed.status, RunStatus::Failed, "AC2: original run failed");

        let retry = app
            .state
            .agent_runs
            .iter()
            .find(|r| r.stage == "coder" && r.attempt == 2)
            .expect("AC2: vendor failover must launch attempt 2 on a different vendor");
        assert_ne!(
            retry.vendor, run.vendor,
            "AC2: retry vendor must differ from the watchdog-killed vendor"
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_uses_tough_thresholds() {
    with_temp_root(|| {
        let session_id = "watchdog-tough-ac3";
        let mut state = coder_round_state(session_id);
        let mut run = make_coder_run(40, 1, 1);
        run.effort = EffortLevel::Tough;
        let window_name = run.window_name.clone();
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;

        crate::runner::request_run_label_active_for_test(&window_name);
        let prompt_path = write_watchdog_test_prompt(session_id, "coder-r1.md");
        install_watchdog_run(
            &mut app,
            run.id,
            &window_name,
            prompt_path,
            EffortLevel::Tough,
        );

        // Past normal warn (10m) but below tough warn (15m): must not fire.
        fast_forward_idle(
            &mut app,
            run.id,
            super::watchdog::WARN_AFTER_NORMAL + Duration::from_secs(60),
        );
        app.tick_watchdog();
        assert!(
            crate::runner::drain_test_input_receiver_for(&window_name).is_empty(),
            "AC3: tough run must not warn at the normal-effort 10 min threshold",
        );

        // Cross the tough warn threshold (15m).
        fast_forward_idle(&mut app, run.id, super::watchdog::WARN_AFTER_TOUGH);
        app.tick_watchdog();
        let inputs = crate::runner::drain_test_input_receiver_for(&window_name);
        assert_eq!(
            inputs.len(),
            1,
            "AC3: warning must fire after the tough warn threshold"
        );
        assert!(
            inputs[0].1.contains("15 minutes"),
            "AC3: remaining-minutes must reflect the tough kill-warn gap (30 - 15)"
        );

        // Cross the tough kill threshold (30m).
        fast_forward_idle(&mut app, run.id, super::watchdog::KILL_AFTER_TOUGH);
        app.tick_watchdog();
        let cancels = crate::runner::drain_test_cancel_receiver_for(&window_name);
        assert_eq!(
            cancels,
            vec!["terminate"],
            "AC3: kill must fire after the tough kill threshold"
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_clock_pauses_during_tool_call_activity() {
    with_temp_root(|| {
        let session_id = "watchdog-toolcall-ac4";
        let mut state = coder_round_state(session_id);
        let run = make_coder_run(50, 1, 1);
        let window_name = run.window_name.clone();
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;

        crate::runner::request_run_label_active_for_test(&window_name);
        let prompt_path = write_watchdog_test_prompt(session_id, "coder-r1.md");
        install_watchdog_run(
            &mut app,
            run.id,
            &window_name,
            prompt_path,
            EffortLevel::Normal,
        );

        // Anchor: 30 simulated minutes since the last summary write — but a
        // single tool call has been in flight for the last 25 minutes,
        // freezing the idle clock at 5 minutes idle-adjusted.
        let now = std::time::Instant::now();
        let state_mut = app
            .watchdog
            .get_mut(run.id)
            .expect("watchdog state registered");
        state_mut.last_live_summary_event = now - Duration::from_secs(30 * 60);
        state_mut.pause_began_at = Some(now - Duration::from_secs(25 * 60));
        state_mut.in_flight_tool_calls = 1;

        app.tick_watchdog();
        assert!(
            crate::runner::drain_test_input_receiver_for(&window_name).is_empty(),
            "AC4: clock must stay paused while a tool call is in flight",
        );
        assert!(
            crate::runner::drain_test_cancel_receiver_for(&window_name).is_empty(),
            "AC4: kill must not fire while the clock is paused",
        );

        // A second concurrent tool call must not advance the clock further.
        // Its terminal counterpart only releases the pause when in-flight
        // count returns to zero.
        let state_mut = app.watchdog.get_mut(run.id).expect("state");
        state_mut.on_tool_call_started(now);
        state_mut.on_tool_call_finished(now);
        assert_eq!(
            state_mut.in_flight_tool_calls, 1,
            "AC4: counter (not bool) — concurrent calls do not unpause early"
        );

        // Now release the long-running call. Tool call ran for 25 minutes; the
        // idle-adjusted clock advances to 30 - 25 = 5 minutes. Below warn.
        let state_mut = app.watchdog.get_mut(run.id).expect("state");
        state_mut.on_tool_call_finished(now);
        assert_eq!(state_mut.in_flight_tool_calls, 0);
        app.tick_watchdog();
        assert!(
            crate::runner::drain_test_input_receiver_for(&window_name).is_empty(),
            "AC4: 5 min idle-adjusted is below the 10 min warn threshold",
        );

        // Push raw idle to 40 minutes; tool-call subtracts 25 → 15 min idle
        // adjusted, past warn.
        if let Some(s) = app.watchdog.get_mut(run.id) {
            s.last_live_summary_event = now - Duration::from_secs(40 * 60);
        }
        app.tick_watchdog();
        let inputs = crate::runner::drain_test_input_receiver_for(&window_name);
        assert_eq!(
            inputs.len(),
            1,
            "AC4: warning must fire once tool-call-adjusted elapsed crosses warn"
        );

        // Push to 45 minutes → 20 min idle adjusted, past kill.
        if let Some(s) = app.watchdog.get_mut(run.id) {
            s.last_live_summary_event = now - Duration::from_secs(45 * 60);
        }
        app.tick_watchdog();
        let cancels = crate::runner::drain_test_cancel_receiver_for(&window_name);
        assert_eq!(
            cancels,
            vec!["terminate"],
            "AC4: kill must fire after tool-call-adjusted elapsed crosses kill"
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_does_not_arm_for_interactive_runs() {
    with_temp_root(|| {
        let session_id = "watchdog-interactive-ac5";
        let mut state = SessionState::new(session_id.to_string());
        state.current_phase = Phase::PlanningRunning;
        let mut interactive = make_planning_run(60, 1);
        interactive.modes.interactive = true;
        let window_name = interactive.window_name.clone();
        state.agent_runs.push(interactive.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(interactive.id);
        app.run_launched = true;
        app.models = vec![
            ranked_model(selection::VendorKind::Codex, "gpt-5", 1, 10, 10),
            ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 2, 10, 10),
        ];
        app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
            TestLaunchHarness::default(),
        )));

        crate::runner::request_run_label_interactive_input_for_test(&window_name);

        // Drive `start_run_tracking` for an interactive launch and assert
        // the registry stays empty (AC5). The path mirrors the brainstorm
        // launch — start_run_tracking is the only non-test entry point that
        // registers the watchdog.
        app.start_run_tracking(
            "planning",
            None,
            1,
            "gpt-5".to_string(),
            "codex".to_string(),
            window_name.clone(),
            EffortLevel::Normal,
            crate::state::LaunchModes {
                yolo: false,
                cheap: false,
                interactive: true,
            },
            std::path::PathBuf::from("prompts/planning.md"),
        );
        assert!(
            app.watchdog.is_empty(),
            "AC5: interactive run must not register watchdog state"
        );

        // Even with a long-stale fake heartbeat, tick_watchdog is a no-op
        // because nothing is registered.
        app.tick_watchdog();
        assert!(
            crate::runner::drain_test_input_receiver_for(&window_name).is_empty(),
            "AC5: no warning must be sent for interactive runs"
        );
        assert!(
            crate::runner::drain_test_cancel_receiver_for(&window_name).is_empty(),
            "AC5: no Terminate must be sent for interactive runs"
        );
        let any_summary_warn = app
            .messages
            .iter()
            .any(|m| m.kind == MessageKind::SummaryWarn);
        assert!(
            !any_summary_warn,
            "AC5: no SummaryWarn must be appended for interactive runs",
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_warning_does_not_re_arm_after_summary_recovery() {
    with_temp_root(|| {
        let session_id = "watchdog-no-rearm-ac6";
        let mut state = coder_round_state(session_id);
        let run = make_coder_run(70, 1, 1);
        let window_name = run.window_name.clone();
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;

        crate::runner::request_run_label_active_for_test(&window_name);
        let prompt_path = write_watchdog_test_prompt(session_id, "coder-r1.md");
        install_watchdog_run(
            &mut app,
            run.id,
            &window_name,
            prompt_path,
            EffortLevel::Normal,
        );

        // Stage 1: cross warn — exactly one warning fires.
        fast_forward_idle(&mut app, run.id, super::watchdog::WARN_AFTER_NORMAL);
        app.tick_watchdog();
        assert_eq!(
            crate::runner::drain_test_input_receiver_for(&window_name).len(),
            1,
            "AC6: first warning must fire"
        );

        // Stage 2: the agent writes one summary — clock resets, but the
        // `warned` flag stays true (operator answer 5: no re-arm).
        if let Some(s) = app.watchdog.get_mut(run.id) {
            s.on_live_summary_event(std::time::Instant::now());
            assert!(
                s.warned,
                "AC6: warned flag must persist across summary writes"
            );
        }

        // Stage 3: stall again past the kill threshold. Kill fires; no second
        // warning.
        fast_forward_idle(&mut app, run.id, super::watchdog::KILL_AFTER_NORMAL);
        app.tick_watchdog();
        let inputs = crate::runner::drain_test_input_receiver_for(&window_name);
        assert!(
            inputs.is_empty(),
            "AC6: no second warning must be sent after recovery; got {inputs:?}",
        );
        let cancels = crate::runner::drain_test_cancel_receiver_for(&window_name);
        assert_eq!(
            cancels,
            vec!["terminate"],
            "AC6: kill must still fire on the second stall"
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_warning_falls_back_when_prompt_cannot_be_read() {
    with_temp_root(|| {
        let session_id = "watchdog-degraded-fallback";
        let mut state = coder_round_state(session_id);
        let run = make_coder_run(80, 1, 1);
        let window_name = run.window_name.clone();
        state.agent_runs.push(run.clone());
        let mut app = idle_app(state);
        app.current_run_id = Some(run.id);
        app.run_launched = true;

        crate::runner::request_run_label_active_for_test(&window_name);
        // Point at a prompt path that does not exist on disk.
        let missing_path = session_state::session_dir(session_id)
            .join("prompts")
            .join("does-not-exist.md");
        install_watchdog_run(
            &mut app,
            run.id,
            &window_name,
            missing_path,
            EffortLevel::Normal,
        );
        fast_forward_idle(&mut app, run.id, super::watchdog::WARN_AFTER_NORMAL);

        app.tick_watchdog();

        let inputs = crate::runner::drain_test_input_receiver_for(&window_name);
        assert_eq!(inputs.len(), 1, "warning must still fire on read failure");
        assert!(
            inputs[0]
                .1
                .contains(super::watchdog::PROMPT_UNAVAILABLE_BODY),
            "fallback body must use the documented degraded message",
        );

        crate::runner::cancel_run_labels_matching(&window_name);
    });
}

#[test]
fn watchdog_register_uses_compressed_threshold_from_env() {
    with_temp_root(|| {
        // SAFETY: `with_temp_root` serializes test-global env mutations via
        // `test_fs_lock`, so this set/unset window is visible only to this
        // test's `WatchdogRegistry::from_env()` call.
        let prev = std::env::var_os(super::watchdog::SCALE_ENV_VAR);
        unsafe {
            std::env::set_var(super::watchdog::SCALE_ENV_VAR, "1000000");
        }
        let registry = super::watchdog::WatchdogRegistry::from_env();
        let mut registry = registry;
        let now = Instant::now();
        registry.register(
            1,
            EffortLevel::Normal,
            "[scaled]".to_string(),
            std::path::PathBuf::from("/p"),
            now,
        );
        let state = registry.get(1).expect("registered");
        // 600 simulated seconds × 1_000_000 ns/s = 600 ms real wall clock.
        assert_eq!(state.warn_threshold, Duration::from_millis(600));
        assert_eq!(state.kill_threshold, Duration::from_millis(1200));
        assert_eq!(state.warning_remaining_minutes, 10);

        unsafe {
            match prev {
                Some(v) => std::env::set_var(super::watchdog::SCALE_ENV_VAR, v),
                None => std::env::remove_var(super::watchdog::SCALE_ENV_VAR),
            }
        }
    });
}
