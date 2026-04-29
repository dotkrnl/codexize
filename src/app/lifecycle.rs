// lifecycle.rs
use super::*;
use crate::{
    artifacts::{ArtifactKind, Spec},
    cache,
    selection::{self, ranking::build_version_index},
    state::{self as session_state, MessageKind, Node, NodeStatus, Phase, RunStatus, SessionState},
    tasks,
    tmux::TmuxContext,
    tui::AppTerminal,
};
use anyhow::Result;
use crossterm::event::{self, Event};

use super::{
    models::spawn_refresh,
    prompts::*,
    state::ModelRefreshState,
    tree::{
        NodeKey, active_path_keys, build_tree, current_node_index, deepest_path_for_run,
        flatten_visible_rows, node_at_path, node_key_at_path,
    },
};

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant},
};
impl App {
    pub(super) fn active_modal(&self) -> Option<ModalKind> {
        match self.state.current_phase {
            Phase::SkipToImplPending => Some(ModalKind::SkipToImpl),
            Phase::GitGuardPending => Some(ModalKind::GitGuard),
            Phase::SpecReviewPaused => Some(ModalKind::SpecReviewPaused),
            Phase::PlanReviewPaused => Some(ModalKind::PlanReviewPaused),
            Phase::BrainstormRunning if self.state.agent_error.is_some() => {
                Some(ModalKind::StageError(StageId::Brainstorm))
            }
            Phase::SpecReviewRunning if self.state.agent_error.is_some() => {
                Some(ModalKind::StageError(StageId::SpecReview))
            }
            Phase::PlanningRunning if self.state.agent_error.is_some() => {
                Some(ModalKind::StageError(StageId::Planning))
            }
            Phase::PlanReviewRunning if self.state.agent_error.is_some() => {
                Some(ModalKind::StageError(StageId::PlanReview))
            }
            Phase::ShardingRunning if self.state.agent_error.is_some() => {
                Some(ModalKind::StageError(StageId::Sharding))
            }
            Phase::ImplementationRound(_) if self.state.agent_error.is_some() => {
                Some(ModalKind::StageError(StageId::Implementation))
            }
            Phase::ReviewRound(_) if self.state.agent_error.is_some() => {
                Some(ModalKind::StageError(StageId::Review))
            }
            _ => None,
        }
    }

