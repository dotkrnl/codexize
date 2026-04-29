// test_harness.rs
use super::*;
use super::{
    prompts::write_review_scope_artifact,
    tree::{build_tree, current_node_index, node_at_path, node_key_at_path},
};
use crate::{
    adapters::EffortLevel,
    selection::{self, ranking::build_version_index},
    state::{PendingGuardDecision, Phase, RunRecord, RunStatus, SessionState},
};

pub(super) fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
    let _guard = crate::state::test_fs_lock()
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let temp = tempfile::TempDir::new().expect("tempdir");
    let prev = std::env::var_os("CODEXIZE_ROOT");

    // SAFETY: env mutation is serialized by `test_fs_lock`.
    unsafe {
        std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    unsafe {
        match prev {
            Some(v) => std::env::set_var("CODEXIZE_ROOT", v),
            None => std::env::remove_var("CODEXIZE_ROOT"),
        }
    }
    result.expect("test panicked")
}

pub(super) fn mk_tmux() -> TmuxContext {
    TmuxContext {
        session_name: "test".to_string(),
        window_index: "0".to_string(),
        window_name: "test".to_string(),
    }
}

pub(super) fn mk_state_with_runs() -> SessionState {
    let mut state = SessionState::new("t".to_string());
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
    state
}

pub(super) fn coder_round_state(session_id: &str) -> SessionState {
    let mut state = SessionState::new(session_id.to_string());
    state.current_phase = Phase::ImplementationRound(1);
    state.builder.current_task = Some(1);
    state
}

pub(super) fn mk_app(state: SessionState) -> App {
    let nodes = build_tree(&state);
    let current = current_node_index(&nodes);
    let selected_key = node_key_at_path(&nodes, &[current]);
    let mut app = App {
        tmux: mk_tmux(),
        state,
        nodes,
        visible_rows: Vec::new(),
        models: Vec::new(),
        versions: build_version_index(&[]),
        model_refresh: ModelRefreshState::Idle(Instant::now()),
        selected: 0,
        selected_key,
        collapsed_overrides: BTreeMap::new(),
        viewport_top: 0,
        follow_tail: true,
        explicit_viewport_scroll: false,
        progress_follow_active: true,
        tail_detach_baseline: None,
        body_inner_height: 30,
        body_inner_width: 80,
        input_mode: false,
        input_buffer: String::new(),
        input_cursor: 0,
        pending_view_path: None,
        confirm_back: false,
        window_launched: true,
        quota_errors: Vec::new(),
        quota_retry_delay: Duration::from_secs(60),
        agent_line_count: 0,
        agent_content_hash: 0,
        agent_last_change: None,
        spinner_tick: 0,
        live_summary_spinner_visible: false,
        live_summary_watcher: None,
        live_summary_change_rx: None,
        live_summary_path: None,
        live_summary_cached_text: String::new(),
        live_summary_cached_mtime: None,
        pending_drain_deadline: None,
        current_run_id: Some(2),
        failed_models: HashMap::new(),
        pending_yolo_toggle_gate: None,
        yolo_exit_issued: HashSet::new(),
        yolo_exit_observations: HashMap::new(),
        test_launch_harness: None,
        messages: Vec::new(),
        status_line: Rc::new(RefCell::new(status_line::StatusLine::new())),
        prev_models_mode: models_area::ModelsAreaMode::default(),
        palette: palette::PaletteState::default(),
    };
    app.rebuild_visible_rows();
    app.restore_selection(app.selected_key.clone(), app.selected);
    app
}

pub(super) fn make_coder_run(id: u64, round: u32, attempt: u32) -> RunRecord {
    RunRecord {
        id,
        stage: "coder".to_string(),
        task_id: Some(1),
        round,
        attempt,
        model: "gpt-5".to_string(),
        vendor: "codex".to_string(),
        window_name: format!("[Builder t1 r{round}]"),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    }
}

pub(super) fn make_planning_run(id: u64, attempt: u32) -> RunRecord {
    RunRecord {
        id,
        stage: "planning".to_string(),
        task_id: None,
        round: 1,
        attempt,
        model: "gpt-5".to_string(),
        vendor: "codex".to_string(),
        window_name: format!("[Planning a{attempt}]"),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    }
}

