// tests_lifecycle.rs
use super::tree::node_at_path;
use super::*;
use super::{prompts::*, test_harness::*};
use crate::{
    adapters::EffortLevel,
    selection::{self},
    state::{
        self as session_state, MessageKind, PendingGuardDecision, Phase, PipelineItem,
        PipelineItemStatus, RunRecord, RunStatus, SessionState,
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
fn interactive_exit_is_handled_locally_without_quitting_tui() {
    with_temp_root(|| {
        let mut state = SessionState::new("interactive-exit-local".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        for c in "/exit".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        let should_quit = app.handle_key(key(crossterm::event::KeyCode::Enter));

        assert!(!should_quit);
        assert_eq!(app.current_run_id, Some(7));
        assert!(app.input_mode, "interactive input surface stays focused");
        assert!(!app.palette.open);
        assert!(app.input_buffer.is_empty());
    });
}

#[test]
fn interactive_palette_opens_only_after_colon() {
    with_temp_root(|| {
        let mut state = SessionState::new("interactive-palette-colon".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        app.handle_key(key(crossterm::event::KeyCode::Char('h')));
        assert!(app.input_mode);
        assert_eq!(app.input_buffer, "h");
        assert!(!app.palette.open);

        app.handle_key(key(crossterm::event::KeyCode::Char(':')));
        assert!(app.palette.open);
        assert!(app.palette.buffer.is_empty());
    });
}

#[test]
fn interactive_palette_closes_when_colon_suffix_is_removed() {
    with_temp_root(|| {
        let mut state = SessionState::new("interactive-palette-remove-colon".to_string());
        state.current_phase = Phase::BrainstormRunning;
        let mut run = make_brainstorm_run(7);
        run.modes.interactive = true;
        state.agent_runs.push(run);
        let mut app = idle_app(state);
        app.current_run_id = Some(7);

        for c in "hello".chars() {
            app.handle_key(key(crossterm::event::KeyCode::Char(c)));
        }
        app.handle_key(key(crossterm::event::KeyCode::Char(':')));

        assert!(app.palette.open);
        assert_eq!(app.input_buffer, "hello");
        assert!(app.palette.buffer.is_empty());

        app.handle_key(key(crossterm::event::KeyCode::Backspace));

        assert!(!app.palette.open);
        assert!(app.input_mode);
        assert_eq!(app.input_buffer, "hello");
    });
}

#[test]
fn pending_guard_modal_quit_keys_follow_quit_path() {
    with_temp_root(|| {
        let mut app = mk_app(make_pending_guard_state("pending-guard-key-quit", 32));

        assert!(app.handle_key(key(crossterm::event::KeyCode::Char('q'))));
        assert!(app.state.pending_guard_decision.is_some());

        let ctrl_c = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        );
        assert!(app.handle_key(ctrl_c));
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
        assert!(planning.contains("ASK the operator (this is interactive)."));
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
