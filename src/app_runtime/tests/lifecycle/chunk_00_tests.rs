use super::*;

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
fn live_summary_fallback_polls_even_when_watcher_has_no_event() {
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
        app.live_summary_change_events = Some(crate::data::events::LiveSummaryEvents::new(rx));

        app.poll_live_summary_fallback();

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