pub(super) fn make_stage_run(id: u64, stage: &str, round: u32, attempt: u32) -> RunRecord {
    RunRecord {
        id,
        stage: stage.to_string(),
        task_id: None,
        round,
        attempt,
        model: "gpt-5".to_string(),
        vendor: "codex".to_string(),
        window_name: format!("[{stage} r{round} a{attempt}]"),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    }
}

pub(super) fn write_review_scope(round_dir: &std::path::Path, base_sha: &str) {
    write_review_scope_artifact(round_dir, base_sha).expect("write review scope");
}

pub(super) fn write_finish_stamp(
    session_dir: &std::path::Path,
    run_key: &str,
    head_after: &str,
    head_state: &str,
) {
    let stamp = crate::runner::FinishStamp {
        finished_at: chrono::Utc::now().to_rfc3339(),
        exit_code: 0,
        head_before: "base123".to_string(),
        head_after: head_after.to_string(),
        head_state: head_state.to_string(),
        signal_received: String::new(),
        working_tree_clean: true,
    };
    let stamp_path = session_dir
        .join("artifacts")
        .join("run-finish")
        .join(format!("{run_key}.toml"));
    crate::runner::write_finish_stamp(&stamp_path, &stamp).expect("write finish stamp");
}

pub(super) fn build_progress_follow_app(state: SessionState, current_run_id: u64) -> App {
    let mut app = mk_app(state);
    app.current_run_id = Some(current_run_id);
    app.rebuild_tree_view(None);
    app.maybe_refocus_to_progress();
    app
}

pub(super) fn row_index(app: &App, label: &str) -> usize {
    row_index_opt(app, label).expect("row")
}

pub(super) fn row_index_opt(app: &App, label: &str) -> Option<usize> {
    app.visible_rows
        .iter()
        .position(|row| node_at_path(&app.nodes, &row.path).is_some_and(|node| node.label == label))
}

pub(super) fn row_label(app: &App, index: usize) -> String {
    app.node_for_row(index)
        .map(|node| node.label.clone())
        .unwrap_or_default()
}

/// Map a 1-based rank to an axis score that produces a probability gap
/// large enough for `pick_for_phase`'s relative cutoff (1/3) to deterministically
/// keep the rank-1 model and discard the rest. With role_score_exponent = 3:
///   1.0³ = 1.0     → kept
///   0.6³ = 0.216   → 0.216 < 1/3, excluded
///   0.4³ = 0.064   → excluded
pub(super) fn rank_to_axis_score_inner(rank: u8) -> f64 {
    match rank {
        1 => 1.0,
        2 => 0.6,
        3 => 0.4,
        _ => 0.3,
    }
}

pub(super) fn sample_model(name: &str, idea_rank: u8, build_rank: u8) -> selection::CachedModel {
    let idea = rank_to_axis_score_inner(idea_rank);
    let build = rank_to_axis_score_inner(build_rank);
    selection::CachedModel {
        vendor: selection::VendorKind::Claude,
        name: name.to_string(),
        overall_score: 7.0,
        current_score: 7.0,
        standard_error: 2.0,
        axes: vec![
            // Build axes — disjoint from Idea axes.
            ("codequality".to_string(), build),
            ("correctness".to_string(), build),
            ("debugging".to_string(), build),
            ("safety".to_string(), build),
            // Idea axes.
            ("complexity".to_string(), idea),
            ("edgecases".to_string(), idea),
            ("contextawareness".to_string(), idea),
            ("taskcompletion".to_string(), idea),
        ],
        axis_provenance: std::collections::BTreeMap::new(),
        quota_percent: Some(80),
        display_order: 0,
        fallback_from: None,
    }
}