    pub fn new(tmux: TmuxContext, mut state: SessionState) -> Self {
        let messages = SessionState::load_messages(&state.session_id).unwrap_or_default();
        if state.builder.task_titles.is_empty() {
            let tasks_path = session_state::session_dir(&state.session_id)
                .join("artifacts")
                .join("tasks.toml");
            if let Ok(parsed) = tasks::validate(&tasks_path) {
                session_state::transitions::load_task_titles_if_empty(
                    &mut state,
                    parsed.tasks.into_iter().map(|t| (t.id, t.title)),
                );
            }
        }
        let nodes = build_tree(&state);
        let current = current_node_index(&nodes);
        let selected_key = node_key_at_path(&nodes, &[current]);
        let failed_models = Self::rebuild_failed_models(&state);
        let mut app = Self {
            tmux,
            state,
            nodes,
            visible_rows: Vec::new(),
            models: Vec::new(),
            versions: build_version_index(&[]),
            model_refresh: ModelRefreshState::Fetching {
                rx: spawn_refresh(),
                started_at: Instant::now(),
            },
            selected: current,
            selected_key,
            collapsed_overrides: BTreeMap::new(),
            viewport_top: 0,
            follow_tail: true,
            explicit_viewport_scroll: false,
            progress_follow_active: true,
            tail_detach_baseline: None,
            body_inner_height: 0,
            body_inner_width: 0,
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
            live_summary_path: None,
            live_summary_watcher: None,
            live_summary_change_rx: None,
            live_summary_cached_text: String::new(),
            live_summary_cached_mtime: None,
            pending_drain_deadline: None,
            current_run_id: None,
            failed_models,
            pending_yolo_toggle_gate: None,
            yolo_exit_issued: HashSet::new(),
            yolo_exit_observations: HashMap::new(),
            #[cfg(test)]
            test_launch_harness: None,
            messages,
            status_line: Rc::new(RefCell::new(status_line::StatusLine::new())),
            prev_models_mode: models_area::ModelsAreaMode::default(),
            palette: palette::PaletteState::default(),
        };
        app.rebuild_visible_rows();
        app.restore_selection(app.selected_key.clone(), app.selected);
        // Populate the model strip immediately from whatever the cache holds.
        // The background refresh spawned above will replace this if any section
        // is expired.
        let loaded = cache::load();
        let cached = selection::assemble::assemble_from_loaded(&loaded);
        if !cached.is_empty() {
            let cache_has_expired_section = startup_cache_has_expired_section(&loaded);
            app.set_models(cached);
            if !cache_has_expired_section {
                app.model_refresh = ModelRefreshState::Idle(Instant::now());
            }
        }
        if let Ok(output) = std::process::Command::new("tmux")
            .args(["list-windows", "-F", "#{window_name}"])
            .output()
        {
            let live_windows = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>();
            if let Ok(run_id) =
                session_state::transitions::resume_running_runs(&mut app.state, &live_windows)
            {
                app.current_run_id = run_id;
                app.window_launched = run_id.is_some();
                if let Some(rid) = run_id {
                    if let Some(run) = app.state.agent_runs.iter().find(|r| r.id == rid).cloned() {
                        app.live_summary_path = Some(app.live_summary_path_for(&run));
                        app.prime_yolo_exit_tracking(&run);
                    }
                    app.read_live_summary_pipeline();
                }
                app.messages =
                    SessionState::load_messages(&app.state.session_id).unwrap_or_default();
                app.rebuild_tree_view(None);
                app.maybe_refocus_to_progress();
            }
        }
        // Resume validation: if the session was interrupted mid-guard-decision,
        // restore the modal or fail closed.
        if app.state.current_phase == Phase::GitGuardPending {
            if app.state.pending_guard_decision.is_none() {
                app.record_agent_error("guard pending state missing on resume".to_string());
                app.clear_builder_recovery_context();
                let _ = app.transition_to_phase(Phase::BlockedNeedsUser);
                let _ = app.state.save();
            }
        } else if app.state.pending_guard_decision.is_some() {
            // Stale: pending decision with no matching phase — clear it.
            let _ = app.state.log_event(
                "warning: clearing stale pending_guard_decision (phase mismatch on resume)"
                    .to_string(),
            );
            session_state::transitions::clear_pending_guard_decision(&mut app.state);
            let _ = app.state.save();
        }
        // Orphan sweep: remove stale live_summary.*.txt files that do not
        // correspond to a Running run record.
        {
            let artifacts_dir = session_state::session_dir(&app.state.session_id).join("artifacts");
            let running_keys: std::collections::HashSet<String> = app
                .state
                .agent_runs
                .iter()
                .filter(|run| run.status == crate::state::RunStatus::Running)
                .map(|run| App::run_key_for(&run.stage, run.task_id, run.round, run.attempt))
                .collect();
            if let Ok(entries) = std::fs::read_dir(&artifacts_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str == "live_summary.txt" {
                        let _ = std::fs::remove_file(entry.path());
                        continue;
                    }
                    if name_str.starts_with("live_summary.")
                        && name_str.ends_with(".txt")
                        && let Some(run_key) = name_str
                            .strip_prefix("live_summary.")
                            .and_then(|s| s.strip_suffix(".txt"))
                        && !running_keys.contains(run_key)
                    {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
        // Stamp archival: move old finish stamps to archive/ at session start.
        // Stamps older than the oldest Running record are archived (best effort).
        {
            let finish_dir = session_state::session_dir(&app.state.session_id)
                .join("artifacts")
                .join("run-finish");
            let archive_dir = finish_dir.join("archive");
            let oldest_running_timestamp = app
                .state
                .agent_runs
                .iter()
                .filter(|run| run.status == crate::state::RunStatus::Running)
                .map(|run| run.started_at)
                .min();
            if let Some(cutoff) = oldest_running_timestamp
                && let Ok(entries) = std::fs::read_dir(&finish_dir)
            {
                for entry in entries.flatten() {
                    if !entry.path().is_file() {
                        continue;
                    }
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if !name_str.ends_with(".toml") {
                        continue;
                    }
                    if let Ok(stamp) = crate::runner::read_finish_stamp(&entry.path())
                        && let Ok(finished) =
                            chrono::DateTime::parse_from_rfc3339(&stamp.finished_at)
                    {
                        let finished_utc = finished.with_timezone(&chrono::Utc);
                        if finished_utc < cutoff {
                            let _ = std::fs::create_dir_all(&archive_dir);
                            let dest = archive_dir.join(&name);
                            let _ = std::fs::rename(entry.path(), dest);
                        }
                    }
                }
            }
        }
        let _ = app.setup_watcher();
        app
    }

    pub fn run(&mut self, terminal: &mut AppTerminal) -> Result<()> {
        loop {
            if let Some(path) = self.pending_view_path.take() {
                let banner_inserted = prepend_review_banner(&path);
                let _ = crate::tui::run_foreground(terminal, || {
                    let _ = std::process::Command::new(
                        std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string()),
                    )
                    .arg(&path)
                    .status();
                    Ok(())
                });
                if banner_inserted {
                    let _ = strip_review_banner(&path);
                }
            }
            self.refresh_models_if_due();
            self.poll_agent_window();
            self.maybe_yolo_auto_resolve();
            self.maybe_auto_launch();
            self.update_agent_progress();
            self.process_live_summary_changes();
            terminal.draw(|frame| self.draw(frame))?;
            self.on_frame_drawn();

            if event::poll(self.event_poll_duration())?
                && let Event::Key(key) = event::read()?
                && self.handle_key(key)
            {
                return Ok(());
            }
        }
    }

    /// Called once per successful frame draw to advance spinner-driven UI state.
    pub(crate) fn on_frame_drawn(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
    }

    pub(crate) fn event_poll_duration(&self) -> Duration {
        if self.live_summary_spinner_visible {
            Duration::from_millis(LIVE_SUMMARY_EVENT_POLL_MS)
        } else {
            Duration::from_millis(DEFAULT_EVENT_POLL_MS)
        }
    }

    /// Returns a clone of the shared `StatusLine` handle so non-render call
    /// sites can push messages.
    #[allow(dead_code)]
    pub(super) fn status_line_handle(&self) -> Rc<RefCell<status_line::StatusLine>> {
        self.status_line.clone()
    }

    pub(super) fn current_node(&self) -> usize {
        current_node_index(&self.nodes)
    }

    #[cfg(test)]
    pub(super) fn current_row(&self) -> usize {
        let current = self.current_node();
        self.visible_rows
            .iter()
            .position(|row| row.depth == 0 && row.path.first().copied() == Some(current))
            .unwrap_or(0)
    }

    pub(super) fn node_for_row(&self, index: usize) -> Option<&Node> {
        let row = self.visible_rows.get(index)?;
        node_at_path(&self.nodes, &row.path)
    }

    pub(super) fn default_selected_key(&self) -> Option<NodeKey> {
        let current = self.current_node();
        node_key_at_path(&self.nodes, &[current])
    }

    pub(super) fn active_path_keys(&self) -> BTreeSet<NodeKey> {
        active_path_keys(&self.nodes, &self.state.agent_runs)
    }

    pub(super) fn rebuild_visible_rows(&mut self) {
        let active_keys = self.active_path_keys();
        let current = self.current_node();
        let overrides = self.collapsed_overrides.clone();
        self.visible_rows = flatten_visible_rows(&self.nodes, |row| {
            effective_expansion(row, current, &active_keys, &overrides)
        });
    }

    pub(super) fn restore_selection(
        &mut self,
        preferred_key: Option<NodeKey>,
        previous_selected: usize,
    ) {
        self.explicit_viewport_scroll = false;
        let target = preferred_key.or_else(|| self.default_selected_key());
        if let Some(key) = target {
            if let Some(index) = self.visible_rows.iter().position(|row| row.key == key) {
                self.selected = index;
                self.selected_key = Some(key);
                return;
            }
            if let Some(index) = key
                .ancestors()
                .find_map(|ancestor| self.visible_rows.iter().position(|row| row.key == ancestor))
            {
                self.selected = index;
                self.selected_key = self.visible_rows.get(index).map(|row| row.key.clone());
                return;
            }
        }

        self.selected = previous_selected.min(self.visible_rows.len().saturating_sub(1));
        self.selected_key = self
            .visible_rows
            .get(self.selected)
            .map(|row| row.key.clone());
    }

    pub(super) fn rebuild_tree_view(&mut self, preferred_key: Option<NodeKey>) {
        let previous_selected = self.selected;
        let preferred_key = preferred_key.or_else(|| self.selected_key.clone());

        self.nodes = build_tree(&self.state);
        self.rebuild_visible_rows();
        self.restore_selection(preferred_key, previous_selected);
    }

    /// Derive the preferred row for automatic progress follow.
    ///
    /// Resolution order: deepest node backing the current run id when that
    /// run is still `Running`, then the current top-level pipeline stage.
    /// The status check matters during rewinds (`go_back`) and other paths
    /// that finalize the run before clearing `current_run_id` — without it,
    /// a refocus event fired in that window would land on the just-aborted
    /// row instead of the new active stage. Returns `None` only when the
    /// pipeline has no live stage (everything `Done`/`Skipped`), which lets
    /// callers leave `selected_key` alone on terminal phases.
    pub(super) fn progress_focus_key(&self) -> Option<NodeKey> {
        if let Some(run_id) = self.current_run_id
            && self
                .state
                .agent_runs
                .iter()
                .any(|run| run.id == run_id && run.status == RunStatus::Running)
            && let Some(path) = deepest_path_for_run(&self.nodes, run_id)
            && let Some(key) = node_key_at_path(&self.nodes, &path)
        {
            return Some(key);
        }
        let current = self.current_node();
        let active = self
            .nodes
            .get(current)
            .is_some_and(|node| !matches!(node.status, NodeStatus::Done | NodeStatus::Skipped));
        if active {
            return node_key_at_path(&self.nodes, &[current]);
        }
        None
    }

    /// Move focus to the row returned by `progress_focus_key` when progress
    /// follow is active. Reuses `restore_selection` so the collapsed-ancestor
    /// fallback matches normal selection recovery.
    pub(super) fn maybe_refocus_to_progress(&mut self) {
        if !self.progress_follow_active {
            return;
        }
        let Some(target) = self.progress_focus_key() else {
            return;
        };
        let previous_selected = self.selected;
        self.restore_selection(Some(target), previous_selected);
    }

    /// Re-enable progress-follow focus and immediately refocus. Called from
    /// the phase-transition and run-launch boundaries the spec treats as
    /// natural reset points after manual navigation.
    pub(super) fn enable_progress_follow_and_refocus(&mut self) {
        self.progress_follow_active = true;
        self.maybe_refocus_to_progress();
    }

    pub(super) fn can_focus_input(&self) -> bool {
        self.is_expanded(self.selected)
            && self.state.current_phase == Phase::IdeaInput
            && self
                .node_for_row(self.selected)
                .is_some_and(|node| node.label == "Idea")
    }

    pub(super) fn header_y_offsets(&self) -> (Vec<usize>, usize) {
        let mut ys = Vec::with_capacity(self.visible_rows.len());
        let mut y = 0usize;
        for i in 0..self.visible_rows.len() {
            ys.push(y);
            y += 1;
            if self.is_expanded_body(i) {
                y += self.node_body(i).len();
            }
        }
        (ys, y)
    }

    pub(super) fn running_depth_0_header(&self) -> Option<(usize, usize)> {
        let (ys, _) = self.header_y_offsets();
        let mut candidates = self
            .visible_rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.depth == 0)
            .filter_map(|(index, _)| {
                let node = self.node_for_row(index)?;
                (node.status == NodeStatus::Running).then_some((index, ys[index]))
            });
        let candidate = candidates.next()?;
        if candidates.next().is_some() {
            return None;
        }
        Some(candidate)
    }

    pub(super) fn pinned_running_header(&self, viewport_top: usize) -> Option<(usize, usize)> {
        self.running_depth_0_header()
            .filter(|(_, header_y)| *header_y < viewport_top)
    }

    pub(super) fn effective_body_height_for_top(
        &self,
        viewport_top: usize,
        body_height: usize,
    ) -> usize {
        if self.pinned_running_header(viewport_top).is_some() {
            body_height.saturating_sub(1)
        } else {
            body_height
        }
    }

    pub(super) fn effective_body_inner_height(&self) -> usize {
        self.effective_body_height_for_top(self.viewport_top, self.body_inner_height)
    }

    pub(super) fn max_viewport_top_for_height(&self, body_height: usize) -> usize {
        if body_height == 0 {
            return 0;
        }
        let (_, total) = self.header_y_offsets();
        let normal_max = total.saturating_sub(body_height);
        if self
            .running_depth_0_header()
            .is_some_and(|(_, header_y)| header_y < normal_max)
        {
            total.saturating_sub(body_height.saturating_sub(1))
        } else {
            normal_max
        }
    }

    pub(super) fn clamp_viewport(&mut self) {
        let area_h = self.body_inner_height;
        if area_h == 0 {
            self.viewport_top = 0;
            return;
        }
        let (ys, total) = self.header_y_offsets();
        let max_top = self.max_viewport_top_for_height(area_h);
        if self.follow_tail {
            self.viewport_top = max_top;
            self.explicit_viewport_scroll = false;
            return;
        }
        if !self.explicit_viewport_scroll
            && let Some(&header_y) = ys.get(self.selected)
        {
            let section_bottom = ys.get(self.selected + 1).copied().unwrap_or(total);
            // A first adjustment can move the viewport above a running header,
            // which activates pinning and reduces the content height by one.
            for _ in 0..2 {
                let effective_h = self.effective_body_height_for_top(self.viewport_top, area_h);
                // Keep any line of the selected section visible. This lets the user
                // scroll viewport_top through a tall body without the viewport snapping
                // back to the header on every render.
                if section_bottom <= self.viewport_top {
                    self.viewport_top = section_bottom.saturating_sub(1);
                } else if header_y >= self.viewport_top + effective_h {
                    self.viewport_top = header_y + 1 - effective_h;
                } else {
                    break;
                }
            }
        }
        self.viewport_top = self.viewport_top.min(max_top);
        if self.viewport_top >= max_top {
            self.set_follow_tail(true);
            self.explicit_viewport_scroll = false;
        }
    }

    pub(super) fn max_viewport_top(&self) -> usize {
        self.max_viewport_top_for_height(self.body_inner_height)
    }

    pub(super) fn scroll_viewport(&mut self, delta: isize, explicit: bool) {
        self.explicit_viewport_scroll = explicit;
        let max_top = self.max_viewport_top() as isize;
        let next = (self.viewport_top as isize + delta).clamp(0, max_top.max(0));
        self.viewport_top = next as usize;
        self.set_follow_tail(self.viewport_top as isize >= max_top);
        // Explicit paging (PageUp/PageDown today, equivalent mouse handlers
        // tomorrow) signals operator-driven browsing. Implicit scrolls from
        // arrow-key handoff or clamp_viewport do not.
        if explicit {
            self.progress_follow_active = false;
        }
    }

    /// Single writer for `follow_tail`. Tracks the message-count baseline so
    /// the unread-counter badge can compute "messages since detach".
    pub(super) fn set_follow_tail(&mut self, follow: bool) {
        if follow == self.follow_tail {
            return;
        }
        self.follow_tail = follow;
        self.tail_detach_baseline = if follow {
            None
        } else {
            Some(self.messages.len())
        };
        if follow {
            self.explicit_viewport_scroll = false;
        }
    }

    /// Pin every row that's currently effectively expanded as an explicit
    /// Expanded override. Called once per render so that whatever the user
    /// is currently looking at stays expanded across later state changes
    /// (e.g., the active stage rolling over to Done before a phase advance,
    /// which would otherwise drop it off the auto-expand active path).
    pub(super) fn latch_visible_expansions(&mut self) {
        let to_pin: Vec<NodeKey> = (0..self.visible_rows.len())
            .filter(|&i| self.is_expanded(i))
            .filter_map(|i| self.visible_rows.get(i).map(|row| row.key.clone()))
            .filter(|key| !self.collapsed_overrides.contains_key(key))
            .collect();
        for key in to_pin {
            self.collapsed_overrides
                .insert(key, ExpansionOverride::Expanded);
        }
    }

    pub(super) fn unread_below_count(&self) -> usize {
        match self.tail_detach_baseline {
            Some(baseline) => self.messages.len().saturating_sub(baseline),
            None => 0,
        }
    }

    pub(super) fn first_unread_rendered_line(&self) -> Option<usize> {
        let baseline = self.tail_detach_baseline?;
        if baseline >= self.messages.len() {
            return None;
        }

        let local_offset = chrono::Local::now().fixed_offset().offset().to_owned();
        let available_width = self.body_inner_width.max(1);
        let (ys, _) = self.header_y_offsets();

        (0..self.visible_rows.len())
            .filter(|&index| self.is_expanded_body(index))
            .filter_map(|index| {
                let node = self.node_for_row(index)?;
                let run_id = node.run_id.or(node.leaf_run_id)?;
                let run = self.state.agent_runs.iter().find(|run| run.id == run_id)?;
                let old_messages: Vec<_> = self
                    .messages
                    .iter()
                    .take(baseline)
                    .filter(|message| message.run_id == run_id)
                    .cloned()
                    .collect();
                let all_messages: Vec<_> = self
                    .messages
                    .iter()
                    .filter(|message| message.run_id == run_id)
                    .cloned()
                    .collect();

                if old_messages.len() == all_messages.len() {
                    return None;
                }

                let old_line_count = crate::app::chat_widget::message_lines(
                    &old_messages,
                    run,
                    &local_offset,
                    None,
                    available_width,
                )
                .len();
                let all_line_count = crate::app::chat_widget::message_lines(
                    &all_messages,
                    run,
                    &local_offset,
                    None,
                    available_width,
                )
                .len();

                (all_line_count > old_line_count).then_some(ys[index] + 1 + old_line_count)
            })
            .min()
    }

    pub(super) fn transition_to_phase(&mut self, next_phase: Phase) -> Result<()> {
        session_state::transitions::execute_transition(&mut self.state, next_phase)?;
        self.agent_line_count = 0;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        self.rebuild_tree_view(None);

        // Phase transitions are an automatic re-enable point for progress
        // follow: re-engage and snap focus to the running stage / latest run.
        // The collapsed-ancestor fallback inside `maybe_refocus_to_progress`
        // matches the pre-existing single-stage cursor move when no run is
        // active yet.
        self.enable_progress_follow_and_refocus();
        // Re-engage tail-follow on phase change so the new stage's transcript
        // streams into view.
        self.set_follow_tail(true);
        Ok(())
    }

    pub(super) fn record_agent_error(&mut self, message: impl Into<String>) {
        session_state::transitions::record_agent_error(&mut self.state, message);
    }

    pub(super) fn clear_agent_error(&mut self) {
        session_state::transitions::clear_agent_error(&mut self.state);
    }

    pub(super) fn clear_builder_recovery_context(&mut self) {
        session_state::transitions::clear_builder_recovery_context(&mut self.state);
    }

    pub fn accept_skip_to_implementation(&mut self) -> Result<()> {
        use crate::artifacts::SkipToImplKind;
        use crate::synthetic_artifacts::generate_synthetic_artifacts;
        use anyhow::Context;

        let session_dir = session_state::session_dir(&self.state.session_id);

        if self.state.skip_to_impl_kind == Some(SkipToImplKind::NothingToDo) {
            self.transition_to_phase(Phase::Done)?;
            self.state.save()?;
            return Ok(());
        }

        let spec_path = session_dir
            .join("artifacts")
            .join(ArtifactKind::Spec.filename());
        let spec_content = std::fs::read_to_string(&spec_path)
            .with_context(|| format!("cannot read {}", spec_path.display()))?;
        let parsed_spec = Spec {
            content: spec_content,
            spec_refs: vec![],
        };

        generate_synthetic_artifacts(&session_dir, &parsed_spec)?;

        // Initialize BuilderState similarly to ShardingRunning completion
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");
        let parsed_tasks = tasks::validate(&tasks_path)
            .with_context(|| format!("invalid {}", tasks_path.display()))?;

        session_state::transitions::initialize_task_pipeline(
            &mut self.state,
            parsed_tasks
                .tasks
                .iter()
                .map(|task| (task.id, task.title.clone())),
        );

        self.transition_to_phase(Phase::ImplementationRound(1))?;
        self.state.save()?; // Persist state after transition and builder setup

        Ok(())
    }

    pub fn decline_skip_to_implementation(&mut self) -> Result<()> {
        use crate::artifacts::SkipToImplKind;
        let kind = self.state.skip_to_impl_kind;
        session_state::transitions::clear_skip_to_impl_proposal(&mut self.state);
        let target = if kind == Some(SkipToImplKind::NothingToDo) {
            Phase::BrainstormRunning
        } else {
            Phase::SpecReviewRunning
        };
        self.transition_to_phase(target)?;
        self.state.save()?;
        Ok(())
    }

    pub fn accept_guard_reset(&mut self) -> Result<()> {
        let decision = session_state::transitions::take_pending_guard_decision(
            &mut self.state,
            "accept_guard_reset",
        )?;

        for w in &decision.warnings {
            self.append_system_message(decision.run_id, MessageKind::SummaryWarn, w.clone());
        }
        guard::reset_hard_to(&decision.captured_head);

        let run = self
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == decision.run_id)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("accept_guard_reset: run {} not found", decision.run_id)
            })?;

        let _ = self.state.save();
        self.complete_run_finalization(&run, Some("forbidden_head_advance".to_string()))
    }

    pub fn accept_guard_keep(&mut self) -> Result<()> {
        let decision = session_state::transitions::take_pending_guard_decision(
            &mut self.state,
            "accept_guard_keep",
        )?;

        for w in &decision.warnings {
            self.append_system_message(decision.run_id, MessageKind::SummaryWarn, w.clone());
        }
        self.append_system_message(
            decision.run_id,
            MessageKind::SummaryWarn,
            "operator kept unauthorized commit from interactive run".to_string(),
        );

        let run = self
            .state
            .agent_runs
            .iter()
            .find(|r| r.id == decision.run_id)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("accept_guard_keep: run {} not found", decision.run_id)
            })?;

        let _ = self.state.save();
        // Artifact was valid (PendingDecision only fires on valid-artifact path).
        // complete_run_finalization dispatches on current_phase; restore the
        // originating running phase so the correct success successor fires.
        let originating = match decision.stage.as_str() {
            "brainstorm" => Phase::BrainstormRunning,
            "planning" => Phase::PlanningRunning,
            "recovery" => Phase::BuilderRecovery(decision.round),
            other => anyhow::bail!("accept_guard_keep: unexpected stage '{other}'"),
        };
        session_state::transitions::restore_guard_originating_phase(&mut self.state, originating);
        self.complete_run_finalization(&run, None)
    }

    pub(super) fn editable_artifact(&self) -> Option<std::path::PathBuf> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let path = match self.state.current_phase {
            Phase::BrainstormRunning | Phase::SpecReviewRunning | Phase::SpecReviewPaused => {
                artifacts.join("spec.md")
            }
            Phase::PlanningRunning | Phase::PlanReviewRunning | Phase::PlanReviewPaused => {
                artifacts.join("plan.md")
            }
            Phase::ShardingRunning | Phase::BuilderRecoverySharding(_) => {
                artifacts.join("tasks.toml")
            }
            Phase::BuilderRecovery(_) => artifacts.join("tasks.toml"),
            Phase::BuilderRecoveryPlanReview(_) => artifacts.join("plan_review.toml"),
            Phase::ImplementationRound(r) | Phase::ReviewRound(r) => session_dir
                .join("rounds")
                .join(format!("{r:03}"))
                .join("task.toml"),
            Phase::IdeaInput
            | Phase::Done
            | Phase::BlockedNeedsUser
            | Phase::SkipToImplPending
            | Phase::GitGuardPending => {
                return None;
            }
        };
        if path.exists() { Some(path) } else { None }
    }

    pub(super) fn open_editable_artifact(&self) {
        let Some(path) = self.editable_artifact() else {
            return;
        };
        let path_str = path.display().to_string();
        let _ = std::process::Command::new("tmux")
            .args(["new-window", "-n", "[Edit]", &format!("vim {path_str}")])
            .output();
        let _ = std::process::Command::new("tmux")
            .args(["select-window", "-t", "[Edit]"])
            .output();
    }

    pub(super) fn queue_view_of_current_artifact(&mut self, filename: &str) {
        let path = session_state::session_dir(&self.state.session_id)
            .join("artifacts")
            .join(filename);
        if path.exists() {
            self.pending_view_path = Some(path);
        }
    }

    pub(super) fn can_go_back(&self) -> bool {
        !matches!(self.state.current_phase, Phase::IdeaInput | Phase::Done)
    }

    pub(super) fn go_back(&mut self) {
        use std::fs;

        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let prompts = session_dir.join("prompts");

        // Interrupt the running agent (if any) so rewinding takes effect even
        // when the phase-specific kill_window base doesn't match the launch
        // window name (e.g. "[Spec Review 1]" vs "[Spec Review]").
        if let Some(run_id) = self.current_run_id {
            let running = self
                .state
                .agent_runs
                .iter()
                .find(|r| r.id == run_id)
                .cloned();
            if let Some(run) = running {
                kill_window(&run.window_name);
                if run.status == crate::state::RunStatus::Running {
                    self.finalize_run_record(run_id, false, Some("aborted by user".to_string()));
                }
            }
        }

        match self.state.current_phase {
            Phase::BrainstormRunning => {
                kill_window("[Brainstorm]");
                let _ = fs::remove_file(artifacts.join("spec.md"));
                let _ = fs::remove_file(prompts.join("brainstorm.md"));
                self.clear_agent_error();
                let _ = self.transition_to_phase(Phase::IdeaInput);
            }
            Phase::SpecReviewRunning | Phase::SpecReviewPaused => {
                kill_window("[Spec Review]");
                let _ = fs::remove_file(artifacts.join("spec-review-1.md"));
                let _ = fs::remove_file(prompts.join("spec-review-1.md"));
                // TODO(Task 2): clean up all review artifacts by RunRecord instead of the
                // removed spec_reviewers/phase_models state.
                let _ = self.transition_to_phase(Phase::BrainstormRunning);
            }
            Phase::PlanningRunning => {
                kill_window("[Planning]");
                let _ = fs::remove_file(artifacts.join("plan.md"));
                let _ = self.transition_to_phase(Phase::SpecReviewRunning);
            }
            Phase::PlanReviewRunning => {
                kill_window("[Plan Review 1]");
                let _ = fs::remove_file(artifacts.join("plan-review-1.md"));
                let _ = fs::remove_file(prompts.join("plan-review-1.md"));
                let plan_backup = artifacts.join("plan.pre-review-1.md");
                let spec_backup = artifacts.join("spec.pre-review-1.md");
                restore_artifacts(&[
                    (plan_backup.as_path(), artifacts.join("plan.md").as_path()),
                    (spec_backup.as_path(), artifacts.join("spec.md").as_path()),
                ]);
                self.clear_agent_error();
                // TODO(Task 2): restore the paused/running distinction from RunRecord state.
                let _ = self.transition_to_phase(Phase::PlanningRunning);
            }
            Phase::PlanReviewPaused => {
                let plan_backup = artifacts.join("plan.pre-review-1.md");
                let spec_backup = artifacts.join("spec.pre-review-1.md");
                restore_artifacts(&[
                    (plan_backup.as_path(), artifacts.join("plan.md").as_path()),
                    (spec_backup.as_path(), artifacts.join("spec.md").as_path()),
                ]);
                let _ = fs::remove_file(artifacts.join("plan-review-1.md"));
                let _ = fs::remove_file(prompts.join("plan-review-1.md"));
                let _ = fs::remove_file(artifacts.join("plan.pre-review-1.md"));
                let _ = fs::remove_file(artifacts.join("spec.pre-review-1.md"));
                // TODO(Task 2): clean up all plan review artifacts by RunRecord history.
                let _ = self.transition_to_phase(Phase::PlanningRunning);
            }
            Phase::ShardingRunning => {
                kill_window("[Sharding]");
                let _ = fs::remove_file(artifacts.join("tasks.toml"));
                let _ = fs::remove_file(prompts.join("sharding.md"));
                // TODO(Task 2): remove sharding launch metadata from RunRecord instead of the
                // removed phase_models state.
                let _ = self.transition_to_phase(Phase::PlanReviewRunning);
            }
            Phase::ImplementationRound(r) => {
                kill_window(&format!("[Builder r{r}]"));
                let _ = fs::remove_dir_all(session_dir.join("rounds").join(format!("{r:03}")));
                let prev = if r <= 1 {
                    if self.state.skip_to_impl_rationale.is_some() {
                        Phase::BrainstormRunning
                    } else {
                        session_state::transitions::reset_builder_after_rewind(&mut self.state);
                        Phase::ShardingRunning
                    }
                } else {
                    Phase::ReviewRound(r - 1)
                };
                let _ = self.transition_to_phase(prev);
            }
            Phase::ReviewRound(r) => {
                kill_window(&format!("[Review r{r}]"));
                let _ = fs::remove_dir_all(session_dir.join("rounds").join(format!("{r:03}")));
                let _ = self.transition_to_phase(Phase::ImplementationRound(r));
            }
            Phase::BuilderRecovery(r) => {
                kill_window("[Recovery]");
                let _ = fs::remove_file(prompts.join(format!("recovery-r{r}.md")));
                // Recovery is builder-only and should not be rewound into coder/reviewer; go back to
                // the triggering review round so the operator can intervene.
                let _ = self.transition_to_phase(Phase::ReviewRound(r));
            }
            Phase::BuilderRecoveryPlanReview(r) => {
                kill_window("[Recovery Plan Review]");
                let _ = fs::remove_file(prompts.join(format!("recovery-plan-review-r{r}.md")));
                let _ = self.transition_to_phase(Phase::BuilderRecovery(r));
            }
            Phase::BuilderRecoverySharding(r) => {
                kill_window("[Recovery Sharding]");
                let _ = fs::remove_file(prompts.join(format!("recovery-sharding-r{r}.md")));
                let _ = self.transition_to_phase(Phase::BuilderRecoveryPlanReview(r));
            }
            Phase::SkipToImplPending => {
                kill_window("[Skip Confirm]");
                let _ = fs::remove_file(artifacts.join(ArtifactKind::SkipToImpl.filename()));
                session_state::transitions::clear_skip_to_impl_proposal(&mut self.state);
                self.clear_agent_error();
                let _ = self.transition_to_phase(Phase::BrainstormRunning);
            }
            Phase::GitGuardPending => {
                // No window owned by this phase; the modal is purely TUI.
                // Operator handlers are the legitimate exit path; go_back is
                // a no-op while the decision is pending.
            }
            Phase::IdeaInput | Phase::BlockedNeedsUser | Phase::Done => {}
        }

        self.clear_agent_error();
        self.window_launched = false;
        self.current_run_id = None;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        let _ = self.state.save();
    }

    pub(super) fn observed_path_state(path: &std::path::Path) -> ObservedPathState {
        match std::fs::metadata(path) {
            Ok(meta) => ObservedPathState {
                exists: true,
                modified_at: meta.modified().ok(),
            },
            Err(_) => ObservedPathState {
                exists: false,
                modified_at: None,
            },
        }
    }

    pub(super) fn update_agent_progress(&mut self) {
        let Some(run) = self.running_run() else {
            self.agent_line_count = 0;
            self.agent_content_hash = 0;
            self.agent_last_change = None;
            return;
        };
        let output = std::process::Command::new("tmux")
            .args(["capture-pane", "-t", &run.window_name, "-p", "-J"])
            .output();
        let Ok(out) = output else { return };
        let text = String::from_utf8_lossy(&out.stdout);
        self.agent_line_count = text.lines().filter(|l| !l.trim().is_empty()).count();

        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        let hash = hasher.finish();

        let now = Instant::now();
        if self.agent_content_hash == 0 || hash != self.agent_content_hash {
            self.agent_content_hash = hash;
            self.agent_last_change = Some(now);
            return;
        }
        // Preserve the 30s stall-detector probe; spinner progression is now
        // frame-driven and no longer depends on this branch.
        let _stalled = self
            .agent_last_change
            .map(|last| now.duration_since(last) >= Duration::from_secs(30));
    }

    /// Auto-launch the agent for the current phase if it's a non-interactive
    /// one (spec review, sharding, coder, reviewer). Idempotent: no-op if the
    /// window is already up, if models aren't loaded, or if the last run
    /// errored (user needs to intervene).
    pub(super) fn maybe_auto_launch(&mut self) {
        if self.window_launched || self.state.agent_error.is_some() || self.models.is_empty() {
            return;
        }
        match self.state.current_phase {
            Phase::BrainstormRunning => {
                if let Some(idea) = self.state.idea_text.clone() {
                    self.launch_brainstorm(idea);
                }
            }
            Phase::SpecReviewRunning => self.launch_spec_review(),
            Phase::PlanningRunning => self.launch_planning(),
            Phase::PlanReviewRunning => self.launch_plan_review(),
            Phase::ShardingRunning => self.launch_sharding(),
            Phase::ImplementationRound(_) => self.launch_coder(),
            Phase::ReviewRound(_) => self.launch_reviewer(),
            Phase::BuilderRecovery(_) => self.launch_recovery(),
            Phase::BuilderRecoveryPlanReview(_) => self.launch_recovery_plan_review(),
            Phase::BuilderRecoverySharding(_) => self.launch_recovery_sharding(),
            _ => {}
        }
    }

    pub(super) fn poll_agent_window(&mut self) {
        let Some(run_id) = self.current_run_id else {
            self.pending_drain_deadline = None;
            return;
        };
        let Some(run) = self
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
        else {
            self.pending_drain_deadline = None;
            return;
        };
        if self.window_exists(&run.window_name) {
            self.maybe_issue_yolo_exit(&run);
            self.pending_drain_deadline = None;
            return;
        }

        let deadline = *self
            .pending_drain_deadline
            .get_or_insert_with(|| Instant::now() + Self::stamp_timeout_duration());
        let now = Instant::now();
        let stamp_path = self.finish_stamp_path_for(&run);
        let stamp_present = Self::artifact_present(&stamp_path);
        let deadline_elapsed = now >= deadline;
        if !stamp_present && !deadline_elapsed {
            return;
        }
        if !stamp_present && deadline_elapsed && run.stage != "coder" {
            // Reviewer note: fallback warning is emitted once at barrier release
            // so non-coder runs keep legacy verdict behavior but remain diagnosable.
            self.append_system_message(
                run.id,
                MessageKind::SummaryWarn,
                format!(
                    "finish_stamp_missing: {} (continuing with existing {} verdict logic)",
                    stamp_path.display(),
                    run.stage
                ),
            );
        }

        self.pending_drain_deadline = None;
        self.window_launched = false;
        self.current_run_id = None;
        let outcome = self.finalize_current_run(&run);
        if let Err(err) = outcome {
            self.record_agent_error(err.to_string());
            let _ = self.state.log_event(format!(
                "run finalization failed for {}: {err}",
                run.window_name
            ));
        }
        self.rebuild_tree_view(None);
    }
}