pub(super) fn ranked_model(
    vendor: selection::VendorKind,
    name: &str,
    planning_rank: u8,
    build_rank: u8,
    review_rank: u8,
) -> selection::CachedModel {
    let build = rank_to_axis_score_inner(build_rank);
    let planning = rank_to_axis_score_inner(planning_rank);
    let review = rank_to_axis_score_inner(review_rank);
    // REVIEWER: "correctness" / "debugging" / "safety" / "edgecases" / "stability"
    // are shared across multiple phases. Existing `ranked_model` callers only
    // exercise the Build phase (planning_rank/review_rank are typically 10),
    // so we bias the shared axes toward the Build score and use Planning /
    // Review scores only for axes unique to those phases.
    selection::CachedModel {
        vendor,
        name: name.to_string(),
        overall_score: 7.0,
        current_score: 7.0,
        standard_error: 2.0,
        axes: vec![
            ("codequality".to_string(), build),
            ("correctness".to_string(), build),
            ("debugging".to_string(), build),
            ("safety".to_string(), build),
            ("complexity".to_string(), planning),
            ("edgecases".to_string(), planning),
            ("stability".to_string(), review),
            ("contextawareness".to_string(), 0.3),
            ("taskcompletion".to_string(), 0.3),
        ],
        axis_provenance: std::collections::BTreeMap::new(),
        quota_percent: Some(80),
        display_order: 0,
        fallback_from: None,
    }
}

pub(super) fn idle_app(state: SessionState) -> App {
    let nodes = build_tree(&state);
    let current = current_node_index(&nodes);
    let selected_key = node_key_at_path(&nodes, &[current]);
    let mut app = App {
        tmux: mk_tmux(),
        state,
        nodes,
        visible_rows: Vec::new(),
        models: Vec::new(),
        versions: build_version_index(&[]),
        model_refresh: ModelRefreshState::Idle(Instant::now()),
        selected: 0,
        selected_key,
        collapsed_overrides: BTreeMap::new(),
        viewport_top: 0,
        follow_tail: true,
        explicit_viewport_scroll: false,
        progress_follow_active: true,
        tail_detach_baseline: None,
        body_inner_height: 30,
        body_inner_width: 80,
        input_mode: false,
        input_buffer: String::new(),
        input_cursor: 0,
        pending_view_path: None,
        confirm_back: false,
        window_launched: false,
        quota_errors: Vec::new(),
        quota_retry_delay: Duration::from_secs(60),
        agent_line_count: 0,
        agent_content_hash: 0,
        agent_last_change: None,
        spinner_tick: 0,
        live_summary_spinner_visible: false,
        live_summary_watcher: None,
        live_summary_change_rx: None,
        live_summary_path: None,
        live_summary_cached_text: String::new(),
        live_summary_cached_mtime: None,
        pending_drain_deadline: None,
        current_run_id: None,
        failed_models: HashMap::new(),
        pending_yolo_toggle_gate: None,
        yolo_exit_issued: HashSet::new(),
        yolo_exit_observations: HashMap::new(),
        test_launch_harness: None,
        messages: Vec::new(),
        status_line: Rc::new(RefCell::new(status_line::StatusLine::new())),
        prev_models_mode: models_area::ModelsAreaMode::default(),
        palette: palette::PaletteState::default(),
    };
    app.rebuild_visible_rows();
    app.restore_selection(app.selected_key.clone(), app.selected);
    app
}

pub(super) fn make_brainstorm_run(id: u64) -> RunRecord {
    RunRecord {
        id,
        stage: "brainstorm".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        model: "test-model".to_string(),
        vendor: "test".to_string(),
        window_name: "[Brainstorm]".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        status: RunStatus::Running,
        error: None,
        effort: EffortLevel::Normal,
        modes: crate::state::LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
    }
}

pub(super) fn write_ask_operator_snapshot(session_dir: &std::path::Path) {
    let guard_dir = session_dir.join(".guards").join("brainstorm-stage-r1-a1");
    std::fs::create_dir_all(&guard_dir).expect("guard dir");
    std::fs::write(
            guard_dir.join("snapshot.toml"),
            "head = \"0000000000000000000000000000000000000000\"\ngit_status = \"\"\nmode = \"ask_operator\"\n\n[control_files]\n",
        )
        .expect("write snapshot");
}

pub(super) fn make_pending_guard_state(session_id: &str, run_id: u64) -> SessionState {
    let mut state = SessionState::new(session_id.to_string());
    state.current_phase = Phase::GitGuardPending;
    state.agent_runs.push(make_brainstorm_run(run_id));
    state.pending_guard_decision = Some(PendingGuardDecision {
        stage: "brainstorm".to_string(),
        task_id: None,
        round: 1,
        attempt: 1,
        run_id,
        captured_head: "abc123".to_string(),
        current_head: "def456".to_string(),
        warnings: vec![],
    });
    state
}

pub(super) fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
}
