pub mod chat_widget;
mod events;
mod guard;
mod models;
mod render;
mod state;
mod tree;

use crate::{
    adapters::{AgentRun, adapter_for_vendor, window_name_with_model},
    artifacts::{ArtifactKind, SkipToImplProposal, Spec},
    cache, review,
    runner::{launch_interactive, launch_noninteractive},
    selection::{
        self, CachedModel, QuotaError, VendorKind,
        config::SelectionPhase,
        ranking::{VersionIndex, build_version_index},
        selection::{pick_for_phase, select_excluding, select_for_review},
    },
    state::{
        self as session_state, Message, MessageKind, MessageSender, Node, PendingGuardDecision,
        Phase, PipelineItem, PipelineItemStatus, RunStatus, SessionState,
    },
    tasks, tmux,
    tmux::TmuxContext,
    tui::AppTerminal,
};
use anyhow::{Context, Result};
use crossterm::event::{self, Event};

use self::{
    models::{spawn_refresh, vendor_tag},
    state::ModelRefreshState,
    tree::{
        NodeKey, VisibleNodeRow, active_path_keys, build_tree, current_node_index,
        flatten_visible_rows, node_at_path, node_key_at_path,
    },
};

use notify::Watcher;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::mpsc,
    time::{Duration, Instant},
};

type RetryKey = (String, Option<u32>, u32);
type FailedModelSet = HashSet<(VendorKind, String)>;
const DEFAULT_STAMP_TIMEOUT_MS: u64 = 1500;
const ENV_STAMP_TIMEOUT_MS: &str = "CODEXIZE_STAMP_TIMEOUT_MS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpansionOverride {
    Expanded,
    Collapsed,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct TestLaunchOutcome {
    exit_code: i32,
    artifact_contents: Option<String>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct TestLaunchHarness {
    outcomes: std::collections::VecDeque<TestLaunchOutcome>,
}

#[derive(Debug)]
pub struct App {
    tmux: TmuxContext,
    state: SessionState,
    nodes: Vec<Node>,
    visible_rows: Vec<VisibleNodeRow>,
    models: Vec<CachedModel>,
    versions: VersionIndex,
    model_refresh: ModelRefreshState,
    selected: usize,
    selected_key: Option<NodeKey>,
    collapsed_overrides: BTreeMap<NodeKey, ExpansionOverride>,
    viewport_top: usize,
    follow_tail: bool,
    /// When true, the viewport was intentionally paged away from the focused
    /// row and clamp_viewport should not pull it back toward focus.
    explicit_viewport_scroll: bool,
    /// Snapshot of `messages.len()` taken when tail-follow was last
    /// disengaged. None while following. Used to count missed messages
    /// for the "↓ N new" badge.
    tail_detach_baseline: Option<usize>,
    body_inner_height: usize,
    body_inner_width: usize,
    input_mode: bool,
    input_buffer: String,
    input_cursor: usize,
    pending_view_path: Option<std::path::PathBuf>,
    confirm_back: bool,
    window_launched: bool,
    quota_errors: Vec<QuotaError>,
    quota_retry_delay: Duration,
    agent_line_count: usize,
    agent_content_hash: u64,
    agent_last_change: Option<Instant>,
    spinner_tick: usize,
    live_summary_watcher: Option<notify::RecommendedWatcher>,
    live_summary_change_rx: Option<mpsc::Receiver<()>>,
    live_summary_path: Option<std::path::PathBuf>,
    live_summary_cached_text: String,
    live_summary_cached_mtime: Option<std::time::SystemTime>,
    pending_drain_deadline: Option<Instant>,
    pending_drain_notice_emitted: bool,
    current_run_id: Option<u64>,
    failed_models: HashMap<RetryKey, FailedModelSet>,
    #[cfg(test)]
    test_launch_harness: Option<std::sync::Arc<std::sync::Mutex<TestLaunchHarness>>>,
    messages: Vec<Message>,
}

fn default_expansion(
    row: &VisibleNodeRow,
    current_node: usize,
    active_keys: &BTreeSet<NodeKey>,
) -> bool {
    if !row.is_expandable() {
        return false;
    }
    if row.depth == 0 {
        return row.path.first().copied() == Some(current_node);
    }
    active_keys.contains(&row.key)
}

fn effective_expansion(
    row: &VisibleNodeRow,
    current_node: usize,
    active_keys: &BTreeSet<NodeKey>,
    overrides: &BTreeMap<NodeKey, ExpansionOverride>,
) -> bool {
    if !row.is_expandable() {
        return false;
    }
    match overrides.get(&row.key) {
        Some(ExpansionOverride::Expanded) => true,
        Some(ExpansionOverride::Collapsed) => false,
        None => default_expansion(row, current_node, active_keys),
    }
}

fn startup_cache_has_expired_section(loaded: &cache::LoadedCache) -> bool {
    let dashboard_expired = loaded.dashboard.as_ref().map(|s| s.expired).unwrap_or(true);
    let quotas_expired = loaded.quotas.as_ref().map(|s| s.expired).unwrap_or(true);
    dashboard_expired || quotas_expired
}

impl App {
    pub fn new(tmux: TmuxContext, mut state: SessionState) -> Self {
        let messages = SessionState::load_messages(&state.session_id).unwrap_or_default();
        if state.builder.task_titles.is_empty() {
            let tasks_path = session_state::session_dir(&state.session_id)
                .join("artifacts")
                .join("tasks.toml");
            if let Ok(parsed) = tasks::validate(&tasks_path) {
                state.builder.task_titles =
                    parsed.tasks.into_iter().map(|t| (t.id, t.title)).collect();
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
            live_summary_path: None,
            live_summary_watcher: None,
            live_summary_change_rx: None,
            live_summary_cached_text: String::new(),
            live_summary_cached_mtime: None,
            pending_drain_deadline: None,
            pending_drain_notice_emitted: false,
            current_run_id: None,
            failed_models,
            #[cfg(test)]
            test_launch_harness: None,
            messages,
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
            if let Ok(run_id) = app.state.resume_running_runs(&live_windows) {
                app.current_run_id = run_id;
                app.window_launched = run_id.is_some();
                if let Some(rid) = run_id {
                    if let Some(run) = app.state.agent_runs.iter().find(|r| r.id == rid) {
                        app.live_summary_path = Some(app.live_summary_path_for(run));
                    }
                    app.read_live_summary_pipeline();
                }
                app.messages =
                    SessionState::load_messages(&app.state.session_id).unwrap_or_default();
                app.rebuild_tree_view(None);
            }
        }
        // Resume validation: if the session was interrupted mid-guard-decision,
        // restore the modal or fail closed.
        if app.state.current_phase == Phase::GitGuardPending {
            if app.state.pending_guard_decision.is_none() {
                app.state.agent_error = Some("guard pending state missing on resume".to_string());
                let _ = app.transition_to_phase(Phase::BlockedNeedsUser);
                let _ = app.state.save();
            }
        } else if app.state.pending_guard_decision.is_some() {
            // Stale: pending decision with no matching phase — clear it.
            let _ = app.state.log_event(
                "warning: clearing stale pending_guard_decision (phase mismatch on resume)"
                    .to_string(),
            );
            app.state.pending_guard_decision = None;
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

    fn rebuild_failed_models(state: &SessionState) -> HashMap<RetryKey, FailedModelSet> {
        let mut failed_models = HashMap::new();
        let cutoff = state.builder.retry_reset_run_id_cutoff;
        for run in state
            .agent_runs
            .iter()
            .filter(|run| matches!(run.status, RunStatus::Failed | RunStatus::FailedUnverified))
        {
            if run.error.as_deref() == Some("user_forced_retry") {
                continue;
            }
            if matches!(run.stage.as_str(), "coder" | "reviewer")
                && cutoff.is_some_and(|cutoff| run.id <= cutoff)
            {
                continue;
            }
            let Some(vendor) = selection::vendor::str_to_vendor(&run.vendor) else {
                continue;
            };
            failed_models
                .entry((run.stage.clone(), run.task_id, run.round))
                .or_insert_with(HashSet::new)
                .insert((vendor, run.model.clone()));
        }
        failed_models
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
            self.maybe_auto_launch();
            self.update_agent_progress();
            self.process_live_summary_changes();
            terminal.draw(|frame| self.draw(frame))?;

            if event::poll(Duration::from_millis(250))?
                && let Event::Key(key) = event::read()?
                && self.handle_key(key)
            {
                return Ok(());
            }
        }
    }

    fn current_node(&self) -> usize {
        current_node_index(&self.nodes)
    }

    fn current_row(&self) -> usize {
        let current = self.current_node();
        self.visible_rows
            .iter()
            .position(|row| row.depth == 0 && row.path.first().copied() == Some(current))
            .unwrap_or(0)
    }

    fn node_for_row(&self, index: usize) -> Option<&Node> {
        let row = self.visible_rows.get(index)?;
        node_at_path(&self.nodes, &row.path)
    }

    fn default_selected_key(&self) -> Option<NodeKey> {
        let current = self.current_node();
        node_key_at_path(&self.nodes, &[current])
    }

    fn active_path_keys(&self) -> BTreeSet<NodeKey> {
        active_path_keys(&self.nodes, &self.state.agent_runs)
    }

    fn default_expanded(&self, row: &VisibleNodeRow) -> bool {
        default_expansion(row, self.current_node(), &self.active_path_keys())
    }

    fn rebuild_visible_rows(&mut self) {
        let active_keys = self.active_path_keys();
        let current = self.current_node();
        let overrides = self.collapsed_overrides.clone();
        self.visible_rows = flatten_visible_rows(&self.nodes, |row| {
            effective_expansion(row, current, &active_keys, &overrides)
        });
    }

    fn restore_selection(&mut self, preferred_key: Option<NodeKey>, previous_selected: usize) {
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

    fn rebuild_tree_view(&mut self, preferred_key: Option<NodeKey>) {
        let previous_selected = self.selected;
        let preferred_key = preferred_key.or_else(|| self.selected_key.clone());

        self.nodes = build_tree(&self.state);
        self.rebuild_visible_rows();
        self.restore_selection(preferred_key, previous_selected);
    }

    fn can_focus_input(&self) -> bool {
        self.is_expanded(self.selected)
            && self.state.current_phase == Phase::IdeaInput
            && self
                .node_for_row(self.selected)
                .is_some_and(|node| node.label == "Idea")
    }

    pub(super) fn is_expanded(&self, index: usize) -> bool {
        let Some(row) = self.visible_rows.get(index) else {
            return false;
        };
        effective_expansion(
            row,
            self.current_node(),
            &self.active_path_keys(),
            &self.collapsed_overrides,
        )
    }

    pub(super) fn is_expanded_body(&self, index: usize) -> bool {
        self.is_expanded(index)
            && self
                .visible_rows
                .get(index)
                .is_some_and(|row| row.has_transcript || row.has_body)
    }

    /// Y-offset of each visible row's header within the unconstrained content stream,
    /// plus the total number of content rows.
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

    pub(super) fn clamp_viewport(&mut self) {
        let area_h = self.body_inner_height;
        if area_h == 0 {
            self.viewport_top = 0;
            return;
        }
        let (ys, total) = self.header_y_offsets();
        let max_top = total.saturating_sub(area_h);
        if self.follow_tail {
            self.viewport_top = max_top;
            self.explicit_viewport_scroll = false;
            return;
        }
        if !self.explicit_viewport_scroll
            && let Some(&header_y) = ys.get(self.selected)
        {
            let section_bottom = ys.get(self.selected + 1).copied().unwrap_or(total);
            // Keep any line of the selected section visible. This lets the user
            // scroll viewport_top through a tall body without the viewport snapping
            // back to the header on every render.
            if section_bottom <= self.viewport_top {
                self.viewport_top = section_bottom.saturating_sub(1);
            } else if header_y >= self.viewport_top + area_h {
                self.viewport_top = header_y + 1 - area_h;
            }
        }
        self.viewport_top = self.viewport_top.min(max_top);
        if self.viewport_top >= max_top {
            self.set_follow_tail(true);
            self.explicit_viewport_scroll = false;
        }
    }

    pub(super) fn max_viewport_top(&self) -> usize {
        let (_, total) = self.header_y_offsets();
        total.saturating_sub(self.body_inner_height)
    }

    pub(super) fn scroll_viewport(&mut self, delta: isize, explicit: bool) {
        self.explicit_viewport_scroll = explicit;
        let max_top = self.max_viewport_top() as isize;
        let next = (self.viewport_top as isize + delta).clamp(0, max_top.max(0));
        self.viewport_top = next as usize;
        self.set_follow_tail(self.viewport_top as isize >= max_top);
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
                    self.spinner_tick,
                    available_width,
                )
                .len();
                let all_line_count = crate::app::chat_widget::message_lines(
                    &all_messages,
                    run,
                    &local_offset,
                    self.spinner_tick,
                    available_width,
                )
                .len();

                (all_line_count > old_line_count).then_some(ys[index] + 1 + old_line_count)
            })
            .min()
    }

    fn transition_to_phase(&mut self, next_phase: Phase) -> Result<()> {
        self.state.transition_to(next_phase)?;
        self.agent_line_count = 0;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        self.rebuild_tree_view(None);

        // Move cursor to the row of the new current stage.
        if let Some(target) = node_key_at_path(&self.nodes, &[self.current_node()])
            && let Some(idx) = self.visible_rows.iter().position(|row| row.key == target)
        {
            self.selected = idx;
            self.selected_key = Some(target);
        }
        // Re-engage tail-follow on phase change so the new stage's transcript
        // streams into view.
        self.set_follow_tail(true);
        Ok(())
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

        self.state.builder.task_titles = parsed_tasks
            .tasks
            .iter()
            .map(|t| (t.id, t.title.clone()))
            .collect();
        self.state.builder.reset_task_pipeline(
            parsed_tasks
                .tasks
                .iter()
                .map(|task| (task.id, Some(task.title.clone()))),
        );

        self.transition_to_phase(Phase::ImplementationRound(1))?;
        self.state.save()?; // Persist state after transition and builder setup

        Ok(())
    }

    pub fn decline_skip_to_implementation(&mut self) -> Result<()> {
        use crate::artifacts::SkipToImplKind;
        let kind = self.state.skip_to_impl_kind;
        self.state.skip_to_impl_rationale = None;
        self.state.skip_to_impl_kind = None;
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
        let decision = self
            .state
            .pending_guard_decision
            .take()
            .ok_or_else(|| anyhow::anyhow!("accept_guard_reset: no pending guard decision"))?;

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
        let decision = self
            .state
            .pending_guard_decision
            .take()
            .ok_or_else(|| anyhow::anyhow!("accept_guard_keep: no pending guard decision"))?;

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
        self.state.current_phase = originating;
        self.complete_run_finalization(&run, None)
    }

    fn editable_artifact(&self) -> Option<std::path::PathBuf> {
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

    fn open_editable_artifact(&self) {
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

    fn can_go_back(&self) -> bool {
        !matches!(self.state.current_phase, Phase::IdeaInput | Phase::Done)
    }

    fn go_back(&mut self) {
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
                self.state.agent_error = None;
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
                self.state.agent_error = None;
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
                kill_window(&format!("[Coder r{r}]"));
                let _ = fs::remove_dir_all(session_dir.join("rounds").join(format!("{r:03}")));
                let prev = if r <= 1 {
                    if self.state.skip_to_impl_rationale.is_some() {
                        Phase::BrainstormRunning
                    } else {
                        self.state.builder = session_state::BuilderState::default();
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
                self.state.skip_to_impl_rationale = None;
                self.state.skip_to_impl_kind = None;
                self.state.agent_error = None;
                let _ = self.transition_to_phase(Phase::BrainstormRunning);
            }
            Phase::GitGuardPending => {
                // No window owned by this phase; the modal is purely TUI.
                // Operator handlers are the legitimate exit path; go_back is
                // a no-op while the decision is pending.
            }
            Phase::IdeaInput | Phase::BlockedNeedsUser | Phase::Done => {}
        }

        self.state.agent_error = None;
        self.window_launched = false;
        self.current_run_id = None;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        let _ = self.state.save();
    }

    fn attempt_for(&self, stage: &str, task_id: Option<u32>, round: u32) -> u32 {
        self.state
            .agent_runs
            .iter()
            .filter(|run| run.stage == stage && run.task_id == task_id && run.round == round)
            .map(|run| run.attempt)
            .max()
            .unwrap_or(0)
            + 1
    }

    fn completed_rounds(&self, stage: &str) -> u32 {
        self.state
            .agent_runs
            .iter()
            .filter(|run| run.stage == stage && run.status == RunStatus::Done)
            .map(|run| run.round)
            .max()
            .unwrap_or(0)
    }

    fn running_run(&self) -> Option<&crate::state::RunRecord> {
        self.current_run_id.and_then(|run_id| {
            self.state
                .agent_runs
                .iter()
                .find(|run| run.id == run_id && run.status == RunStatus::Running)
        })
    }

    fn try_test_launch(
        &mut self,
        status_path: &std::path::Path,
        artifact_path: Option<&std::path::Path>,
        run_key: &str,
        artifacts_dir: &std::path::Path,
    ) -> Option<Result<()>> {
        #[cfg(not(test))]
        {
            let _ = (status_path, artifact_path, run_key, artifacts_dir);
            None
        }
        #[cfg(test)]
        {
            let harness = self.test_launch_harness.as_ref()?.clone();
            let outcome = harness
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .outcomes
                .pop_front()
                .expect("expected queued test launch outcome");
            Some((|| -> Result<()> {
                if let Some(parent) = status_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(status_path, outcome.exit_code.to_string())?;
                if let (Some(path), Some(contents)) = (artifact_path, outcome.artifact_contents) {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(path, contents)?;
                }
                // Write a synthetic finish stamp so test-path behavior mirrors
                // the real runner-owned wrapper.
                let stamp_path = artifacts_dir
                    .join("run-finish")
                    .join(format!("{run_key}.toml"));
                let stamp = crate::runner::FinishStamp {
                    finished_at: chrono::Utc::now().to_rfc3339(),
                    exit_code: outcome.exit_code,
                    head_before: "test-base".to_string(),
                    head_after: "test-after".to_string(),
                    head_state: "stable".to_string(),
                };
                let _ = crate::runner::write_finish_stamp(&stamp_path, &stamp);
                Ok(())
            })())
        }
    }

    fn window_exists(&self, window_name: &str) -> bool {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return false;
        }
        tmux::window_exists(window_name)
    }

    fn retry_key_for_run(run: &crate::state::RunRecord) -> (String, Option<u32>, u32) {
        (run.stage.clone(), run.task_id, run.round)
    }

    /// Project a list of completed runs into the (vendors, (vendor,model)) shape
    /// expected by `select_for_review` and `select_excluding`. Runs with an
    /// unrecognised vendor string are dropped.
    fn used_review_pairs(
        runs: &[crate::state::RunRecord],
    ) -> (Vec<VendorKind>, Vec<(VendorKind, String)>) {
        let mut vendors = Vec::new();
        let mut models = Vec::new();
        for run in runs {
            let Some(vendor) = selection::vendor::str_to_vendor(&run.vendor) else {
                continue;
            };
            if !vendors.contains(&vendor) {
                vendors.push(vendor);
            }
            let pair = (vendor, run.model.clone());
            if !models.contains(&pair) {
                models.push(pair);
            }
        }
        (vendors, models)
    }

    fn phase_for_stage(stage: &str) -> SelectionPhase {
        match stage {
            "brainstorm" => SelectionPhase::Idea,
            "spec-review" => SelectionPhase::Review,
            "planning" => SelectionPhase::Planning,
            "plan-review" => SelectionPhase::Review,
            "sharding" => SelectionPhase::Planning,
            "recovery" => SelectionPhase::Planning,
            "coder" => SelectionPhase::Build,
            "reviewer" => SelectionPhase::Review,
            _ => SelectionPhase::Build,
        }
    }

    fn run_status_path_for(
        &self,
        stage: &str,
        task_id: Option<u32>,
        round: u32,
        attempt: u32,
    ) -> std::path::PathBuf {
        let task = task_id
            .map(|id| format!("task-{id}"))
            .unwrap_or_else(|| "stage".to_string());
        session_state::session_dir(&self.state.session_id)
            .join("artifacts")
            .join("run-status")
            .join(format!("{stage}-{task}-r{round}-a{attempt}.txt"))
    }

    fn run_status_path(&self, run: &crate::state::RunRecord) -> std::path::PathBuf {
        self.run_status_path_for(&run.stage, run.task_id, run.round, run.attempt)
    }

    fn run_key_for(stage: &str, task_id: Option<u32>, round: u32, attempt: u32) -> String {
        let task = task_id
            .map(|id| format!("task-{id}"))
            .unwrap_or_else(|| "stage".to_string());
        format!("{stage}-{task}-r{round}-a{attempt}")
    }

    fn live_summary_path_for_run(
        &self,
        stage: &str,
        task_id: Option<u32>,
        round: u32,
        attempt: u32,
    ) -> std::path::PathBuf {
        let run_key = Self::run_key_for(stage, task_id, round, attempt);
        session_state::session_dir(&self.state.session_id)
            .join("artifacts")
            .join(format!("live_summary.{run_key}.txt"))
    }

    fn live_summary_path_for(&self, run: &crate::state::RunRecord) -> std::path::PathBuf {
        self.live_summary_path_for_run(&run.stage, run.task_id, run.round, run.attempt)
    }

    fn finish_stamp_path_for_run(
        &self,
        stage: &str,
        task_id: Option<u32>,
        round: u32,
        attempt: u32,
    ) -> std::path::PathBuf {
        let run_key = Self::run_key_for(stage, task_id, round, attempt);
        session_state::session_dir(&self.state.session_id)
            .join("artifacts")
            .join("run-finish")
            .join(format!("{run_key}.toml"))
    }

    fn finish_stamp_path_for(&self, run: &crate::state::RunRecord) -> std::path::PathBuf {
        self.finish_stamp_path_for_run(&run.stage, run.task_id, run.round, run.attempt)
    }

    fn stamp_timeout_duration() -> Duration {
        std::env::var(ENV_STAMP_TIMEOUT_MS)
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_millis(DEFAULT_STAMP_TIMEOUT_MS))
    }

    fn guard_dir_for(
        &self,
        stage: &str,
        task_id: Option<u32>,
        round: u32,
        attempt: u32,
    ) -> std::path::PathBuf {
        let task = task_id
            .map(|id| format!("task-{id}"))
            .unwrap_or_else(|| "stage".to_string());
        session_state::session_dir(&self.state.session_id)
            .join(".guards")
            .join(format!("{stage}-{task}-r{round}-a{attempt}"))
    }

    /// Snapshot the run's immutability state. Non-coder agents must leave the
    /// git tree unchanged; the coder must not edit session control files.
    /// No-op under the test harness (no real git available).
    /// Returns `true` if the working tree was dirty at capture time (non-coder
    /// only; always `false` for coder). `mode` is ignored for the coder stage.
    fn capture_run_guard(
        &self,
        stage: &str,
        task_id: Option<u32>,
        round: u32,
        attempt: u32,
        mode: guard::GuardMode,
    ) -> bool {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return false;
        }
        let dir = self.guard_dir_for(stage, task_id, round, attempt);
        let session_dir = session_state::session_dir(&self.state.session_id);
        if stage == "coder" {
            let _ = guard::capture_coder(&dir, &session_dir, round);
            false
        } else {
            let dirty = guard::git_status_dirty();
            let _ = guard::capture_non_coder(
                &dir,
                &format!(
                    "{stage}-{}-r{round}-a{attempt}",
                    task_id
                        .map(|id| format!("task{id}"))
                        .unwrap_or_else(|| "stage".to_string())
                ),
                mode,
            );
            dirty
        }
    }

    fn enforce_run_guard(&self, run: &crate::state::RunRecord) -> guard::VerifyResult {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return guard::VerifyResult::Ok { warnings: vec![] };
        }
        let dir = self.guard_dir_for(&run.stage, run.task_id, run.round, run.attempt);
        guard::verify(&dir, &run.stage)
    }

    fn read_exit_status_code(&self, run: &crate::state::RunRecord) -> Option<i32> {
        std::fs::read_to_string(self.run_status_path(run))
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
    }

    fn artifact_present(path: &std::path::Path) -> bool {
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.len() > 0)
            .unwrap_or(false)
    }

    /// Capture HEAD at round start so the reviewer can inspect `base_sha..HEAD`.
    /// Idempotent on resume: the original base is preserved.
    fn capture_round_base(&self, round_dir: &std::path::Path) {
        let scope_file = round_dir.join("review_scope.toml");
        if scope_file.exists() {
            return;
        }
        if let Some(parent) = scope_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            let _ = std::fs::write(&scope_file, "base_sha = \"test-base\"\n");
            return;
        }
        if let Some(sha) = git_rev_parse_head() {
            let _ = std::fs::write(&scope_file, format!("base_sha = \"{sha}\"\n"));
        }
    }

    fn failed_unverified_reason(stamp_path: &std::path::Path, detail: impl AsRef<str>) -> String {
        format!(
            "failed_unverified: {} at {}",
            detail.as_ref(),
            stamp_path.display()
        )
    }

    fn coder_gate_reason(
        &self,
        run: &crate::state::RunRecord,
        round_dir: &std::path::Path,
    ) -> Option<String> {
        let scope_file = round_dir.join("review_scope.toml");
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return (!Self::artifact_present(&scope_file)).then(|| "base_missing".to_string());
        }
        if !Self::artifact_present(&scope_file) {
            return Some("base_missing".to_string());
        }
        let base = match read_review_scope_base_sha(&scope_file) {
            Ok(s) => s,
            Err(_) => return Some("base_missing".to_string()),
        };
        if base.is_empty() {
            return Some("base_missing".to_string());
        }
        let stamp_path = self.finish_stamp_path_for(run);
        if !Self::artifact_present(&stamp_path) {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                "missing finish stamp",
            ));
        }
        let stamp = match crate::runner::read_finish_stamp(&stamp_path) {
            Ok(stamp) => stamp,
            Err(_) => {
                return Some(Self::failed_unverified_reason(
                    &stamp_path,
                    "malformed finish stamp",
                ));
            }
        };
        if stamp.head_state != "stable" {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                format!("head_state={}", stamp.head_state),
            ));
        }
        if stamp.exit_code == 0 && stamp.head_after.trim().is_empty() {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                "empty stable head_after",
            ));
        }
        if stamp.head_after == base {
            return Some("no_commits_since_round_start".to_string());
        }
        None
    }

    fn normalized_failure_reason(
        &mut self,
        run: &crate::state::RunRecord,
    ) -> Result<Option<String>> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let (has_artifact_check, artifact_reason) = match run.stage.as_str() {
            "brainstorm" => {
                let spec_path = session_dir.join("artifacts").join("spec.md");
                (
                    true,
                    (!Self::artifact_present(&spec_path)).then(|| "artifact_missing".to_string()),
                )
            }
            "spec-review" => {
                let review_path = session_dir
                    .join("artifacts")
                    .join(format!("spec-review-{}.md", run.round));
                (
                    true,
                    (!Self::artifact_present(&review_path)).then(|| "artifact_missing".to_string()),
                )
            }
            "planning" => {
                let plan_path = session_dir.join("artifacts").join("plan.md");
                (
                    true,
                    (!Self::artifact_present(&plan_path)).then(|| "artifact_missing".to_string()),
                )
            }
            "plan-review" => {
                let review_path = session_dir
                    .join("artifacts")
                    .join(format!("plan-review-{}.md", run.round));
                (
                    true,
                    (!Self::artifact_present(&review_path)).then(|| "artifact_missing".to_string()),
                )
            }
            "sharding" => {
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                let reason = if !Self::artifact_present(&tasks_path) {
                    Some("artifact_missing".to_string())
                } else {
                    tasks::validate(&tasks_path)
                        .err()
                        .map(|err| format!("artifact_invalid: {err}"))
                };
                (true, reason)
            }
            "recovery" => {
                let spec_path = session_dir.join("artifacts").join("spec.md");
                let plan_path = session_dir.join("artifacts").join("plan.md");
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                let recovery_path = session_dir
                    .join("rounds")
                    .join(format!("{:03}", run.round))
                    .join("recovery.toml");
                let reason = if !Self::artifact_present(&spec_path)
                    || !Self::artifact_present(&plan_path)
                    || !Self::artifact_present(&tasks_path)
                    || !Self::artifact_present(&recovery_path)
                {
                    Some("artifact_missing".to_string())
                } else if let Err(err) =
                    validate_stage_toml_writes(&session_dir, "recovery", run.round)
                {
                    Some(format!("artifact_invalid: {err}"))
                } else {
                    tasks::validate(&tasks_path)
                        .err()
                        .map(|err| format!("artifact_invalid: {err}"))
                };
                (true, reason)
            }
            "coder" => {
                // Coder's real deliverable is a git commit, not a file. We
                let round_dir = session_dir.join("rounds").join(format!("{:03}", run.round));
                (false, self.coder_gate_reason(run, &round_dir))
            }
            "reviewer" => {
                let review_path = session_dir
                    .join("rounds")
                    .join(format!("{:03}", run.round))
                    .join("review.toml");
                let reason = if !Self::artifact_present(&review_path) {
                    Some("artifact_missing".to_string())
                } else {
                    review::validate(&review_path)
                        .err()
                        .map(|err| format!("artifact_invalid: {err}"))
                };
                (true, reason)
            }
            _ => (false, None),
        };

        // If the stage produced a valid artifact, treat the run as successful
        // regardless of the wrapped pipeline's exit code. Agent commands like
        // `codex exec --json | jq ...` can return non-zero (e.g., a stray
        // non-JSON line from the agent makes jq exit 4/5) even after the
        // agent has already written a well-formed artifact. Warnings are
        // emitted for dirty-tree changes; a hard guard error (HEAD advance)
        // still fails the run.
        if has_artifact_check && artifact_reason.is_none() {
            if let Some(code) = self.read_exit_status_code(run)
                && code != 0
            {
                let _ = self.state.log_event(format!(
                    "run {} ({}) exited {code} but produced a valid artifact; treating as success",
                    run.id, run.stage
                ));
            }
            match self.enforce_run_guard(run) {
                guard::VerifyResult::Ok { warnings } => {
                    for w in warnings {
                        self.append_system_message(run.id, MessageKind::SummaryWarn, w);
                    }
                    return Ok(None);
                }
                guard::VerifyResult::HardError { reason, warnings } => {
                    for w in warnings {
                        self.append_system_message(run.id, MessageKind::SummaryWarn, w);
                    }
                    return Ok(Some(reason));
                }
                guard::VerifyResult::PendingDecision {
                    captured_head,
                    current_head,
                    warnings,
                } => {
                    // Park the run: populate pending decision and return Ok(None).
                    // Warnings are NOT appended yet — they replay at resolution time.
                    // The finalization caller detects the populated field and
                    // transitions to GitGuardPending instead of completing normally.
                    self.state.pending_guard_decision = Some(PendingGuardDecision {
                        stage: run.stage.clone(),
                        task_id: run.task_id,
                        round: run.round,
                        attempt: run.attempt,
                        run_id: run.id,
                        captured_head,
                        current_head,
                        warnings,
                    });
                    return Ok(None);
                }
            }
        }

        // No artifact (unknown stage) or artifact missing/invalid: exit code
        // takes precedence so the operator sees the real failure first.
        if let Some(code) = self.read_exit_status_code(run)
            && code != 0
        {
            if code > 128 {
                return Ok(Some(format!("killed({})", code - 128)));
            }
            return Ok(Some(format!("exit({code})")));
        }

        // Guard reason beats artifact reason (coder control-file edits are a
        // real protocol violation; non-coder HEAD advances are hard errors).
        // PendingDecision here means artifact was missing/invalid — the run is
        // already a failure from the artifact check, so treat it as no guard error.
        let (guard_reason, guard_warnings) = match self.enforce_run_guard(run) {
            guard::VerifyResult::Ok { warnings } => (None, warnings),
            guard::VerifyResult::HardError { reason, warnings } => (Some(reason), warnings),
            guard::VerifyResult::PendingDecision { warnings, .. } => (None, warnings),
        };
        for w in guard_warnings {
            self.append_system_message(run.id, MessageKind::SummaryWarn, w);
        }
        Ok(guard_reason.or(artifact_reason))
    }

    fn append_system_message(&mut self, run_id: u64, kind: MessageKind, text: String) {
        let message = Message {
            ts: chrono::Utc::now(),
            run_id,
            kind,
            sender: MessageSender::System,
            text,
        };
        if let Err(err) = self.state.append_message(&message) {
            let _ = self.state.log_event(format!(
                "failed to append system message for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(message);
        }
    }

    fn emit_dirty_tree_warning(&mut self) {
        if let Some(run_id) = self.current_run_id {
            self.append_system_message(
                run_id,
                MessageKind::SummaryWarn,
                "working tree is dirty \u{2014} agent will run against uncommitted changes"
                    .to_string(),
            );
        }
    }

    fn start_run_tracking(
        &mut self,
        stage: &str,
        task_id: Option<u32>,
        round: u32,
        model: String,
        vendor: String,
        window_name: String,
    ) {
        let attempt = self.attempt_for(stage, task_id, round);
        let run_id = self.state.create_run_record(
            stage.to_string(),
            task_id,
            round,
            attempt,
            model,
            vendor,
            window_name,
        );
        let Some(run) = self.state.agent_runs.iter().find(|run| run.id == run_id) else {
            return;
        };
        let started = Message {
            ts: chrono::Utc::now(),
            run_id,
            kind: MessageKind::Started,
            sender: MessageSender::System,
            text: format!("agent started · {} ({})", run.model, run.vendor),
        };
        if let Err(err) = self.state.append_message(&started) {
            let _ = self.state.log_event(format!(
                "failed to append started message for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(started);
        }
        self.current_run_id = Some(run_id);
        self.window_launched = true;
        self.live_summary_path =
            Some(self.live_summary_path_for_run(stage, task_id, round, attempt));
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        let _ = self.setup_watcher();
        if let Err(err) = self.state.save() {
            let _ = self
                .state
                .log_event(format!("failed to save session after launch: {err}"));
        }
        self.read_live_summary_pipeline();
        self.messages = SessionState::load_messages(&self.state.session_id).unwrap_or_default();
        self.rebuild_tree_view(None);
    }

    fn update_agent_progress(&mut self) {
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
            self.spinner_tick = self.spinner_tick.wrapping_add(1);
            return;
        }
        // Keep spinning while the stall is under 30s; freeze after that.
        if let Some(last) = self.agent_last_change
            && now.duration_since(last) < Duration::from_secs(30)
        {
            self.spinner_tick = self.spinner_tick.wrapping_add(1);
        }
    }

    /// Auto-launch the agent for the current phase if it's a non-interactive
    /// one (spec review, sharding, coder, reviewer). Idempotent: no-op if the
    /// window is already up, if models aren't loaded, or if the last run
    /// errored (user needs to intervene).
    fn maybe_auto_launch(&mut self) {
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

    fn poll_agent_window(&mut self) {
        let Some(run_id) = self.current_run_id else {
            self.pending_drain_deadline = None;
            self.pending_drain_notice_emitted = false;
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
            self.pending_drain_notice_emitted = false;
            return;
        };
        if self.window_exists(&run.window_name) {
            self.pending_drain_deadline = None;
            self.pending_drain_notice_emitted = false;
            return;
        }

        let deadline = *self
            .pending_drain_deadline
            .get_or_insert_with(|| Instant::now() + Self::stamp_timeout_duration());
        if !self.pending_drain_notice_emitted {
            self.append_system_message(
                run.id,
                MessageKind::Summary,
                "window closed; draining live summary and finish stamp before finalize".to_string(),
            );
            self.pending_drain_notice_emitted = true;
        }
        let now = Instant::now();
        let live_summary_missing = !self.live_summary_path_for(&run).exists();
        let stamp_path = self.finish_stamp_path_for(&run);
        let stamp_present = Self::artifact_present(&stamp_path);
        let deadline_elapsed = now >= deadline;
        if !live_summary_missing || (!stamp_present && !deadline_elapsed) {
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
        self.pending_drain_notice_emitted = false;
        self.window_launched = false;
        self.current_run_id = None;
        let outcome = self.finalize_current_run(&run);
        if let Err(err) = outcome {
            self.state.agent_error = Some(err.to_string());
            let _ = self.state.log_event(format!(
                "run finalization failed for {}: {err}",
                run.window_name
            ));
        }
        self.rebuild_tree_view(None);
    }

    fn ensure_builder_task_for_round(&mut self, round: u32) -> Option<u32> {
        let task_id = self.state.builder.ensure_task_for_round(round)?;
        let round_dir = session_state::session_dir(&self.state.session_id)
            .join("rounds")
            .join(format!("{round:03}"));
        let _ = std::fs::create_dir_all(&round_dir);
        Some(task_id)
    }

    /// Enter builder recovery.  Preserves `builder.done`/`builder.pending` and
    /// records recovery context.  `trigger` must be `"human_blocked"` or
    /// `"agent_pivot"`; `"human_blocked"` produces an interactive recovery stage.
    ///
    /// Circuit breaker: if `recovery_cycle_count` reaches 3 the trigger is
    /// automatically escalated to `"human_blocked"` and a pipeline message is
    /// emitted identifying the loop.
    ///
    /// Returns true so callers can treat this like other auto-retry paths.
    fn enter_builder_recovery(
        &mut self,
        triggering_round: u32,
        trigger_task_id: Option<u32>,
        trigger_summary: Option<String>,
        trigger: &str,
    ) -> bool {
        if self.current_run_id.is_some() || self.window_launched {
            let _ = self.state.log_event(
                "enter_builder_recovery called while a run window is still marked active"
                    .to_string(),
            );
        }

        let session_dir = session_state::session_dir(&self.state.session_id);
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");
        let (prev_task_ids, prev_max) = tasks::validate(&tasks_path)
            .ok()
            .map(|f| {
                let ids = f.tasks.iter().map(|t| t.id).collect::<Vec<_>>();
                let max = ids.iter().copied().max();
                (ids, max)
            })
            .unwrap_or_default();

        // Circuit breaker: after 3 consecutive recovery cycles without an approved
        // plan review, force human_blocked so a human can break the loop.
        self.state.builder.recovery_cycle_count += 1;
        let effective_trigger = if self.state.builder.recovery_cycle_count >= 3
            && trigger != "human_blocked"
        {
            let loop_msg = format!(
                "recovery loop: {} consecutive recovery cycles without approval — escalating to human_blocked",
                self.state.builder.recovery_cycle_count
            );
            let _ = self.state.log_event(loop_msg.clone());
            let msg = Message {
                ts: chrono::Utc::now(),
                run_id: self.current_run_id.unwrap_or(0),
                kind: MessageKind::SummaryWarn,
                sender: MessageSender::System,
                text: loop_msg,
            };
            if let Err(err) = self.state.append_message(&msg) {
                let _ = self.state.log_event(format!(
                    "failed to append circuit-breaker escalation message: {err}"
                ));
            } else {
                self.messages.push(msg);
            }
            "human_blocked"
        } else {
            trigger
        };

        self.state.builder.recovery_trigger_task_id =
            trigger_task_id.or(self.state.builder.current_task_id());
        self.state.builder.recovery_prev_max_task_id = prev_max;
        self.state.builder.recovery_prev_task_ids = prev_task_ids;
        self.state.builder.recovery_trigger_summary = trigger_summary;
        if let Some(current_task_id) = self.state.builder.current_task_id() {
            let status = if self.state.builder.pipeline_items.is_empty() {
                PipelineItemStatus::Pending
            } else {
                PipelineItemStatus::Failed
            };
            let _ =
                self.state
                    .builder
                    .set_task_status(current_task_id, status, Some(triggering_round));
        }
        let interactive = effective_trigger == "human_blocked";
        let title = if interactive {
            "Human-blocked recovery"
        } else {
            "Agent pivot recovery"
        };
        self.state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "recovery".to_string(),
            task_id: None,
            round: Some(triggering_round),
            status: PipelineItemStatus::Running,
            title: Some(title.to_string()),
            mode: None,
            trigger: Some(effective_trigger.to_string()),
            interactive: Some(interactive),
        });
        self.state.agent_error = None;

        if let Err(err) = self.transition_to_phase(Phase::BuilderRecovery(triggering_round)) {
            self.state.agent_error = Some(format!("failed to enter builder recovery: {err}"));
            let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
        }
        true
    }

    fn started_builder_task_ids(&self) -> BTreeSet<u32> {
        self.state
            .agent_runs
            .iter()
            .filter(|run| matches!(run.stage.as_str(), "coder" | "reviewer"))
            .filter_map(|run| run.task_id)
            .collect()
    }

    fn recovery_notes_document_started_supersession(
        text: &str,
        superseded_ids: &BTreeSet<u32>,
    ) -> Result<()> {
        if !text.contains("Recovery Notes") {
            anyhow::bail!("missing required `Recovery Notes` section");
        }
        for id in superseded_ids {
            let needle = id.to_string();
            let mut found = false;
            for (idx, _) in text.match_indices(&needle) {
                // REVIEWER: spec requires superseded ids be explicitly named but does not
                // prescribe formatting; treat any standalone numeric token match as explicit.
                let prev = idx
                    .checked_sub(1)
                    .and_then(|p| text.as_bytes().get(p).copied())
                    .map(char::from);
                let next = text
                    .as_bytes()
                    .get(idx + needle.len())
                    .copied()
                    .map(char::from);
                let prev_digit = prev.is_some_and(|ch| ch.is_ascii_digit());
                let next_digit = next.is_some_and(|ch| ch.is_ascii_digit());
                if !prev_digit && !next_digit {
                    found = true;
                    break;
                }
            }
            if !found {
                anyhow::bail!("`Recovery Notes` missing superseded started task id {id}");
            }
        }
        Ok(())
    }

    fn reconcile_builder_recovery(&mut self, recovery_run_id: u64) -> Result<()> {
        use anyhow::Context;

        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let parsed = tasks::validate(&tasks_path)
            .with_context(|| format!("invalid {}", tasks_path.display()))?;

        let done_ids = self
            .state
            .builder
            .done
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let started_ids = self.started_builder_task_ids();
        let prev_task_ids = self
            .state
            .builder
            .recovery_prev_task_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let historical_max = self
            .state
            .builder
            .recovery_prev_max_task_id
            .into_iter()
            .chain(done_ids.iter().copied())
            .chain(started_ids.iter().copied())
            .max()
            .unwrap_or(0);

        let recovered_ids = parsed.tasks.iter().map(|t| t.id).collect::<Vec<_>>();
        let recovered_set = recovered_ids.iter().copied().collect::<BTreeSet<_>>();

        if let Some(collision) = recovered_ids.iter().find(|id| done_ids.contains(id)) {
            anyhow::bail!("recovered unfinished tasks include completed task id {collision}");
        }

        let historical_ids = prev_task_ids
            .iter()
            .copied()
            .chain(done_ids.iter().copied())
            .chain(started_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        for id in &recovered_ids {
            if !historical_ids.contains(id) && *id <= historical_max {
                anyhow::bail!(
                    "new recovery task id {id} must be greater than prior max id {historical_max}"
                );
            }
        }

        let superseded_started = started_ids
            .difference(&done_ids)
            .copied()
            .collect::<BTreeSet<_>>()
            .difference(&recovered_set)
            .copied()
            .collect::<BTreeSet<_>>();
        if !superseded_started.is_empty() {
            let spec_text = std::fs::read_to_string(&spec_path)
                .with_context(|| format!("cannot read {}", spec_path.display()))?;
            Self::recovery_notes_document_started_supersession(&spec_text, &superseded_started)
                .with_context(|| format!("invalid {}", spec_path.display()))?;

            let plan_text = std::fs::read_to_string(&plan_path)
                .with_context(|| format!("cannot read {}", plan_path.display()))?;
            Self::recovery_notes_document_started_supersession(&plan_text, &superseded_started)
                .with_context(|| format!("invalid {}", plan_path.display()))?;
        }

        let completed_ids = self.state.builder.done_task_ids();
        let completed_set = completed_ids.iter().copied().collect::<BTreeSet<_>>();
        let mut next_items = self
            .state
            .builder
            .pipeline_items
            .iter()
            .filter(|item| {
                item.stage == "coder"
                    && item
                        .task_id
                        .is_some_and(|task_id| completed_set.contains(&task_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        if next_items.is_empty() {
            for task_id in &completed_ids {
                next_items.push(PipelineItem {
                    id: 0,
                    stage: "coder".to_string(),
                    task_id: Some(*task_id),
                    round: None,
                    status: PipelineItemStatus::Approved,
                    title: self.state.builder.task_titles.get(task_id).cloned(),
                    mode: None,
                    trigger: None,
                    interactive: None,
                });
            }
        }
        for task in &parsed.tasks {
            self.state
                .builder
                .task_titles
                .insert(task.id, task.title.clone());
            if !completed_set.contains(&task.id) {
                next_items.push(PipelineItem {
                    id: 0,
                    stage: "coder".to_string(),
                    task_id: Some(task.id),
                    round: None,
                    status: PipelineItemStatus::Pending,
                    title: Some(task.title.clone()),
                    mode: None,
                    trigger: None,
                    interactive: None,
                });
            }
        }
        self.state.builder.pipeline_items = next_items;
        self.state.builder.sync_legacy_queue_views();
        if let Some(item) = self
            .state
            .builder
            .pipeline_items
            .iter_mut()
            .rev()
            .find(|item| item.stage == "recovery" && item.status == PipelineItemStatus::Running)
        {
            item.status = PipelineItemStatus::Done;
        }
        self.state.builder.retry_reset_run_id_cutoff = Some(recovery_run_id);
        self.state.builder.recovery_trigger_task_id = None;
        self.state.builder.recovery_prev_max_task_id = None;
        self.state.builder.recovery_prev_task_ids.clear();
        self.state.builder.recovery_trigger_summary = None;
        Ok(())
    }

    /// Called when a recovery-mode plan review agent run completes.
    ///
    /// Reads `artifacts/plan_review.toml`, applies the verdict, and either
    /// advances to recovery sharding (approved) or re-runs recovery
    /// (revise/human_blocked/agent_pivot with circuit-breaker).
    fn handle_recovery_plan_review_completed(
        &mut self,
        run: &crate::state::RunRecord,
        round: u32,
    ) -> Result<()> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let plan_review_path = session_dir.join("artifacts").join("plan_review.toml");

        // Mark the recovery plan-review pipeline item as done/completed.
        if let Some(item) = self
            .state
            .builder
            .pipeline_items
            .iter_mut()
            .rev()
            .find(|i| i.stage == "plan-review" && i.status == PipelineItemStatus::Running)
        {
            item.status = PipelineItemStatus::Done;
        }

        match review::validate(&plan_review_path) {
            Ok(verdict) => {
                let summary_text = verdict.summary.trim().to_string();
                if !summary_text.is_empty() {
                    let kind = match verdict.status {
                        review::ReviewStatus::Approved => MessageKind::Summary,
                        _ => MessageKind::SummaryWarn,
                    };
                    let msg = Message {
                        ts: chrono::Utc::now(),
                        run_id: run.id,
                        kind,
                        sender: MessageSender::Agent {
                            model: run.model.clone(),
                            vendor: run.vendor.clone(),
                        },
                        text: summary_text,
                    };
                    if let Err(err) = self.state.append_message(&msg) {
                        let _ = self.state.log_event(format!(
                            "failed to append recovery plan review message: {err}"
                        ));
                    } else {
                        self.messages.push(msg);
                    }
                }
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                match verdict.status {
                    review::ReviewStatus::Approved => {
                        // Reset circuit-breaker: recovery reached an approved plan review.
                        self.state.builder.recovery_cycle_count = 0;
                        // Insert recovery sharding pipeline item.
                        self.state.builder.push_pipeline_item(PipelineItem {
                            id: 0,
                            stage: "sharding".to_string(),
                            task_id: None,
                            round: Some(round),
                            status: PipelineItemStatus::Pending,
                            title: Some("Recovery sharding".to_string()),
                            mode: Some("recovery".to_string()),
                            trigger: None,
                            interactive: Some(false),
                        });
                        self.transition_to_phase(Phase::BuilderRecoverySharding(round))?;
                    }
                    review::ReviewStatus::Revise
                    | review::ReviewStatus::HumanBlocked
                    | review::ReviewStatus::AgentPivot => {
                        let trigger_str = match verdict.status {
                            review::ReviewStatus::HumanBlocked => "human_blocked",
                            review::ReviewStatus::AgentPivot => "agent_pivot",
                            _ => "agent_pivot",
                        };
                        let summary = verdict.feedback.join("\n");
                        let trigger_summary = (!summary.trim().is_empty()).then_some(summary);
                        self.enter_builder_recovery(round, None, trigger_summary, trigger_str);
                    }
                }
            }
            Err(err) => {
                let reason = format!("recovery_plan_review_failed: {err:#}");
                self.finalize_run_record(run.id, false, Some(reason.clone()));
                let failed_run = self
                    .state
                    .agent_runs
                    .iter()
                    .find(|r| r.id == run.id)
                    .cloned()
                    .unwrap_or_else(|| run.clone());
                if !self.maybe_auto_retry(&failed_run) {
                    self.state.agent_error = Some(reason);
                }
            }
        }
        Ok(())
    }

    /// Called when a recovery-mode sharding agent run completes.
    ///
    /// Validates the regenerated `tasks.toml` against the completed task history,
    /// rebuilds the pipeline queue, and advances to the next implementation round.
    fn handle_recovery_sharding_completed(
        &mut self,
        run: &crate::state::RunRecord,
        round: u32,
    ) -> Result<()> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");

        // Mark the recovery sharding pipeline item as done.
        if let Some(item) = self
            .state
            .builder
            .pipeline_items
            .iter_mut()
            .rev()
            .find(|i| i.stage == "sharding" && i.status == PipelineItemStatus::Running)
        {
            item.status = PipelineItemStatus::Done;
        }

        match tasks::validate(&tasks_path) {
            Ok(parsed) => {
                let done_ids = self
                    .state
                    .builder
                    .done_task_ids()
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>();

                // Validate no collisions with completed task IDs.
                for task in &parsed.tasks {
                    if done_ids.contains(&task.id) {
                        let reason = format!(
                            "recovery sharding produced task id {} that collides with a completed task",
                            task.id
                        );
                        self.finalize_run_record(run.id, false, Some(reason.clone()));
                        self.state.agent_error = Some(reason);
                        let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
                        return Ok(());
                    }
                }

                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;

                // Rebuild pipeline: completed tasks stay as-is, add pending from recovered tasks.
                let mut next_items: Vec<PipelineItem> = self
                    .state
                    .builder
                    .pipeline_items
                    .iter()
                    .filter(|item| {
                        item.stage == "coder"
                            && item.task_id.is_some_and(|id| done_ids.contains(&id))
                    })
                    .cloned()
                    .collect();

                if next_items.is_empty() {
                    for &tid in &done_ids {
                        next_items.push(PipelineItem {
                            id: 0,
                            stage: "coder".to_string(),
                            task_id: Some(tid),
                            round: None,
                            status: PipelineItemStatus::Approved,
                            title: self.state.builder.task_titles.get(&tid).cloned(),
                            mode: None,
                            trigger: None,
                            interactive: None,
                        });
                    }
                }

                for task in &parsed.tasks {
                    self.state
                        .builder
                        .task_titles
                        .insert(task.id, task.title.clone());
                    if !done_ids.contains(&task.id) {
                        next_items.push(PipelineItem {
                            id: 0,
                            stage: "coder".to_string(),
                            task_id: Some(task.id),
                            round: None,
                            status: PipelineItemStatus::Pending,
                            title: Some(task.title.clone()),
                            mode: None,
                            trigger: None,
                            interactive: None,
                        });
                    }
                }
                self.state.builder.pipeline_items = next_items;
                self.state.builder.sync_legacy_queue_views();

                let pipeline_msg = format!(
                    "recovery sharding complete: {} pending tasks",
                    self.state.builder.pending_task_ids().len()
                );
                self.append_system_message(run.id, MessageKind::Summary, pipeline_msg);

                self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
            }
            Err(err) => {
                let reason = format!("recovery_sharding_failed: {err:#}");
                self.finalize_run_record(run.id, false, Some(reason.clone()));
                let failed_run = self
                    .state
                    .agent_runs
                    .iter()
                    .find(|r| r.id == run.id)
                    .cloned()
                    .unwrap_or_else(|| run.clone());
                if !self.maybe_auto_retry(&failed_run) {
                    self.state.agent_error = Some(reason);
                }
            }
        }
        Ok(())
    }

    /// Launch the non-interactive recovery-mode plan review agent.
    fn launch_recovery_plan_review(&mut self) {
        let _ = self.launch_recovery_plan_review_with_model(None);
    }

    fn launch_recovery_plan_review_with_model(
        &mut self,
        override_model: Option<CachedModel>,
    ) -> bool {
        use anyhow::Context;

        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }
        let Phase::BuilderRecoveryPlanReview(round) = self.state.current_phase else {
            return false;
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let plan_review_path = artifacts.join("plan_review.toml");
        let _ = std::fs::remove_file(&plan_review_path);

        let recovery_path = session_dir
            .join("rounds")
            .join(format!("{round:03}"))
            .join("recovery.toml");
        let triggering_review_path = session_dir
            .join("rounds")
            .join(format!("{round:03}"))
            .join("review.toml");
        let attempt = self.attempt_for("plan-review", None, round);
        let live_summary_path = self.live_summary_path_for_run("plan-review", None, round, attempt);
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("recovery-plan-review-r{round}.md"));

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| pick_for_phase(&self.models, SelectionPhase::Review, None, &self.versions))
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let prompt = recovery_plan_review_prompt(
            &spec_path,
            &plan_path,
            &triggering_review_path,
            &recovery_path,
            &live_summary_path,
            &plan_review_path,
        );
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt)
            .with_context(|| format!("cannot write {}", prompt_path.display()))
        {
            self.state.agent_error = Some(err.to_string());
            return false;
        }

        // Update plan-review pipeline item to Running.
        if let Some(item) = self
            .state
            .builder
            .pipeline_items
            .iter_mut()
            .rev()
            .find(|i| i.stage == "plan-review" && i.status == PipelineItemStatus::Pending)
        {
            item.status = PipelineItemStatus::Running;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
        };
        let status_path = self.run_status_path_for("plan-review", None, round, attempt);
        let dirty = self.capture_run_guard(
            "plan-review",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let window_name = window_name_with_model("[Recovery Plan Review]", &model);
        let run_key = Self::run_key_for("plan-review", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) = self.try_test_launch(
            &status_path,
            Some(&plan_review_path),
            &run_key,
            &artifacts_dir,
        ) {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("plan-review", None, round, model, vendor, window_name);
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(err) => {
                self.state.agent_error =
                    Some(format!("failed to launch recovery plan review: {err}"));
                false
            }
        }
    }

    /// Launch the non-interactive recovery-mode sharding agent.
    fn launch_recovery_sharding(&mut self) {
        let _ = self.launch_recovery_sharding_with_model(None);
    }

    fn launch_recovery_sharding_with_model(&mut self, override_model: Option<CachedModel>) -> bool {
        use anyhow::Context;

        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }
        let Phase::BuilderRecoverySharding(round) = self.state.current_phase else {
            return false;
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let _ = std::fs::remove_file(&tasks_path);
        let attempt = self.attempt_for("sharding", None, round);
        let live_summary_path = self.live_summary_path_for_run("sharding", None, round, attempt);
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("recovery-sharding-r{round}.md"));

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| {
                pick_for_phase(&self.models, SelectionPhase::Planning, None, &self.versions)
            })
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let completed = self.state.builder.done_task_ids();
        let prompt = recovery_sharding_prompt(
            &spec_path,
            &plan_path,
            &live_summary_path,
            &tasks_path,
            &completed,
        );
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt)
            .with_context(|| format!("cannot write {}", prompt_path.display()))
        {
            self.state.agent_error = Some(err.to_string());
            return false;
        }

        // Update sharding pipeline item to Running.
        if let Some(item) = self
            .state
            .builder
            .pipeline_items
            .iter_mut()
            .rev()
            .find(|i| i.stage == "sharding" && i.status == PipelineItemStatus::Pending)
        {
            item.status = PipelineItemStatus::Running;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
        };
        let status_path = self.run_status_path_for("sharding", None, round, attempt);
        let dirty = self.capture_run_guard(
            "sharding",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let window_name = window_name_with_model("[Recovery Sharding]", &model);
        let run_key = Self::run_key_for("sharding", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&tasks_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("sharding", None, round, model, vendor, window_name);
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(err) => {
                self.state.agent_error = Some(format!("failed to launch recovery sharding: {err}"));
                false
            }
        }
    }

    fn finalize_run_record(&mut self, run_id: u64, success: bool, error: Option<String>) {
        let Some(run) = self
            .state
            .agent_runs
            .iter_mut()
            .find(|run| run.id == run_id)
        else {
            return;
        };
        let ended_at = chrono::Utc::now();
        run.ended_at = Some(ended_at);
        let unverified = error
            .as_deref()
            .is_some_and(|reason| reason.starts_with("failed_unverified:"));
        run.status = if success {
            RunStatus::Done
        } else if unverified {
            RunStatus::FailedUnverified
        } else {
            RunStatus::Failed
        };
        run.error = error.clone();

        let duration = ended_at.signed_duration_since(run.started_at);
        let total_seconds = duration.num_seconds().max(0);
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        let text = if success {
            format!(
                "done in {minutes}m{seconds:02}s · {} ({})",
                run.model, run.vendor
            )
        } else if unverified {
            format!(
                "attempt {} unverified: {}",
                run.attempt,
                error.unwrap_or_else(|| "unknown error".to_string())
            )
        } else {
            format!(
                "attempt {} failed: {}",
                run.attempt,
                error.unwrap_or_else(|| "unknown error".to_string())
            )
        };
        let message = Message {
            ts: ended_at,
            run_id,
            kind: MessageKind::End,
            sender: MessageSender::System,
            text,
        };
        if let Err(err) = self.state.append_message(&message) {
            let _ = self.state.log_event(format!(
                "failed to append end message for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(message);
        }
        if let Err(err) = self.state.save() {
            let _ = self.state.log_event(format!(
                "failed to save session after finalizing run {run_id}: {err}"
            ));
        }
    }

    fn retry_exhausted_summary(&self, failed_run: &crate::state::RunRecord) -> String {
        let mut attempts = self
            .state
            .agent_runs
            .iter()
            .filter(|run| {
                run.stage == failed_run.stage
                    && run.task_id == failed_run.task_id
                    && run.round == failed_run.round
                    && matches!(run.status, RunStatus::Failed | RunStatus::FailedUnverified)
            })
            .cloned()
            .collect::<Vec<_>>();
        attempts.sort_by_key(|run| run.attempt);

        let mut lines = vec![format!("retry exhausted ({} attempts)", attempts.len())];
        for run in attempts {
            lines.push(format!(
                "  attempt {}: {}/{} — {}",
                run.attempt,
                run.vendor,
                run.model,
                run.error.unwrap_or_else(|| "unknown error".to_string())
            ));
        }
        lines.join("\n")
    }

    fn maybe_auto_retry(&mut self, failed_run: &crate::state::RunRecord) -> bool {
        if failed_run.status == RunStatus::FailedUnverified {
            let _ = self.state.log_event(format!(
                "auto-retry suppressed for {} round {} attempt {} due to failed_unverified",
                failed_run.stage, failed_run.round, failed_run.attempt
            ));
            return false;
        }

        if failed_run.error.as_deref() == Some("user_forced_retry") {
            return false;
        }

        let key = Self::retry_key_for_run(failed_run);
        let last_failed_vendor = selection::vendor::str_to_vendor(&failed_run.vendor);
        if let Some(vendor) = last_failed_vendor {
            self.failed_models
                .entry(key.clone())
                .or_default()
                .insert((vendor, failed_run.model.clone()));
        }

        let max_attempts = self.models.len() as u32 + 2;
        if failed_run.attempt >= max_attempts {
            let summary = self.retry_exhausted_summary(failed_run);
            if matches!(failed_run.stage.as_str(), "coder" | "reviewer") {
                return self.enter_builder_recovery(
                    failed_run.round,
                    failed_run.task_id,
                    Some(summary),
                    "agent_pivot",
                );
            }
            if failed_run.stage == "recovery" {
                let summary = format!("builder recovery retry exhausted\n{summary}");
                self.state.agent_error = Some(summary.clone());
                let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
                self.append_system_message(failed_run.id, MessageKind::End, summary);
                return true;
            }

            self.state.agent_error = Some(summary.clone());
            let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
            self.append_system_message(failed_run.id, MessageKind::End, summary);
            let _ = self.state.log_event(format!(
                "auto-retry safety cap hit for {} round {} attempt {}",
                failed_run.stage, failed_run.round, failed_run.attempt
            ));
            return true;
        }

        let excluded: Vec<(VendorKind, String)> = self
            .failed_models
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let next_model = select_excluding(
            &self.models,
            Self::phase_for_stage(&failed_run.stage),
            &excluded,
            last_failed_vendor,
            &self.versions,
        );

        if let Some(next_model) = next_model.cloned() {
            self.append_system_message(
                failed_run.id,
                MessageKind::Started,
                format!(
                    "retrying with {}/{}",
                    vendor_tag(next_model.vendor),
                    next_model.name
                ),
            );
            return self.launch_retry_for_stage(failed_run, next_model);
        }

        let summary = self.retry_exhausted_summary(failed_run);
        if matches!(failed_run.stage.as_str(), "coder" | "reviewer") {
            return self.enter_builder_recovery(
                failed_run.round,
                failed_run.task_id,
                Some(summary),
                "agent_pivot",
            );
        }
        if failed_run.stage == "recovery" {
            let summary = format!("builder recovery retry exhausted\n{summary}");
            self.state.agent_error = Some(summary.clone());
            let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
            self.append_system_message(failed_run.id, MessageKind::End, summary);
            return true;
        }

        self.state.agent_error = Some(summary.clone());
        let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
        self.append_system_message(failed_run.id, MessageKind::End, summary);
        true
    }

    fn finalize_current_run(&mut self, run: &crate::state::RunRecord) -> Result<()> {
        self.drain_live_summary(run);

        let failure_reason = self.normalized_failure_reason(run)?;
        if failure_reason.is_none()
            && self
                .state
                .pending_guard_decision
                .as_ref()
                .is_some_and(|d| d.run_id == run.id)
        {
            self.transition_to_phase(Phase::GitGuardPending)?;
            let _ = self.state.save();
            return Ok(());
        }
        self.complete_run_finalization(run, failure_reason)
    }

    fn complete_run_finalization(
        &mut self,
        run: &crate::state::RunRecord,
        failure_reason: Option<String>,
    ) -> Result<()> {
        use anyhow::Context;

        let session_dir = session_state::session_dir(&self.state.session_id);
        if let Some(error) = failure_reason {
            self.finalize_run_record(run.id, false, Some(error.clone()));
            let failed_run = self
                .state
                .agent_runs
                .iter()
                .find(|candidate| candidate.id == run.id)
                .cloned()
                .unwrap_or_else(|| run.clone());
            if !self.maybe_auto_retry(&failed_run) {
                self.state.agent_error = Some(error);
            }
            return Ok(());
        }
        match self.state.current_phase {
            Phase::BrainstormRunning => {
                let skip_artifact_path = session_dir
                    .join("artifacts")
                    .join(ArtifactKind::SkipToImpl.filename());
                let proposal = match SkipToImplProposal::read_from_path(&skip_artifact_path) {
                    Ok(p) => p,
                    Err(err) => {
                        let _ = self.state.log_event(format!(
                            "warning: skip_proposal.toml malformed or invalid, falling through to spec review: {err:#}"
                        ));
                        None
                    }
                };

                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;

                match proposal {
                    Some(p) if p.proposed => {
                        self.state.skip_to_impl_rationale = Some(p.rationale);
                        self.state.skip_to_impl_kind = Some(p.status);
                        self.transition_to_phase(Phase::SkipToImplPending)?;
                    }
                    _ => {
                        self.transition_to_phase(Phase::SpecReviewRunning)?;
                    }
                }
            }
            Phase::SpecReviewRunning => {
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                self.transition_to_phase(Phase::SpecReviewPaused)?;
                self.append_system_message(
                    run.id,
                    MessageKind::Summary,
                    "Spec review complete. Press Enter to continue to planning, or n to run another review round.".to_string(),
                );
            }
            Phase::PlanningRunning => {
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                self.transition_to_phase(Phase::PlanReviewRunning)?;
            }
            Phase::PlanReviewRunning => {
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                self.transition_to_phase(Phase::PlanReviewPaused)?;
                self.append_system_message(
                    run.id,
                    MessageKind::Summary,
                    "Plan review complete. Press Enter to continue to sharding, or n to run another review round.".to_string(),
                );
            }
            Phase::ShardingRunning => {
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                let parsed = tasks::validate(&tasks_path)
                    .with_context(|| format!("invalid {}", tasks_path.display()));
                match parsed {
                    Ok(parsed) => {
                        self.state.builder.task_titles = parsed
                            .tasks
                            .iter()
                            .map(|t| (t.id, t.title.clone()))
                            .collect();
                        self.state.builder.reset_task_pipeline(
                            parsed
                                .tasks
                                .iter()
                                .map(|task| (task.id, Some(task.title.clone()))),
                        );
                        self.finalize_run_record(run.id, true, None);
                        self.state.agent_error = None;
                        self.transition_to_phase(Phase::ImplementationRound(1))?;
                    }
                    Err(err) => return Err(err),
                }
            }
            Phase::ImplementationRound(round) => {
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                self.transition_to_phase(Phase::ReviewRound(round))?;
            }
            Phase::ReviewRound(round) => {
                let review_path = session_dir
                    .join("rounds")
                    .join(format!("{round:03}"))
                    .join("review.toml");
                match review::validate(&review_path) {
                    Ok(verdict) => {
                        let summary_text = verdict.summary.trim();
                        if !summary_text.is_empty() {
                            let kind = match verdict.status {
                                review::ReviewStatus::Approved => MessageKind::Summary,
                                review::ReviewStatus::Revise
                                | review::ReviewStatus::HumanBlocked
                                | review::ReviewStatus::AgentPivot => MessageKind::SummaryWarn,
                            };
                            let msg = Message {
                                ts: chrono::Utc::now(),
                                run_id: run.id,
                                kind,
                                sender: MessageSender::Agent {
                                    model: run.model.clone(),
                                    vendor: run.vendor.clone(),
                                },
                                text: summary_text.to_string(),
                            };
                            if let Err(err) = self.state.append_message(&msg) {
                                let _ = self.state.log_event(format!(
                                    "failed to append review summary message for run {}: {err}",
                                    run.id
                                ));
                            } else {
                                self.messages.push(msg);
                            }
                        }
                        self.finalize_run_record(run.id, true, None);
                        self.state.agent_error = None;
                        self.state.builder.last_verdict =
                            Some(format!("{:?}", verdict.status).to_lowercase());
                        match verdict.status {
                            review::ReviewStatus::Approved => {
                                // Advisory feedback on an approved verdict is non-blocking;
                                // surface it to the UI but continue the pipeline.
                                if !verdict.feedback.is_empty() {
                                    let advisory = format!(
                                        "advisory ({}): {}",
                                        verdict.feedback.len(),
                                        verdict.feedback[0].trim()
                                    );
                                    let advisory_msg = Message {
                                        ts: chrono::Utc::now(),
                                        run_id: run.id,
                                        kind: MessageKind::SummaryWarn,
                                        sender: MessageSender::Agent {
                                            model: run.model.clone(),
                                            vendor: run.vendor.clone(),
                                        },
                                        text: advisory,
                                    };
                                    if let Err(err) = self.state.append_message(&advisory_msg) {
                                        let _ = self.state.log_event(format!(
                                            "failed to append advisory feedback message: {err}"
                                        ));
                                    } else {
                                        self.messages.push(advisory_msg);
                                    }
                                }
                                if let Some(task_id) = self.state.builder.current_task_id() {
                                    let _ = self.state.builder.set_task_status(
                                        task_id,
                                        PipelineItemStatus::Approved,
                                        Some(round),
                                    );
                                }
                                if !self.state.builder.has_unfinished_tasks() {
                                    self.transition_to_phase(Phase::Done)?;
                                } else {
                                    self.transition_to_phase(Phase::ImplementationRound(
                                        round + 1,
                                    ))?;
                                }
                            }
                            review::ReviewStatus::Revise => {
                                if let Some(task_id) = self.state.builder.current_task_id() {
                                    if verdict.new_tasks.is_empty() {
                                        let _ = self.state.builder.set_task_status(
                                            task_id,
                                            PipelineItemStatus::Revise,
                                            Some(round),
                                        );
                                    } else {
                                        let new_tasks = verdict
                                            .new_tasks
                                            .iter()
                                            .map(|task| {
                                                (
                                                    task.title.clone(),
                                                    task.description.clone(),
                                                    task.test.clone(),
                                                    task.estimated_tokens,
                                                )
                                            })
                                            .collect::<Vec<_>>();
                                        let assigned_ids = assigned_revise_task_ids(
                                            &self.state.builder,
                                            new_tasks.len(),
                                        );
                                        rewrite_tasks_for_revise(
                                            &session_dir,
                                            task_id,
                                            &verdict.new_tasks,
                                            &assigned_ids,
                                        )?;
                                        self.state
                                            .builder
                                            .apply_revise_with_new_tasks(task_id, new_tasks);
                                        if let Some(first_inserted) = assigned_ids.first().copied()
                                        {
                                            self.state.builder.current_task = Some(first_inserted);
                                        }
                                    }
                                }
                                self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
                            }
                            review::ReviewStatus::HumanBlocked
                            | review::ReviewStatus::AgentPivot => {
                                let (verdict_status, trigger_str) = match verdict.status {
                                    review::ReviewStatus::HumanBlocked => {
                                        (PipelineItemStatus::HumanBlocked, "human_blocked")
                                    }
                                    review::ReviewStatus::AgentPivot => {
                                        (PipelineItemStatus::AgentPivot, "agent_pivot")
                                    }
                                    review::ReviewStatus::Approved
                                    | review::ReviewStatus::Revise => {
                                        unreachable!("already handled")
                                    }
                                };
                                if let Some(task_id) = self.state.builder.current_task_id() {
                                    let _ = self.state.builder.set_task_status(
                                        task_id,
                                        verdict_status,
                                        Some(round),
                                    );
                                }
                                let summary = verdict.feedback.join("\n");
                                let trigger_summary =
                                    (!summary.trim().is_empty()).then_some(summary);
                                self.enter_builder_recovery(
                                    round,
                                    self.state.builder.current_task_id(),
                                    trigger_summary,
                                    trigger_str,
                                );
                            }
                        }
                    }
                    Err(err) => return Err(err),
                }
            }
            Phase::BuilderRecovery(round) => match self.reconcile_builder_recovery(run.id) {
                Ok(()) => {
                    self.finalize_run_record(run.id, true, None);
                    self.state.agent_error = None;
                    // Insert the recovery-mode plan review pipeline item before
                    // transitioning so the UI shows it as the next pending stage.
                    self.state.builder.push_pipeline_item(PipelineItem {
                        id: 0,
                        stage: "plan-review".to_string(),
                        task_id: None,
                        round: Some(round),
                        status: PipelineItemStatus::Pending,
                        title: Some("Recovery plan review".to_string()),
                        mode: Some("recovery".to_string()),
                        trigger: None,
                        interactive: Some(false),
                    });
                    self.transition_to_phase(Phase::BuilderRecoveryPlanReview(round))?;
                }
                Err(err) => {
                    let reason = format!("recovery_reconcile_failed: {err:#}");
                    self.finalize_run_record(run.id, false, Some(reason.clone()));
                    let failed_run = self
                        .state
                        .agent_runs
                        .iter()
                        .find(|candidate| candidate.id == run.id)
                        .cloned()
                        .unwrap_or_else(|| run.clone());
                    if !self.maybe_auto_retry(&failed_run) {
                        self.state.agent_error = Some(reason);
                    }
                }
            },
            Phase::BuilderRecoveryPlanReview(round) => {
                self.handle_recovery_plan_review_completed(run, round)?;
            }
            Phase::BuilderRecoverySharding(round) => {
                self.handle_recovery_sharding_completed(run, round)?;
            }
            Phase::IdeaInput
            | Phase::SpecReviewPaused
            | Phase::PlanReviewPaused
            | Phase::BlockedNeedsUser
            | Phase::SkipToImplPending
            | Phase::GitGuardPending
            | Phase::Done => {}
        }
        Ok(())
    }

    fn launch_brainstorm(&mut self, idea: String) {
        let _ = self.launch_brainstorm_with_model(idea, None);
    }

    fn launch_brainstorm_with_model(
        &mut self,
        idea: String,
        override_model: Option<CachedModel>,
    ) -> bool {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| Self::select_brainstorm_model(&self.models, &self.versions))
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error =
                Some("no model available with quota — check model strip".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let session_id = &self.state.session_id;
        let prompt_path = session_state::session_dir(session_id)
            .join("prompts")
            .join("brainstorm.md");
        let spec_path = session_state::session_dir(session_id)
            .join("artifacts")
            .join("spec.md");

        let _ = std::fs::remove_file(&spec_path);
        let _ = std::fs::remove_file(
            session_state::session_dir(session_id)
                .join("artifacts")
                .join(ArtifactKind::SkipToImpl.filename()),
        );

        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let attempt = self.attempt_for("brainstorm", None, 1);
        let live_summary_path = self.live_summary_path_for_run("brainstorm", None, 1, attempt);
        let prompt = brainstorm_prompt(
            &idea,
            &spec_path.display().to_string(),
            &live_summary_path.display().to_string(),
        );
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.state.agent_error = Some(format!("error writing prompt: {e}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let status_path = self.run_status_path_for("brainstorm", None, 1, attempt);
        let dirty = self.capture_run_guard(
            "brainstorm",
            None,
            1,
            attempt,
            guard::GuardMode::AskOperator,
        );
        let adapter = adapter_for_vendor(vendor_kind);
        let window_name = window_name_with_model("[Brainstorm]", &model);
        let run_key = Self::run_key_for("brainstorm", None, 1, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&spec_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            launch_interactive(
                &window_name,
                &run,
                adapter.as_ref(),
                true,
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.state.idea_text = Some(idea.clone());
                self.state.selected_model = Some(model.clone());
                let _ = self.transition_to_phase(Phase::BrainstormRunning);
                self.start_run_tracking("brainstorm", None, 1, model, vendor, window_name);
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(e) => {
                self.state.agent_error = Some(format!("failed to launch brainstorm: {e}"));
                false
            }
        }
    }

    fn select_brainstorm_model<'a>(
        models: &'a [CachedModel],
        versions: &VersionIndex,
    ) -> Option<&'a CachedModel> {
        pick_for_phase(models, SelectionPhase::Idea, None, versions)
    }

    fn launch_retry_for_stage(
        &mut self,
        failed_run: &crate::state::RunRecord,
        chosen: CachedModel,
    ) -> bool {
        match failed_run.stage.as_str() {
            "brainstorm" => {
                let Some(idea) = self.state.idea_text.clone() else {
                    return false;
                };
                self.launch_brainstorm_with_model(idea, Some(chosen))
            }
            "spec-review" => self.launch_spec_review_with_model(Some(chosen)),
            "planning" => self.launch_planning_with_model(Some(chosen), true),
            "plan-review" => self.launch_plan_review_with_model(Some(chosen)),
            "sharding" => self.launch_sharding_with_model(Some(chosen)),
            "recovery" => self.launch_recovery_with_model(Some(chosen)),
            "coder" => self.launch_coder_with_model(Some(chosen)),
            "reviewer" => self.launch_reviewer_with_model(Some(chosen)),
            _ => false,
        }
    }

    fn launch_spec_review(&mut self) {
        let _ = self.launch_spec_review_with_model(None);
    }

    fn launch_spec_review_with_model(&mut self, override_model: Option<CachedModel>) -> bool {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }

        let round = match self.state.current_phase {
            Phase::SpecReviewPaused => self.completed_rounds("spec-review") + 1,
            _ => self.completed_rounds("spec-review").max(1),
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let review_path = session_dir
            .join("artifacts")
            .join(format!("spec-review-{round}.md"));
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("spec-review-{round}.md"));

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| {
                let runs: Vec<_> = self
                    .state
                    .agent_runs
                    .iter()
                    .filter(|run| {
                        (run.stage == "brainstorm"
                            || (run.stage == "spec-review" && run.round == round))
                            && run.status == RunStatus::Done
                    })
                    .cloned()
                    .collect();
                let (used_vendors, used_models) = Self::used_review_pairs(&runs);
                select_for_review(&self.models, &used_vendors, &used_models, &self.versions)
            })
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available for review".to_string());
            let _ = self.state.save();
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let attempt = self.attempt_for("spec-review", None, round);
        let live_summary_path = self.live_summary_path_for_run("spec-review", None, round, attempt);
        let prompt = spec_review_prompt(
            &spec_path.display().to_string(),
            &review_path.display().to_string(),
            &live_summary_path.display().to_string(),
        );
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt) {
            self.state.agent_error = Some(format!("error writing prompt: {err}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
        };
        let window_name = window_name_with_model(&format!("[Spec Review {round}]"), &model);
        let status_path = self.run_status_path_for("spec-review", None, round, attempt);
        let dirty = self.capture_run_guard(
            "spec-review",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let run_key = Self::run_key_for("spec-review", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&review_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("spec-review", None, round, model, vendor, window_name);
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(err) => {
                self.state.agent_error = Some(format!("failed to launch spec review: {err}"));
                false
            }
        }
    }

    fn launch_planning(&mut self) {
        let _ = self.launch_planning_with_model(None, true);
    }

    fn launch_planning_with_model(
        &mut self,
        override_model: Option<CachedModel>,
        interactive: bool,
    ) -> bool {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");

        let review_paths: Vec<std::path::PathBuf> = self
            .state
            .agent_runs
            .iter()
            .filter(|run| run.stage == "spec-review" && run.status == RunStatus::Done)
            .map(|run| {
                session_dir
                    .join("artifacts")
                    .join(format!("spec-review-{}.md", run.round))
            })
            .filter(|path| path.exists())
            .collect();

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| {
                pick_for_phase(&self.models, SelectionPhase::Planning, None, &self.versions)
            })
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let _ = std::fs::remove_file(&plan_path);

        let prompt_path = session_dir.join("prompts").join("planning.md");
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let attempt = self.attempt_for("planning", None, 1);
        let live_summary_path = self.live_summary_path_for_run("planning", None, 1, attempt);
        let prompt = planning_prompt(&spec_path, &review_paths, &plan_path, &live_summary_path);
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            let _ = self.state.log_event(format!("error writing prompt: {e}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let adapter = adapter_for_vendor(vendor_kind);
        let status_path = self.run_status_path_for("planning", None, 1, attempt);
        let guard_mode = if interactive {
            guard::GuardMode::AskOperator
        } else {
            guard::GuardMode::AutoReset
        };
        let dirty = self.capture_run_guard("planning", None, 1, attempt, guard_mode);
        let window_name = window_name_with_model("[Planning]", &model);
        let run_key = Self::run_key_for("planning", None, 1, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&plan_path), &run_key, &artifacts_dir)
        {
            result
        } else if interactive {
            launch_interactive(
                &window_name,
                &run,
                adapter.as_ref(),
                true,
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        } else {
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("planning", None, 1, model, vendor, window_name);
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(e) => {
                let _ = self
                    .state
                    .log_event(format!("failed to launch planning: {e}"));
                false
            }
        }
    }

    fn launch_plan_review(&mut self) {
        let _ = self.launch_plan_review_with_model(None);
    }

    fn launch_plan_review_with_model(&mut self, override_model: Option<CachedModel>) -> bool {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }

        let round = match self.state.current_phase {
            Phase::PlanReviewPaused => self.completed_rounds("plan-review") + 1,
            _ => self.completed_rounds("plan-review").max(1),
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");
        let review_path = session_dir
            .join("artifacts")
            .join(format!("plan-review-{round}.md"));
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("plan-review-{round}.md"));

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| {
                let runs: Vec<_> = self
                    .state
                    .agent_runs
                    .iter()
                    .filter(|run| {
                        (run.stage == "planning"
                            || (run.stage == "plan-review" && run.round == round))
                            && run.status == RunStatus::Done
                    })
                    .cloned()
                    .collect();
                let (used_vendors, used_models) = Self::used_review_pairs(&runs);
                select_for_review(&self.models, &used_vendors, &used_models, &self.versions)
            })
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available for review".to_string());
            let _ = self.state.save();
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let attempt = self.attempt_for("plan-review", None, round);
        let live_summary_path = self.live_summary_path_for_run("plan-review", None, round, attempt);
        let prompt = plan_review_prompt(
            &spec_path.display().to_string(),
            &plan_path.display().to_string(),
            &review_path.display().to_string(),
            &live_summary_path.display().to_string(),
        );
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt) {
            self.state.agent_error = Some(format!("error writing prompt: {err}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
        };
        let window_name = window_name_with_model(&format!("[Plan Review {round}]"), &model);
        let status_path = self.run_status_path_for("plan-review", None, round, attempt);
        let dirty = self.capture_run_guard(
            "plan-review",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let run_key = Self::run_key_for("plan-review", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&review_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("plan-review", None, round, model, vendor, window_name);
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(err) => {
                self.state.agent_error = Some(format!("failed to launch plan review: {err}"));
                false
            }
        }
    }

    fn launch_sharding(&mut self) {
        let _ = self.launch_sharding_with_model(None);
    }

    fn launch_sharding_with_model(&mut self, override_model: Option<CachedModel>) -> bool {
        self.state.agent_error = None;

        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| {
                pick_for_phase(&self.models, SelectionPhase::Planning, None, &self.versions)
            })
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let _ = std::fs::remove_file(&tasks_path);

        let prompt_path = session_dir.join("prompts").join("sharding.md");
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let attempt = self.attempt_for("sharding", None, 1);
        let live_summary_path = self.live_summary_path_for_run("sharding", None, 1, attempt);
        let prompt = sharding_prompt(&spec_path, &plan_path, &tasks_path, &live_summary_path);
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            let _ = self.state.log_event(format!("error writing prompt: {e}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let status_path = self.run_status_path_for("sharding", None, 1, attempt);
        let dirty =
            self.capture_run_guard("sharding", None, 1, attempt, guard::GuardMode::AutoReset);
        let window_name = window_name_with_model("[Sharding]", &model);
        let run_key = Self::run_key_for("sharding", None, 1, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&tasks_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("sharding", None, 1, model, vendor, window_name);
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(e) => {
                let _ = self
                    .state
                    .log_event(format!("failed to launch sharding: {e}"));
                false
            }
        }
    }

    fn launch_recovery(&mut self) {
        let _ = self.launch_recovery_with_model(None);
    }

    fn launch_recovery_with_model(&mut self, override_model: Option<CachedModel>) -> bool {
        use anyhow::Context;

        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }
        let Phase::BuilderRecovery(round) = self.state.current_phase else {
            return false;
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let recovery_path = session_dir
            .join("rounds")
            .join(format!("{round:03}"))
            .join("recovery.toml");
        let _ = std::fs::remove_file(&recovery_path);
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("recovery-r{round}.md"));

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| {
                pick_for_phase(&self.models, SelectionPhase::Planning, None, &self.versions)
            })
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let is_human_blocked = self
            .state
            .builder
            .pipeline_items_by_stage("recovery")
            .iter()
            .find(|i| i.status == PipelineItemStatus::Running)
            .and_then(|i| i.trigger.as_deref())
            == Some("human_blocked");

        let completed = self.state.builder.done_task_ids();
        let mut started = self
            .started_builder_task_ids()
            .into_iter()
            .collect::<Vec<_>>();
        started.sort_unstable();
        let attempt = self.attempt_for("recovery", None, round);
        let live_summary_path = self.live_summary_path_for_run("recovery", None, round, attempt);
        let prompt = recovery_prompt(
            &spec_path,
            &plan_path,
            &tasks_path,
            self.state.builder.recovery_trigger_task_id,
            self.state.builder.recovery_trigger_summary.as_deref(),
            &completed,
            &started,
            &live_summary_path,
            &recovery_path,
            is_human_blocked,
        );
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt)
            .with_context(|| format!("cannot write {}", prompt_path.display()))
        {
            self.state.agent_error = Some(err.to_string());
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
        };
        let status_path = self.run_status_path_for("recovery", None, round, attempt);
        let recovery_guard_mode = if is_human_blocked {
            guard::GuardMode::AskOperator
        } else {
            guard::GuardMode::AutoReset
        };
        let dirty = self.capture_run_guard("recovery", None, round, attempt, recovery_guard_mode);
        let window_name = window_name_with_model("[Recovery]", &model);
        let run_key = Self::run_key_for("recovery", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&tasks_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            if is_human_blocked {
                launch_interactive(
                    &window_name,
                    &run,
                    adapter.as_ref(),
                    true,
                    &status_path,
                    &run_key,
                    &artifacts_dir,
                )
            } else {
                launch_noninteractive(
                    &window_name,
                    &run,
                    adapter.as_ref(),
                    &status_path,
                    &run_key,
                    &artifacts_dir,
                )
            }
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("recovery", None, round, model, vendor, window_name);
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(err) => {
                self.state.agent_error = Some(format!("failed to launch recovery: {err}"));
                false
            }
        }
    }

    fn launch_coder(&mut self) {
        let _ = self.launch_coder_with_model(None);
    }

    fn launch_coder_with_model(&mut self, override_model: Option<CachedModel>) -> bool {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }
        let Phase::ImplementationRound(r) = self.state.current_phase else {
            return false;
        };

        let Some(task_id) = self.ensure_builder_task_for_round(r) else {
            self.state.agent_error = Some("no pending tasks".to_string());
            let _ = self.state.save();
            return false;
        };

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let round_dir = session_dir.join("rounds").join(format!("{r:03}"));
        let task_file = round_dir.join("task.toml");

        if !task_file.exists() {
            let body = task_toml_for(&session_dir, task_id).unwrap_or_else(|e| {
                format!("# task body could not be loaded: {e}\nid = {task_id}\n")
            });
            let _ = std::fs::write(&task_file, body);
        }

        // Pin the base HEAD before the coder runs; preserves original base on resume.
        self.capture_round_base(&round_dir);

        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| pick_for_phase(&self.models, SelectionPhase::Build, None, &self.versions))
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available with quota".to_string());
            let _ = self.state.save();
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let prompt_path = session_dir.join("prompts").join(format!("coder-r{r}.md"));
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let attempt = self.attempt_for("coder", Some(task_id), r);
        let live_summary_path = self.live_summary_path_for_run("coder", Some(task_id), r, attempt);
        let resume = self
            .state
            .agent_runs
            .iter()
            .any(|run| run.stage == "coder" && run.task_id == Some(task_id) && run.round == r);
        let prompt = coder_prompt(
            &session_dir,
            task_id,
            r,
            &task_file,
            &live_summary_path,
            resume,
        );
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            let _ = self.state.log_event(format!("error writing prompt: {e}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let window_name = window_name_with_model(&format!("[Coder r{r}]"), &model);
        let status_path = self.run_status_path_for("coder", Some(task_id), r, attempt);
        self.capture_run_guard(
            "coder",
            Some(task_id),
            r,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let run_key = Self::run_key_for("coder", Some(task_id), r, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, None, &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("coder", Some(task_id), r, model, vendor, window_name);
                true
            }
            Err(e) => {
                let _ = self.state.log_event(format!("failed to launch coder: {e}"));
                false
            }
        }
    }

    fn launch_reviewer(&mut self) {
        let _ = self.launch_reviewer_with_model(None);
    }

    fn launch_reviewer_with_model(&mut self, override_model: Option<CachedModel>) -> bool {
        self.state.agent_error = None;
        if self.models.is_empty() {
            self.state.agent_error =
                Some("model list not yet loaded — wait a moment and try again".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }
        let Phase::ReviewRound(r) = self.state.current_phase else {
            return false;
        };
        let Some(task_id) = self.state.builder.current_task_id() else {
            self.state.agent_error = Some("no current task".to_string());
            let _ = self.state.save();
            return false;
        };

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let round_dir = session_dir.join("rounds").join(format!("{r:03}"));
        let review_path = round_dir.join("review.toml");
        let review_scope_file = round_dir.join("review_scope.toml");
        let task_file = round_dir.join("task.toml");

        let _ = std::fs::remove_file(&review_path);

        let excluded = self
            .state
            .agent_runs
            .iter()
            .filter(|run| {
                (run.stage == "reviewer" || run.stage == "coder")
                    && run.task_id == Some(task_id)
                    && run.round == r
            })
            .cloned()
            .collect::<Vec<_>>();
        let Some(chosen) = override_model
            .as_ref()
            .or_else(|| {
                let (used_vendors, used_models) = Self::used_review_pairs(&excluded);
                select_for_review(&self.models, &used_vendors, &used_models, &self.versions)
            })
            .map(|m| (m.name.clone(), m.vendor, vendor_tag(m.vendor).to_string()))
        else {
            self.state.agent_error = Some("no model available for review".to_string());
            let _ = self.state.save();
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let attempt = self.attempt_for("reviewer", Some(task_id), r);
        let live_summary_path =
            self.live_summary_path_for_run("reviewer", Some(task_id), r, attempt);
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("reviewer-r{r}.md"));
        let prompt = reviewer_prompt(
            &session_dir,
            task_id,
            r,
            &task_file,
            &review_scope_file,
            &review_path,
            &live_summary_path,
        );
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            let _ = self.state.log_event(format!("error writing prompt: {e}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
        };

        let window_name = window_name_with_model(&format!("[Review r{r}]"), &model);
        let status_path = self.run_status_path_for("reviewer", Some(task_id), r, attempt);
        let dirty = self.capture_run_guard(
            "reviewer",
            Some(task_id),
            r,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let run_key = Self::run_key_for("reviewer", Some(task_id), r, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&review_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking("reviewer", Some(task_id), r, model, vendor, window_name);
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(e) => {
                let _ = self
                    .state
                    .log_event(format!("failed to launch reviewer: {e}"));
                false
            }
        }
    }

    fn setup_watcher(&mut self) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let watcher_result = notify::RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if res.is_ok() {
                    let _ = tx.send(());
                }
            },
            notify::Config::default(),
        );
        match watcher_result {
            Ok(mut watcher) => {
                let Some(path) = self.live_summary_path.clone() else {
                    return Ok(());
                };
                if let Err(e) = watcher.watch(&path, notify::RecursiveMode::NonRecursive) {
                    let _ = self
                        .state
                        .log_event(format!("watcher setup failed: {}, falling back to poll", e));
                    return Ok(());
                }
                self.live_summary_watcher = Some(watcher);
                self.live_summary_change_rx = Some(rx);
                Ok(())
            }
            Err(e) => {
                let _ = self
                    .state
                    .log_event(format!("watcher init failed: {}, falling back to poll", e));
                Ok(())
            }
        }
    }

    fn process_live_summary_changes(&mut self) {
        if let Some(ref rx) = self.live_summary_change_rx {
            let mut saw_change = false;
            while rx.try_recv().is_ok() {
                saw_change = true;
            }
            if saw_change {
                self.read_live_summary_pipeline();
            }
        } else {
            self.poll_live_summary_fallback();
        }
    }

    fn poll_live_summary_fallback(&mut self) {
        if !self.window_launched {
            self.live_summary_cached_text.clear();
            self.live_summary_cached_mtime = None;
            return;
        }
        let Some(path) = self.live_summary_path.clone() else {
            self.live_summary_cached_text.clear();
            return;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            self.live_summary_cached_text.clear();
            self.live_summary_cached_mtime = None;
            return;
        };
        let Ok(mtime) = meta.modified() else { return };
        let stale = mtime
            .elapsed()
            .map(|d| d > std::time::Duration::from_secs(60))
            .unwrap_or(true);
        if stale {
            self.live_summary_cached_text.clear();
            return;
        }
        let should_read = match self.live_summary_cached_mtime {
            None => true,
            Some(cached) => mtime > cached,
        };
        if should_read {
            self.read_live_summary_pipeline();
        }
    }

    fn read_live_summary_pipeline(&mut self) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        let Some(run) = self.running_run() else {
            return;
        };
        if !tmux::window_exists(&run.window_name) {
            return;
        }
        let Some(path) = self.live_summary_path.clone() else {
            return;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        let Ok(mtime) = meta.modified() else { return };
        if let Some(cached_mtime) = self.live_summary_cached_mtime
            && mtime <= cached_mtime
        {
            return;
        }
        let stale = mtime
            .elapsed()
            .map(|d| d > std::time::Duration::from_secs(60))
            .unwrap_or(true);
        if stale {
            return;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };
        let sanitized = render::sanitize_live_summary(&content);
        if sanitized.is_empty() {
            return;
        }
        if sanitized == self.live_summary_cached_text {
            return;
        }
        let msg = Message {
            ts: chrono::Utc::now(),
            run_id,
            kind: MessageKind::Brief,
            sender: MessageSender::Agent {
                model: run.model.clone(),
                vendor: run.vendor.clone(),
            },
            text: sanitized.clone(),
        };
        if let Err(err) = self.state.append_message(&msg) {
            let _ = self.state.log_event(format!(
                "failed to append brief message for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(msg);
        }
        self.live_summary_cached_text = sanitized;
        self.live_summary_cached_mtime = Some(mtime);
    }

    /// Final read + cleanup of the live-summary file when a run finishes.
    /// Emits any last summary as a Brief message, then deletes the file so
    /// the next run starts with a clean slate.
    fn drain_live_summary(&mut self, run: &crate::state::RunRecord) {
        let path = self.live_summary_path_for(run);
        if let Ok(content) = std::fs::read_to_string(&path) {
            let sanitized = render::sanitize_live_summary(&content);
            if !sanitized.is_empty() && sanitized != self.live_summary_cached_text {
                let msg = Message {
                    ts: chrono::Utc::now(),
                    run_id: run.id,
                    kind: MessageKind::Brief,
                    sender: MessageSender::Agent {
                        model: run.model.clone(),
                        vendor: run.vendor.clone(),
                    },
                    text: sanitized,
                };
                if let Err(err) = self.state.append_message(&msg) {
                    let _ = self.state.log_event(format!(
                        "failed to append final brief message for run {}: {err}",
                        run.id
                    ));
                } else {
                    self.messages.push(msg);
                }
            }
        }
        let _ = std::fs::remove_file(&path);
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
    }
}

fn kill_window(base: &str) {
    // Windows are now named "[Base] <model>", so match by prefix: exact match
    // or the base followed by a space. The base ends with `]`, which prevents
    // `[Coder r1]` from accidentally matching `[Coder r10]`, etc.
    let prefix = format!("{base} ");
    let Ok(output) = std::process::Command::new("tmux")
        .args(["list-windows", "-F", "#{window_name}"])
        .output()
    else {
        return;
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in stdout.lines() {
        if name == base || name.starts_with(&prefix) {
            let _ = std::process::Command::new("tmux")
                .args(["kill-window", "-t", name])
                .output();
        }
    }
}

fn restore_artifacts(pairs: &[(&std::path::Path, &std::path::Path)]) {
    for (backup, target) in pairs {
        if backup.exists() {
            let _ = std::fs::copy(backup, target);
        }
    }
}

fn task_toml_for(session_dir: &std::path::Path, task_id: u32) -> anyhow::Result<String> {
    use anyhow::Context;
    let tasks_path = session_dir.join("artifacts").join("tasks.toml");
    let parsed = tasks::validate(&tasks_path).context("load tasks.toml")?;
    let task = parsed
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .ok_or_else(|| anyhow::anyhow!("task id {task_id} not found"))?;
    toml::to_string_pretty(task).context("serialize task.toml")
}

fn assigned_revise_task_ids(builder: &session_state::BuilderState, count: usize) -> Vec<u32> {
    let mut next_id = builder.max_task_id() + 1;
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        ids.push(next_id);
        next_id += 1;
    }
    ids
}

fn rewrite_tasks_for_revise(
    session_dir: &std::path::Path,
    current_task_id: u32,
    new_tasks: &[tasks::Task],
    assigned_ids: &[u32],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        new_tasks.len() == assigned_ids.len(),
        "new task count does not match assigned id count"
    );
    let tasks_path = session_dir.join("artifacts").join("tasks.toml");
    let parsed = tasks::validate(&tasks_path).context("load tasks.toml before revise")?;
    let Some(current_idx) = parsed
        .tasks
        .iter()
        .position(|task| task.id == current_task_id)
    else {
        anyhow::bail!("task id {current_task_id} not found in tasks.toml");
    };

    let mut rewritten = Vec::with_capacity(parsed.tasks.len() + new_tasks.len());
    rewritten.extend(parsed.tasks[..current_idx].iter().cloned());

    for (task, id) in new_tasks.iter().zip(assigned_ids.iter().copied()) {
        let mut inserted = task.clone();
        inserted.id = id;
        rewritten.push(inserted);
    }

    let mut next_pending_id = assigned_ids
        .iter()
        .copied()
        .max()
        .unwrap_or_else(|| parsed.tasks.iter().map(|task| task.id).max().unwrap_or(0))
        + 1;
    for task in parsed.tasks[current_idx + 1..].iter().cloned() {
        let mut renumbered = task;
        renumbered.id = next_pending_id;
        next_pending_id += 1;
        rewritten.push(renumbered);
    }

    let file = tasks::TasksFile { tasks: rewritten };
    let text = toml::to_string_pretty(&file).context("serialize revised tasks.toml")?;
    std::fs::write(&tasks_path, text)
        .with_context(|| format!("write revised {}", tasks_path.display()))?;
    Ok(())
}

fn validate_stage_toml_writes(
    session_dir: &std::path::Path,
    stage: &str,
    round: u32,
) -> anyhow::Result<()> {
    let Some(io) = session_state::transitions::stage_io(stage) else {
        return Ok(());
    };
    let round_token = format!("{round:03}");
    let paths = io
        .writes
        .iter()
        .filter(|template| template.ends_with(".toml"))
        .map(|template| session_dir.join(template.replace("{round}", &round_token)))
        .collect::<Vec<_>>();
    let refs = paths.iter().map(|path| path.as_path()).collect::<Vec<_>>();
    crate::runner::validate_toml_artifacts(&refs)
}

fn read_review_scope_base_sha(path: &std::path::Path) -> anyhow::Result<String> {
    #[derive(serde::Deserialize)]
    struct ReviewScope {
        base_sha: String,
    }

    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let scope: ReviewScope =
        toml::from_str(&text).with_context(|| format!("malformed TOML in {}", path.display()))?;
    let base = scope.base_sha.trim().to_string();
    if base.is_empty() {
        anyhow::bail!("base_sha is empty in {}", path.display());
    }
    Ok(base)
}

/// Prepended to every agent prompt. Surfaces project-specific guidance
/// (CLAUDE.md / AGENTS.md) before the agent acts.
const PROJECT_DOC_INSTR: &str = "If CLAUDE.md or AGENTS.md exist in the repo, read it first and follow those directions carefully.\n\n";

fn live_summary_instruction(path: &std::path::Path) -> String {
    format!(
        "\n\nEvery 2–3 min (and whenever your sub-goal changes), overwrite {} \
         with one plain-text line formatted as: `<short title> | <normal \
         summary in a short paragraph>`. The short title (≤5 words) MUST \
         capture the real essence, not a generic label, and SHOULD vary \
         between updates whenever the focus shifts — avoid repeating the \
         same title across successive writes when a different phrasing \
         honestly reflects the current sub-goal. The summary is a single \
         short paragraph covering current progress and next action in \
         normal prose. Your process is killed if this file isn't updated for \
         10 min of wall time (time spent inside tool calls is excluded from \
         that budget).\n",
        path.display()
    )
}

fn spec_review_prompt(spec_path: &str, review_path: &str, live_summary_path: &str) -> String {
    let instr = live_summary_instruction(std::path::Path::new(live_summary_path));
    format!(
        r#"{PROJECT_DOC_INSTR}You review a spec. NON-INTERACTIVE — no clarifying questions; judge from the
spec alone. Do NOT modify code; write ONLY the review file.

Spec:   {spec_path}
Output: {review_path}

Evaluate clarity, completeness, buildability, risks, and gaps. The review MUST cover:
  - Verdict: approve / approve-with-changes / reject
  - Specific issues (if any), each with a suggested fix
  - Open risks the spec does not address
{instr}"#
    )
}

fn plan_review_prompt(
    spec_path: &str,
    plan_path: &str,
    review_path: &str,
    live_summary_path: &str,
) -> String {
    let instr = live_summary_instruction(std::path::Path::new(live_summary_path));
    format!(
        r#"{PROJECT_DOC_INSTR}You review an implementation plan. NON-INTERACTIVE — no clarifying questions.

Inputs:
  Plan: {plan_path}
  Spec: {spec_path}

Flag ONLY critical issues — things that would block or break implementation:
  - Spec requirement with no corresponding plan step.
  - Plan steps ordered unbuildably (a step depends on something a later step creates).
  - Contradictions plan↔spec, or internal contradictions that would lead to the
    wrong build.
  - File paths, function names, or interfaces inconsistent across steps in a way
    that would cause real breakage.
  - Spec-level ambiguity severe enough that an implementer could not proceed.
Multiple valid implementations is NOT a defect; don't force one internal design
when several options satisfy the spec and any explicit interfaces.

If — and only if — you find critical issues, directly edit {plan_path} (and
{spec_path} if spec-level) with the smallest fix. Then write a markdown-bullet
changelog of what you changed and why to {review_path}. If nothing was critical,
write a single bullet saying so — do NOT invent issues to fill space.

Do NOT flag or fix: typos, grammar, wording, formatting, style, tone, structural
polish, missing low-level implementation detail, absence of prescribed helper/
function structure, multiple possible approaches (unless the plan/spec makes an
explicit interface commitment that is internally contradictory), hypothetical
edge cases the spec does not require, or minor nitpicks. When in doubt, leave it
alone — over-editing is worse than under-editing.

Rules: do NOT create or modify source code; do NOT run git or modify version
control; do NOT ask the operator.
{instr}"#
    )
}

fn brainstorm_prompt(idea: &str, spec_path: &str, live_summary_path: &str) -> String {
    let instr = live_summary_instruction(std::path::Path::new(live_summary_path));
    format!(
        r#"{PROJECT_DOC_INSTR}Invoke your brainstorming skill now.

Idea:
---
{idea}
---

When the skill asks where to write the design doc, write it to {spec_path}.

At the very TOP of the spec file, before anything else, include a short
"TL;DR" section: 3–6 bullet points capturing the key decisions so a lazy
reader can skim it in 30 seconds.

This is a spec-only phase: do NOT write or modify any code; the spec file is
your only output. Implementation happens in a later phase.

SKIP-TO-IMPLEMENTATION PROPOSAL (optional): after writing the spec, if the task
is small and self-contained enough that separate planning and sharding phases
would add no value (e.g. a single-file change, a bug fix with an obvious edit
site, a trivial refactor), you MAY write a skip proposal to
`artifacts/skip_proposal.toml` ALONGSIDE the spec. Format:
    proposed = true
    status = "skip_to_impl"
    rationale = "<=500 chars explaining why"
Only emit this when the spec genuinely needs no further breakdown. When in
doubt, omit the file — the normal spec-review → planning → sharding pipeline
is the default. If you emit `"proposed": true`, the rationale MUST be a
non-empty, <=500 character explanation the operator will read before
accepting. When proposing the skip, keep the spec concise — just enough for a
coder to implement directly (goal, edit sites, acceptance check); skip the
long-form sections a planning phase would normally expand.

NOTHING-TO-IMPLEMENT (optional): if there is genuinely nothing to do (already
in place, invalid premise, pure question), skip the spec and write ONLY:
    {{"proposed": true, "kind": "nothing_to_do", "rationale": "<=500 chars"}}
The operator confirms and the session ends. Use sparingly.

HARD rules — override anything the superpowers / brainstorming skill suggests:
  - Do NOT `git add`, `git commit`, `git stash`, or touch version control. The
    spec stays untracked; a later phase commits.
  - Do NOT ask the operator whether to continue, proceed to planning, move on,
    or run any follow-up skill — including any inline "continue to next stage"
    prompt the skill may offer. When the spec is written, STOP and exit. The
    orchestrator drives stage transitions.

The operator IS available to answer questions ABOUT THE DESIGN itself. When
you finish, end your final message with an explicit line asking the operator
to enter `/exit` if they have no further comments.
{instr}"#
    )
}

fn planning_prompt(
    spec_path: &std::path::Path,
    review_paths: &[std::path::PathBuf],
    plan_path: &std::path::Path,
    live_summary_path: &std::path::Path,
) -> String {
    let instr = live_summary_instruction(live_summary_path);
    let reviews_block = if review_paths.is_empty() {
        "(no spec reviews available — work from the spec alone)".to_string()
    } else {
        review_paths
            .iter()
            .enumerate()
            .map(|(i, p)| format!("  - review {}: {}", i + 1, p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        r#"{PROJECT_DOC_INSTR}Invoke your superpowers:writing-plans skill now.

You are turning an approved spec + any spec reviews into an implementation plan.

Inputs:
  Spec:    {spec}
  Reviews:
{reviews}

Triage reviews first: they may contradict each other. Decide what to incorporate,
what to reject, and why. If a trade-off is real and you cannot confidently make
it alone, ASK the operator — this is interactive.

Once every trade-off is resolved, do TWO things IN THIS ORDER:
  1. UPDATE the spec in place at {spec} so it reflects accepted feedback and
     every decision you just made. Another agent reading ONLY the spec must not
     be surprised by anything in the plan.
  2. Write the plan to {plan}. At the very TOP of the plan, before anything
     else, include a short "TL;DR" section: 3–6 bullet points summarising the
     key sequencing/interface decisions so a lazy reader can skim it in 30
     seconds.

Hard rules — override anything the writing-plans skill suggests:
  - Do NOT write or modify any code (source, configs, build scripts). You may
    only edit the spec and write the plan.
  - Do NOT `git add`, `git commit`, `git stash`, or touch version control; both
    files stay untracked (a later phase commits). Refuse if the skill offers to
    commit. Do NOT offer to run tests, commit, or push.
  - The plan MUST be an execution map for coordination. It SHOULD include:
      sequencing and dependencies (what order matters, and why); interfaces,
      integration points, and execution seams that must be honored; constraints
      from the spec that narrow the correct solution space; optional likely
      file/module touchpoints ONLY as orientation.
  - The plan MUST NOT read like a pseudo-implementation or patch recipe: no
      checkbox to-do lists or step-by-step coding instructions; no helper/
      function decomposition or function-by-function edit sequences; no patch-
      like ordering, "change this line then that line", or mini diffs; no
      mandated internal code shape (struct fields, method signatures, class
      layout) unless required by the spec or an explicit interface commitment
      needed for coordination.
  - Authority rule: the spec is the design contract and wins any conflict; the
      plan is advisory for implementation shape; the plan is authoritative ONLY
      for sequencing and explicit interface commitments it names. Do not turn
      advisory detail into an implementation contract.
  - Do NOT ask the operator whether to continue, proceed, start implementing,
      jump to coding, run the next skill, or skip any downstream stage. When
      the plan is written, STOP and exit — the orchestrator drives stage
      transitions.

The operator IS available for clarifying questions about the design itself.
When you finish, end your final message with an explicit line asking the
operator to enter `/exit` if they have no further comments.
{instr}"#,
        spec = spec_path.display(),
        reviews = reviews_block,
        plan = plan_path.display(),
        instr = instr,
    )
}

fn sharding_prompt(
    spec_path: &std::path::Path,
    plan_path: &std::path::Path,
    tasks_path: &std::path::Path,
    live_summary_path: &std::path::Path,
) -> String {
    let instr = live_summary_instruction(live_summary_path);
    format!(
        r#"{PROJECT_DOC_INSTR}You split an approved plan into actionable, self-contained, buildable tasks.
NON-INTERACTIVE — do NOT modify any code; your ONLY output is the tasks TOML.

Inputs:
  Spec: {spec}
  Plan: {plan}
Read both carefully before sharding.

Sizing:
  - Target ~100_000 tokens of implementation effort per task — small enough for
    one coding session without context compaction, large enough to be meaningful.
  - Decompose only when the plan warrants it. If the whole plan fits one ~100k
    session, a single-task tasks.toml is correct — do NOT force artificial
    splits. Bigger plans split along natural seams (subsystem / layer / phase).
  - Each task must be self-contained: buildable on its own (compiles / links /
    type-checks) by a single coding session. A task does NOT have to be
    independently testable — scaffolding or groundwork tasks that only become
    testable after a later task lands are allowed, AS LONG AS they still build
    cleanly on their own.
  - Unless a dependency is explicitly listed in a task's description, no task
    may assume another task has shipped first.

Required fields per task:
  - id               sequential integer starting at 1
  - title            very short summary (≤60 chars, imperative, no trailing
                     period) — shown as the task label in the pipeline UI
  - description      detailed what-to-do (multi-line TOML string allowed)
  - test             concrete verification steps, OR the literal string
                     "not testable" followed by a one-line reason (e.g.
                     "not testable — scaffolding; verified by task 4's tests").
                     Use "not testable" ONLY for genuine intermediate/
                     scaffolding tasks. The reviewer honors this by skipping
                     the test-pass check for such tasks, but still requires
                     the code to build.
  - estimated_tokens integer estimate (target ~100_000)
  - spec_refs        array of {{ path, lines }} pointing into the spec
  - plan_refs        array of {{ path, lines }} pointing into the plan
  `lines` is a range like "12-45" or a single number.

Description rules — outcome- and coordination-oriented:
  - SHOULD focus on required outcomes, dependencies/ordering, acceptance checks,
    and relevant interfaces/touchpoints (file/module touchpoints only as
    orientation).
  - MUST NOT be recipe-style: no step-by-step coding instructions; no miniature
    edit scripts or pseudo-patch sequences; no mandated internal design or
    helper/function decomposition unless required by the spec or an explicit
    interface commitment needed for coordination.
  - `plan_refs` MUST point to plan content about goals, sequencing,
    dependencies, or interface commitments — not primarily to recipe-like
    implementation instructions.

Output: write the TOML to {tasks} in EXACTLY this shape (double-quoted strings;
triple-quoted for multi-line; arrays of inline tables for refs):

    [[tasks]]
    id = 1
    title = "Scaffold the worker pool"
    description = """
    Wire up a Tokio worker pool in src/pool.rs. …
    """
    test = """
    Run `cargo test pool::` — the new tests must pass.
    """
    estimated_tokens = 90000
    spec_refs = [
      {{ path = "artifacts/spec.md", lines = "10-45" }},
    ]
    plan_refs = [
      {{ path = "artifacts/plan.md", lines = "22-60" }},
      {{ path = "artifacts/plan.md", lines = "110-125" }},
    ]

    [[tasks]]
    id = 2
    …

The file is validated programmatically — missing or empty fields cause
rejection. Do NOT emit any prose around the TOML.
{instr}"#,
        spec = spec_path.display(),
        plan = plan_path.display(),
        tasks = tasks_path.display(),
        instr = instr,
    )
}

#[allow(clippy::too_many_arguments)]
fn recovery_prompt(
    spec_path: &std::path::Path,
    plan_path: &std::path::Path,
    tasks_path: &std::path::Path,
    trigger_task_id: Option<u32>,
    trigger_summary: Option<&str>,
    completed_task_ids: &[u32],
    started_task_ids: &[u32],
    live_summary_path: &std::path::Path,
    recovery_path: &std::path::Path,
    interactive: bool,
) -> String {
    let instr = live_summary_instruction(live_summary_path);
    let trigger_task = trigger_task_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "(none)".to_string());
    let trigger_summary = trigger_summary.unwrap_or("(none recorded)");
    let completed = if completed_task_ids.is_empty() {
        "(none)".to_string()
    } else {
        completed_task_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let started = if started_task_ids.is_empty() {
        "(none)".to_string()
    } else {
        started_task_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if interactive {
        format!(
            r#"{PROJECT_DOC_INSTR}You are the builder recovery agent. INTERACTIVE — the operator is present.

Human judgment is required to resolve this recovery. You MUST discuss the proposed
changes with the operator and get explicit confirmation before updating spec or plan.

Your job is to repair builder artifacts so orchestration can reconcile and resume.
You may edit ONLY:
  - {spec}
  - {plan}
  - {tasks}
  - {recovery}

Context from orchestrator:
  - Triggering task id: {trigger_task}
  - Trigger summary / latest reviewer feedback:
{trigger_summary}
  - Completed task ids (must stay completed): {completed}
  - Started task ids from run history: {started}

Hard requirements:
  - Read the triggering review first and identify the human decision needed.
  - Present the proposed correction to the operator and wait for confirmation.
  - Do NOT update spec or plan until the operator confirms the direction.
  - Keep `tasks.toml` valid and include unfinished work only.
  - Do NOT include completed ids in recovered `tasks.toml`.
  - If you supersede/remove started-but-unfinished task ids, add a `Recovery Notes`
    section in BOTH spec and plan, naming each superseded id and reason.
  - Write `{recovery}` with `status`, `summary`, and `feedback` TOML fields
    describing the confirmed recovery decision.
  - Do NOT modify source code or version control.
{instr}"#,
            spec = spec_path.display(),
            plan = plan_path.display(),
            tasks = tasks_path.display(),
            recovery = recovery_path.display(),
            trigger_task = trigger_task,
            trigger_summary = trigger_summary,
            completed = completed,
            started = started,
            instr = instr,
        )
    } else {
        format!(
            r#"{PROJECT_DOC_INSTR}You are the builder recovery agent. NON-INTERACTIVE — no operator questions.

Your job is to repair builder artifacts so orchestration can reconcile and resume.
You may edit ONLY:
  - {spec}
  - {plan}
  - {tasks}
  - {recovery}

Context from orchestrator:
  - Triggering task id: {trigger_task}
  - Trigger summary / latest reviewer feedback:
{trigger_summary}
  - Completed task ids (must stay completed): {completed}
  - Started task ids from run history: {started}

Hard requirements:
  - Keep `tasks.toml` valid and include unfinished work only.
  - Do NOT include completed ids in recovered `tasks.toml`.
  - If you supersede/remove started-but-unfinished task ids, add a `Recovery Notes`
    section in BOTH spec and plan, naming each superseded id and reason.
  - Keep changes minimal and deterministic.
  - Write `{recovery}` with `status`, `summary`, and `feedback` TOML fields
    describing the recovery decision.
  - Do NOT modify source code or version control.
{instr}"#,
            spec = spec_path.display(),
            plan = plan_path.display(),
            tasks = tasks_path.display(),
            recovery = recovery_path.display(),
            trigger_task = trigger_task,
            trigger_summary = trigger_summary,
            completed = completed,
            started = started,
            instr = instr,
        )
    }
}

fn recovery_plan_review_prompt(
    spec_path: &std::path::Path,
    plan_path: &std::path::Path,
    triggering_review_path: &std::path::Path,
    recovery_path: &std::path::Path,
    live_summary_path: &std::path::Path,
    plan_review_output_path: &std::path::Path,
) -> String {
    format!(
        r#"You are a non-interactive recovery plan reviewer. A recovery stage has just completed.

Inputs:
  - Spec: {spec}
  - Plan: {plan}
  - Triggering review: {review}
  - Recovery artifact: {recovery}
  - Live summary: {live_summary}

Your job:
  1. Verify the recovered spec/plan directly addresses the triggering review.
  2. Verify the plan is coherent enough for sharding.
  3. Do NOT reopen broad product/design debate.
  4. Make minimal fixes to spec.md or plan.md only for critical issues.

Write `{output}` with fields:
  - `status`: one of "approved", "revise", "human_blocked", "agent_pivot"
  - `summary`: one-line verdict
  - `feedback`: array of strings (required unless approved with no issues)

If approved, pipeline continues to sharding.
If revise/human_blocked/agent_pivot, recovery re-runs with your feedback."#,
        spec = spec_path.display(),
        plan = plan_path.display(),
        review = triggering_review_path.display(),
        recovery = recovery_path.display(),
        live_summary = live_summary_path.display(),
        output = plan_review_output_path.display(),
    )
}

fn recovery_sharding_prompt(
    spec_path: &std::path::Path,
    plan_path: &std::path::Path,
    live_summary_path: &std::path::Path,
    tasks_output_path: &std::path::Path,
    completed_ids: &[u32],
) -> String {
    let completed_str = if completed_ids.is_empty() {
        "none".to_string()
    } else {
        completed_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        r#"You are a non-interactive recovery sharding agent.

A recovery cycle has completed and the recovered spec/plan has been approved.
Now regenerate the task list.

Inputs:
  - Spec: {spec}
  - Plan: {plan}
  - Live summary: {live_summary}

Completed task ids (DO NOT include these in output): {completed}

Rules:
  - Regenerate active unfinished tasks from the recovered spec/plan.
  - Assign new task ids that are strictly greater than all completed ids.
  - Do NOT add completed task ids to pending work.
  - Keep tasks atomic and independently reviewable.

Write `{output}` as a valid tasks.toml file."#,
        spec = spec_path.display(),
        plan = plan_path.display(),
        live_summary = live_summary_path.display(),
        completed = completed_str,
        output = tasks_output_path.display(),
    )
}

/// Prepended to spec/plan files when they're auto-opened for review, then
/// stripped (by exact match) once the editor closes. Keep the literal stable
/// — `strip_review_banner` removes only this exact string, so any drift
/// would leave the banner sitting in the file forever.
const REVIEW_BANNER: &str = "\
████████████████████████████████████████████████████████████████████████
██                                                                    ██
██   PLEASE REVIEW THIS DOCUMENT, THEN CLOSE THE EDITOR TO CONTINUE.  ██
██                                                                    ██
██   This banner is auto-inserted on open and removed on close —      ██
██   leave it in place; it will not appear in the saved artifact.     ██
██                                                                    ██
████████████████████████████████████████████████████████████████████████

";

fn prepend_review_banner(path: &std::path::Path) -> bool {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return false;
    };
    if existing.contains(REVIEW_BANNER) {
        return false;
    }
    let mut combined = String::with_capacity(REVIEW_BANNER.len() + existing.len());
    combined.push_str(REVIEW_BANNER);
    combined.push_str(&existing);
    std::fs::write(path, combined).is_ok()
}

fn strip_review_banner(path: &std::path::Path) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path)?;
    let Some(idx) = existing.find(REVIEW_BANNER) else {
        return Ok(());
    };
    let mut stripped = String::with_capacity(existing.len() - REVIEW_BANNER.len());
    stripped.push_str(&existing[..idx]);
    stripped.push_str(&existing[idx + REVIEW_BANNER.len()..]);
    std::fs::write(path, stripped)
}

fn git_rev_parse_head() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

fn coder_prompt(
    session_dir: &std::path::Path,
    task_id: u32,
    round: u32,
    task_file: &std::path::Path,
    live_summary_path: &std::path::Path,
    resume: bool,
) -> String {
    let spec = session_dir.join("artifacts/spec.md");
    let plan = session_dir.join("artifacts/plan.md");
    let prev_review = if round > 1 {
        let p = session_dir
            .join("rounds")
            .join(format!("{:03}", round - 1))
            .join("review.toml");
        if p.exists() {
            format!(
                "\nPrevious reviewer feedback (round {}): {}\nRead it first and address every feedback item.\n",
                round - 1,
                p.display()
            )
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    let resume_hint = if resume {
        "\nThis is a RESUME of a previous coding session on the same task — pick up where\nyou left off, honour the reviewer feedback above, and finish the work.\n"
    } else {
        ""
    };
    let instr = live_summary_instruction(live_summary_path);
    format!(
        r#"{PROJECT_DOC_INSTR}You are the coder for task {task_id}, round {round}. NON-INTERACTIVE — the
operator is NOT available. Make your own judgement calls, document them in the
commit message, and leave a line comment for the reviewer on anything genuinely
ambiguous.

Inputs:
  Task:  {task}   (lists what to do, test steps, and line refs into spec/plan)
  Spec:  {spec}
  Plan:  {plan}
{prev_review}{resume_hint}
Job:
  1. Read the task file first.
  2. Implement end-to-end on the current branch.
  3. Make the tests described in the task pass — UNLESS the task's `test`
     field starts with "not testable" (genuine scaffolding/intermediate
     task). In that case you may skip writing tests, but the code you land
     MUST still build cleanly (compiles / links / type-checks) on its own.
     Lint is faster than full tests: run lint first and fix any warnings
     before the final full test run.
  4. Commit as a series of small atomic commits (see below). The reviewer
     inspects the aggregate `base..HEAD` range for this round, where `base`
     was pinned by the orchestrator before you started; the TUI detects
     completion by observing HEAD advanced past base.

Commit granularity (MANDATORY):
  - Prefer many small atomic commits over one large one. Each commit = ONE
    logical change that stands on its own (a single refactor step, a new
    function + its test, a single bug fix, a single rename). Any commit read
    in isolation should reveal its intent.
  - Every commit MUST build on its own (compiles / links / type-checks at
    that SHA). Never split in a way that leaves an intermediate commit broken.
  - Do NOT mix unrelated changes in one commit (e.g. rename + bug fix + new
    feature). Do NOT bundle formatting/whitespace churn into a functional
    commit — make it a separate `style:`/`chore:` commit if at all.
  - If a commit's real-logic diff (excluding generated files, lockfiles, large
    fixtures) exceeds ~200 lines, consider splitting.
  - One-task-one-commit is acceptable ONLY when the task genuinely is one
    atomic change. Otherwise split.

Commit message (MANDATORY — reviewer rejects violations):
  - Conventional Commits: `type(scope): summary` (feat, fix, refactor, test,
    docs, chore, perf, style, build). E.g. `feat(auth): add refresh-token
    rotation`, `fix(db): close pool on shutdown`.
  - No `Co-Authored-By:` trailers or other co-author attribution.
  - No orchestrator vocabulary: no "task <N>", "round <N>", "plan", "shard",
    "phase", or references to this prompt. Write as if a human engineer
    authored the change standalone.

Delegate tedious chores to subagents — bulk renames, codebase audits, test
sweeps, dependency tracing, large refactors. They run in parallel. Give each
a clear, self-contained brief and verify their output before committing.

Hard rules:
  - Do NOT ask clarifying questions; work from task + spec + plan.
  - Stay within this one task's scope. Follow-up work you uncover → note for
    the reviewer; do NOT do it yourself.
  - Do NOT force-push, rebase history, or delete branches.
  - Do NOT proceed to the next task; one task per round.
{instr}"#,
        task_id = task_id,
        round = round,
        task = task_file.display(),
        spec = spec.display(),
        plan = plan.display(),
        prev_review = prev_review,
        resume_hint = resume_hint,
        instr = instr,
    )
}

fn reviewer_prompt(
    session_dir: &std::path::Path,
    task_id: u32,
    round: u32,
    task_file: &std::path::Path,
    review_scope_file: &std::path::Path,
    review_file: &std::path::Path,
    live_summary_path: &std::path::Path,
) -> String {
    let spec = session_dir.join("artifacts/spec.md");
    let plan = session_dir.join("artifacts/plan.md");
    let instr = live_summary_instruction(live_summary_path);
    let prior_reviews = if round > 1 {
        let lines: Vec<String> = (1..round)
            .map(|r| {
                let p = session_dir
                    .join("rounds")
                    .join(format!("{r:03}"))
                    .join("review.toml");
                format!("    {}", p.display())
            })
            .collect();
        format!(
            "  Prior reviews for this task (read first; do not repeat their feedback):\n{}\n",
            lines.join("\n")
        )
    } else {
        String::new()
    };
    format!(
        r#"{PROJECT_DOC_INSTR}You are the reviewer for task {task_id}, round {round}. NON-INTERACTIVE — no
operator. Do NOT modify code. Write ONLY the review TOML.

Inputs:
  Task:         {task}
  Spec:         {spec}
  Plan:         {plan}
  Review scope: {review_scope} (TOML with base_sha = HEAD at round start)
{prior_reviews}

Review:
  1. BASE=$(sed -n 's/^base_sha = "\(.*\)"$/\1/p' {review_scope})
     `git log --oneline $BASE..HEAD` — every commit in this round.
     `git diff $BASE..HEAD`           — aggregate change.
     `git show <sha>`                 — drill into any commit.
     The coder may have made one or more commits; judge the aggregate delta
     against the task. Per-commit structure is the coder's choice.
  2. Judge task completion: does the aggregate delta actually deliver what's
     required? Read the task `description` AND the spec/plan sections it
     points to (via `spec_refs` and `plan_refs` in the task file) — the task
     is complete only when the delta satisfies all of them. A green test run
     does NOT by itself prove completion, and a missing test run does NOT by
     itself prove failure — read the code against those requirements.
  3. Verify the task's test description passes (run it, inspect code). If the
     task's `test` field starts with "not testable" (scaffolding/intermediate
     task), SKIP the test-pass check — but still require the code to build
     cleanly (compiles / links / type-checks). Completion still matters.
  4. Check correctness, missing edge cases, broken contracts, bad error
     handling, test gaps. Uncommitted working-tree changes are NOT in scope —
     review only `base..HEAD`.

Emit the verdict to {review} in EXACTLY this TOML shape (double-quoted strings;
triple-quoted for multi-line; arrays of inline tables for any new task refs):

    status  = "approved" | "revise" | "human_blocked" | "agent_pivot"
    summary = "One-paragraph summary of what was done and your verdict."
    feedback = [
      "Specific thing to fix, if status is revise/human_blocked/agent_pivot.",
      "One item per string.",
    ]

    # Optional: follow-up tasks for work genuinely out-of-scope for this task
    # but needed later.
    [[new_tasks]]
    id = 100
    title = "…"
    description = """…"""
    test = """…"""
    estimated_tokens = 150000
    spec_refs = [{{ path = "artifacts/spec.md", lines = "10-30" }}]
    plan_refs = [{{ path = "artifacts/plan.md", lines = "50-70" }}]

Rules:
  - approved      → outcomes delivered AND (tests pass OR task is "not testable" and
                    the code builds cleanly). Do not include new_tasks.
  - revise        → list the specific issues. For complex tasks, also suggest a
                    direction (file/approach/sketch) — do not just reject.
  - human_blocked → human judgement required; explain what's unclear.
  - agent_pivot   → autonomous recovery is required; explain the pivot.
  - Don't repeat feedback from prior reviews unless the coder ignored it
              without good reason — in which case call that out explicitly.
  - Don't leave feedback empty for revise/human_blocked/agent_pivot, and don't emit prose
              outside the TOML.
{instr}"#,
        task_id = task_id,
        round = round,
        task = task_file.display(),
        spec = spec.display(),
        plan = plan.display(),
        review_scope = review_scope_file.display(),
        review = review_file.display(),
        instr = instr,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{RunRecord, RunStatus};

    #[test]
    fn review_banner_round_trip_restores_original_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("spec.md");
        let original = "# Spec\n\nbody line one\nbody line two\n";
        std::fs::write(&path, original).unwrap();

        assert!(prepend_review_banner(&path));
        let with_banner = std::fs::read_to_string(&path).unwrap();
        assert!(with_banner.starts_with(REVIEW_BANNER));
        assert!(with_banner.ends_with(original));

        strip_review_banner(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn review_banner_strip_is_noop_when_banner_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("plan.md");
        // User edited the banner away (or it was never there): we must not
        // silently delete the first N lines.
        let edited = "# Plan\n\nactual content\n";
        std::fs::write(&path, edited).unwrap();
        strip_review_banner(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
    }

    #[test]
    fn review_banner_prepend_is_idempotent() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("spec.md");
        std::fs::write(&path, "# Spec\nbody\n").unwrap();
        assert!(prepend_review_banner(&path));
        // Second prepend on the same file must not stack a second banner.
        assert!(!prepend_review_banner(&path));
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.matches(REVIEW_BANNER).count(), 1);
    }

    fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
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

    fn mk_tmux() -> TmuxContext {
        TmuxContext {
            session_name: "test".to_string(),
            window_index: "0".to_string(),
            window_name: "test".to_string(),
        }
    }

    fn mk_state_with_runs() -> SessionState {
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
            hostname: None,
            mount_device_id: None,
        });
        state
    }

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

    fn mk_app(state: SessionState) -> App {
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
            live_summary_watcher: None,
            live_summary_change_rx: None,
            live_summary_path: None,
            live_summary_cached_text: String::new(),
            live_summary_cached_mtime: None,
            pending_drain_deadline: None,
            pending_drain_notice_emitted: false,
            current_run_id: Some(2),
            failed_models: HashMap::new(),
            test_launch_harness: None,
            messages: Vec::new(),
        };
        app.rebuild_visible_rows();
        app.restore_selection(app.selected_key.clone(), app.selected);
        app
    }

    fn make_coder_run(id: u64, round: u32, attempt: u32) -> RunRecord {
        RunRecord {
            id,
            stage: "coder".to_string(),
            task_id: Some(1),
            round,
            attempt,
            model: "gpt-5".to_string(),
            vendor: "codex".to_string(),
            window_name: format!("[Coder t1 r{round}]"),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            hostname: None,
            mount_device_id: None,
        }
    }

    fn make_planning_run(id: u64, attempt: u32) -> RunRecord {
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
            hostname: None,
            mount_device_id: None,
        }
    }

    fn write_review_scope(round_dir: &std::path::Path, base_sha: &str) {
        std::fs::create_dir_all(round_dir).expect("round dir");
        std::fs::write(
            round_dir.join("review_scope.toml"),
            format!("base_sha = \"{base_sha}\"\n"),
        )
        .expect("write review scope");
    }

    fn write_finish_stamp(
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
        };
        let stamp_path = session_dir
            .join("artifacts")
            .join("run-finish")
            .join(format!("{run_key}.toml"));
        crate::runner::write_finish_stamp(&stamp_path, &stamp).expect("write finish stamp");
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
        assert!(!app.is_expanded(bs_idx));
        app.toggle_expand_focused();
        assert!(app.is_expanded(bs_idx));
        assert_eq!(
            app.collapsed_overrides.get(&bs_key),
            Some(&ExpansionOverride::Expanded)
        );
        app.toggle_expand_focused();
        assert!(!app.is_expanded(bs_idx));
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
            window_name: "[Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            hostname: None,
            mount_device_id: None,
        });
        let mut app = mk_app(state);
        let task_idx = row_index(&app, "Task 7");
        let coder_idx = row_index(&app, "Coder");
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
        assert!(row_index_opt(&app, "Coder").is_none());
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
            window_name: "[Coder]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            hostname: None,
            mount_device_id: None,
        });
        let mut app = mk_app(state);
        let coder_idx = row_index(&app, "Coder");
        let coder_key = app.visible_rows[coder_idx].key.clone();
        app.selected = coder_idx;

        app.toggle_expand_focused();

        assert_eq!(
            app.collapsed_overrides.get(&coder_key),
            Some(&ExpansionOverride::Collapsed)
        );
        let coder_idx = row_index(&app, "Coder");
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
        assert!(!app.is_expanded(brainstorm_idx));
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
            window_name: "[Coder 7]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
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
            window_name: "[Coder 8]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            hostname: None,
            mount_device_id: None,
        });
        let mut app = mk_app(state);
        let task_idx = row_index(&app, "Task 7");
        app.selected = task_idx;

        app.toggle_expand_focused();

        assert_eq!(row_label(&app, app.selected), "Task 7");
        assert!(row_index_opt(&app, "Coder").is_some());
    }

    #[test]
    fn review_round_row_can_be_expanded_for_multiround_transcript_access() {
        let mut state = SessionState::new("review-drilldown".to_string());
        state.current_phase = Phase::SpecReviewRunning;
        state.agent_runs.push(RunRecord {
            id: 31,
            stage: "spec-review".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "openai".to_string(),
            window_name: "[Spec Review 1]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Done,
            error: None,
            hostname: None,
            mount_device_id: None,
        });
        state.agent_runs.push(RunRecord {
            id: 32,
            stage: "spec-review".to_string(),
            task_id: None,
            round: 2,
            attempt: 1,
            model: "gpt-5".to_string(),
            vendor: "openai".to_string(),
            window_name: "[Spec Review 2]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            hostname: None,
            mount_device_id: None,
        });
        let mut app = mk_app(state);
        let round_one_idx = row_index(&app, "Round 1");
        app.selected = round_one_idx;

        app.toggle_expand_focused();

        let round_one_idx = row_index(&app, "Round 1");
        assert!(app.is_expanded_body(round_one_idx));
        assert_eq!(
            app.visible_rows[round_one_idx].backing_leaf_run_id,
            Some(31)
        );
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
                hostname: None,
                mount_device_id: None,
            });
        }
        let mut app = mk_app(state);
        let coder_idx = row_index(&app, "Coder");
        app.selected = coder_idx;
        app.toggle_expand_focused();
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
            Some(&ExpansionOverride::Expanded)
        );
        assert!(!app.collapsed_overrides.contains_key(&attempt_rows[1].1));
    }

    fn row_index(app: &App, label: &str) -> usize {
        row_index_opt(app, label).expect("row")
    }

    fn row_index_opt(app: &App, label: &str) -> Option<usize> {
        app.visible_rows.iter().position(|row| {
            node_at_path(&app.nodes, &row.path).is_some_and(|node| node.label == label)
        })
    }

    fn row_label(app: &App, index: usize) -> String {
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
    fn rank_to_axis_score_inner(rank: u8) -> f64 {
        match rank {
            1 => 1.0,
            2 => 0.6,
            3 => 0.4,
            _ => 0.3,
        }
    }

    fn sample_model(name: &str, idea_rank: u8, build_rank: u8) -> selection::CachedModel {
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
            quota_percent: Some(80),
            display_order: 0,
            fallback_from: None,
        }
    }

    fn ranked_model(
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
            quota_percent: Some(80),
            display_order: 0,
            fallback_from: None,
        }
    }

    fn idle_app(state: SessionState) -> App {
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
            live_summary_watcher: None,
            live_summary_change_rx: None,
            live_summary_path: None,
            live_summary_cached_text: String::new(),
            live_summary_cached_mtime: None,
            pending_drain_deadline: None,
            pending_drain_notice_emitted: false,
            current_run_id: None,
            failed_models: HashMap::new(),
            test_launch_harness: None,
            messages: Vec::new(),
        };
        app.rebuild_visible_rows();
        app.restore_selection(app.selected_key.clone(), app.selected);
        app
    }

    #[test]
    fn brainstorm_selection_uses_idea_task_kind() {
        let models = vec![
            sample_model("idea-first", 1, 2),
            sample_model("build-first", 2, 1),
        ];

        let versions = build_version_index(&models);
        let chosen =
            App::select_brainstorm_model(&models, &versions).expect("expected brainstorm model");

        assert_eq!(chosen.name, "idea-first");
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
                window_name: "[Coder r3]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Failed,
                error: Some("exit(1)".to_string()),
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
                window_name: "[Coder r3]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Failed,
                error: Some("artifact_missing".to_string()),
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
                window_name: "[Coder r3]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Failed,
                error: Some("user_forced_retry".to_string()),
                hostname: None,
                mount_device_id: None,
            });
            state.save().expect("save session");

            let app = App::new(
                mk_tmux(),
                SessionState::load(session_id).expect("load session"),
            );

            let key = ("coder".to_string(), Some(7), 3);
            let failed = app
                .failed_models
                .get(&key)
                .expect("expected failed model set");
            assert!(failed.contains(&(selection::VendorKind::Claude, "claude-sonnet".to_string())));
            assert!(
                failed.contains(&(selection::VendorKind::Gemini, "gemini-2.5-pro".to_string()))
            );
            assert!(!failed.contains(&(selection::VendorKind::Codex, "gpt-5".to_string())));
            assert!(app.current_run_id.is_none());
        });
    }

    #[test]
    fn normalize_failure_reason_reports_exit_signal_and_artifact_errors() {
        with_temp_root(|| {
            let session_id = "normalize-failure-reason";
            let session_dir = session_state::session_dir(session_id);
            std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
            let state = SessionState::new(session_id.to_string());
            let mut app = mk_app(state);
            let run = RunRecord {
                id: 9,
                stage: "planning".to_string(),
                task_id: None,
                round: 1,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Planning]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
                hostname: None,
                mount_device_id: None,
            };
            std::fs::create_dir_all(app.run_status_path(&run).parent().expect("status dir"))
                .expect("create status dir");

            std::fs::write(app.run_status_path(&run), "1").expect("write exit code");
            assert_eq!(
                app.normalized_failure_reason(&run).expect("exit reason"),
                Some("exit(1)".to_string())
            );

            std::fs::write(app.run_status_path(&run), "143").expect("write signal exit");
            assert_eq!(
                app.normalized_failure_reason(&run).expect("signal reason"),
                Some("killed(15)".to_string())
            );

            std::fs::write(app.run_status_path(&run), "0").expect("write clean exit");
            assert_eq!(
                app.normalized_failure_reason(&run)
                    .expect("missing artifact"),
                Some("artifact_missing".to_string())
            );

            std::fs::write(session_dir.join("artifacts").join("plan.md"), "")
                .expect("write empty plan");
            assert_eq!(
                app.normalized_failure_reason(&run).expect("empty artifact"),
                Some("artifact_missing".to_string())
            );

            let brainstorm = RunRecord {
                stage: "brainstorm".to_string(),
                window_name: "[Brainstorm]".to_string(),
                ..run.clone()
            };
            std::fs::write(app.run_status_path(&brainstorm), "0").expect("clean brainstorm exit");
            std::fs::write(session_dir.join("artifacts").join("spec.md"), "")
                .expect("write empty spec");
            assert_eq!(
                app.normalized_failure_reason(&brainstorm)
                    .expect("empty spec"),
                Some("artifact_missing".to_string())
            );

            let sharding = RunRecord {
                stage: "sharding".to_string(),
                window_name: "[Sharding]".to_string(),
                ..run.clone()
            };
            std::fs::write(app.run_status_path(&sharding), "0").expect("clean sharding exit");
            std::fs::write(
                session_dir.join("artifacts").join("tasks.toml"),
                "not valid toml = [",
            )
            .expect("write invalid tasks");
            assert!(
                app.normalized_failure_reason(&sharding)
                    .expect("invalid tasks")
                    .expect("error text")
                    .starts_with("artifact_invalid: ")
            );
        });
    }

    #[test]
    fn normalize_failure_reason_artifact_present_still_fails_on_head_advance() {
        with_temp_root(|| {
            let session_id = "normalize-failure-reason-guard";
            let session_dir = session_state::session_dir(session_id);
            std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
            let state = SessionState::new(session_id.to_string());
            let mut app = mk_app(state);
            let run = RunRecord {
                id: 1,
                stage: "planning".to_string(),
                task_id: None,
                round: 1,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Planning]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
                hostname: None,
                mount_device_id: None,
            };
            std::fs::create_dir_all(app.run_status_path(&run).parent().expect("status dir"))
                .expect("create status dir");
            // Valid plan artifact so artifact_reason is None.
            std::fs::write(session_dir.join("artifacts").join("plan.md"), "# Plan\n")
                .expect("write plan");
            std::fs::write(app.run_status_path(&run), "0").expect("write exit code");

            // Write a guard snapshot whose HEAD differs from real HEAD so
            // verify_non_coder will return forbidden_head_advance.
            let guard_dir = session_dir.join(".guards").join("planning-stage-r1-a1");
            std::fs::create_dir_all(&guard_dir).expect("guard dir");
            std::fs::write(
                guard_dir.join("snapshot.toml"),
                "head = \"0000000000000000000000000000000000000000\"\ngit_status = \"\"\n\n[control_files]\n",
            )
            .expect("write snapshot");

            let reason = app
                .normalized_failure_reason(&run)
                .expect("normalized")
                .expect("hard error expected");
            assert_eq!(reason, "forbidden_head_advance");
        });
    }

    #[test]
    fn coder_retry_loop_uses_distinct_models_until_success() {
        with_temp_root(|| {
            let session_id = "coder-retry-loop";
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ImplementationRound(1);
            state.builder.current_task = Some(1);
            let mut app = idle_app(state);
            app.models = vec![
                ranked_model(selection::VendorKind::Claude, "claude-sonnet", 10, 1, 10),
                ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 10, 2, 10),
                ranked_model(selection::VendorKind::Codex, "gpt-5", 10, 3, 10),
            ];
            let harness = std::sync::Arc::new(std::sync::Mutex::new(TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![
                    TestLaunchOutcome {
                        exit_code: 1,
                        artifact_contents: None,
                    },
                    TestLaunchOutcome {
                        exit_code: 1,
                        artifact_contents: None,
                    },
                    TestLaunchOutcome {
                        exit_code: 0,
                        artifact_contents: Some("abc123".to_string()),
                    },
                ]),
            }));
            app.test_launch_harness = Some(harness);

            app.launch_coder();
            for _ in 0..6 {
                if app.current_run_id.is_none() {
                    break;
                }
                app.poll_agent_window();
            }

            assert!(app.current_run_id.is_none());
            assert_eq!(app.state.agent_runs.len(), 3);
            assert_eq!(app.state.agent_runs[0].attempt, 1);
            assert_eq!(app.state.agent_runs[1].attempt, 2);
            assert_eq!(app.state.agent_runs[2].attempt, 3);
            assert_eq!(app.state.agent_runs[0].status, RunStatus::Failed);
            assert_eq!(app.state.agent_runs[1].status, RunStatus::Failed);
            assert_eq!(app.state.agent_runs[2].status, RunStatus::Done);
            assert_eq!(app.state.agent_runs[0].error.as_deref(), Some("exit(1)"));
            assert_eq!(app.state.agent_runs[1].error.as_deref(), Some("exit(1)"));
            assert_eq!(app.state.agent_runs[0].model, "claude-sonnet");
            assert_eq!(app.state.agent_runs[1].model, "gemini-2.5-pro");
            assert_eq!(app.state.agent_runs[2].model, "gpt-5");
            assert_eq!(app.state.current_phase, Phase::ReviewRound(1));

            let end_texts = app
                .messages
                .iter()
                .filter(|message| message.kind == MessageKind::End)
                .map(|message| message.text.clone())
                .collect::<Vec<_>>();
            assert!(end_texts.contains(&"attempt 1 failed: exit(1)".to_string()));
            assert!(end_texts.contains(&"attempt 2 failed: exit(1)".to_string()));

            let started_texts = app
                .messages
                .iter()
                .filter(|message| message.kind == MessageKind::Started)
                .map(|message| message.text.clone())
                .collect::<Vec<_>>();
            assert!(started_texts.contains(&"retrying with gemini/gemini-2.5-pro".to_string()));
            assert!(started_texts.contains(&"retrying with codex/gpt-5".to_string()));
        });
    }

    #[test]
    fn coder_finalize_succeeds_from_stable_advancing_finish_stamp() {
        with_temp_root(|| {
            let session_id = "coder-stable-advance";
            let session_dir = session_state::session_dir(session_id);
            let round_dir = session_dir.join("rounds").join("001");
            write_review_scope(&round_dir, "base123");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ImplementationRound(1);
            state.builder.current_task = Some(1);
            let run = make_coder_run(1, 1, 1);
            state.agent_runs.push(run.clone());
            let mut app = idle_app(state);

            write_finish_stamp(
                &session_dir,
                &App::run_key_for("coder", Some(1), 1, 1),
                "head456",
                "stable",
            );

            app.finalize_current_run(&run).expect("finalize coder");

            let finalized = app
                .state
                .agent_runs
                .iter()
                .find(|r| r.id == 1)
                .expect("run");
            assert_eq!(finalized.status, RunStatus::Done);
            assert_eq!(finalized.error, None);
            assert_eq!(app.state.current_phase, Phase::ReviewRound(1));
        });
    }

    #[test]
    fn coder_gate_reports_authoritative_failure_when_stamp_head_matches_base() {
        with_temp_root(|| {
            let session_id = "coder-stable-unchanged";
            let session_dir = session_state::session_dir(session_id);
            let round_dir = session_dir.join("rounds").join("001");
            write_review_scope(&round_dir, "base123");
            write_finish_stamp(
                &session_dir,
                &App::run_key_for("coder", Some(1), 1, 1),
                "base123",
                "stable",
            );

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ImplementationRound(1);
            state.builder.current_task = Some(1);
            let run = make_coder_run(1, 1, 1);
            state.agent_runs.push(run.clone());
            let mut app = idle_app(state);

            let reason = app
                .normalized_failure_reason(&run)
                .expect("normalized failure reason");
            assert_eq!(reason.as_deref(), Some("no_commits_since_round_start"));
        });
    }

    #[test]
    fn coder_gate_fails_unverified_when_finish_stamp_missing_or_unstable() {
        with_temp_root(|| {
            let session_id = "coder-missing-stamp";
            let session_dir = session_state::session_dir(session_id);
            let round_dir = session_dir.join("rounds").join("001");
            write_review_scope(&round_dir, "base123");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ImplementationRound(1);
            state.builder.current_task = Some(1);
            let run = make_coder_run(1, 1, 1);
            state.agent_runs.push(run.clone());
            let mut app = idle_app(state);

            let missing_reason = app
                .normalized_failure_reason(&run)
                .expect("missing normalized failure reason");
            let missing = missing_reason.expect("missing stamp should fail");
            assert!(missing.starts_with("failed_unverified"));
            assert!(missing.contains("missing finish stamp"));

            write_finish_stamp(
                &session_dir,
                &App::run_key_for("coder", Some(1), 1, 1),
                "head456",
                "unstable",
            );
            let unstable_reason = app
                .normalized_failure_reason(&run)
                .expect("unstable normalized failure reason");
            let unstable = unstable_reason.expect("unstable stamp should fail");
            assert!(unstable.starts_with("failed_unverified"));
            assert!(unstable.contains("head_state=unstable"));
        });
    }

    #[test]
    fn coder_gate_fails_unverified_when_finish_stamp_is_malformed() {
        with_temp_root(|| {
            let session_id = "coder-malformed-stamp";
            let session_dir = session_state::session_dir(session_id);
            let round_dir = session_dir.join("rounds").join("001");
            write_review_scope(&round_dir, "base123");

            let run_key = App::run_key_for("coder", Some(1), 1, 1);
            let stamp_path = session_dir
                .join("artifacts")
                .join("run-finish")
                .join(format!("{run_key}.toml"));
            std::fs::create_dir_all(stamp_path.parent().expect("stamp dir")).expect("stamp dir");
            std::fs::write(&stamp_path, "not = [valid").expect("write malformed stamp");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ImplementationRound(1);
            state.builder.current_task = Some(1);
            let run = make_coder_run(1, 1, 1);
            state.agent_runs.push(run.clone());
            let mut app = idle_app(state);

            let reason = app
                .normalized_failure_reason(&run)
                .expect("normalized failure reason");
            let reason = reason.expect("malformed stamp should fail");
            assert!(reason.starts_with("failed_unverified"));
            assert!(reason.contains("malformed finish stamp"));
        });
    }

    #[test]
    fn coder_finalize_marks_missing_stamp_as_failed_unverified_with_hint() {
        with_temp_root(|| {
            let session_id = "coder-finalize-missing-stamp";
            let session_dir = session_state::session_dir(session_id);
            let round_dir = session_dir.join("rounds").join("001");
            write_review_scope(&round_dir, "base123");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ImplementationRound(1);
            state.builder.current_task = Some(1);
            let run = make_coder_run(1, 1, 1);
            state.agent_runs.push(run.clone());
            let mut app = idle_app(state);

            app.finalize_current_run(&run).expect("finalize coder");

            let finalized = app
                .state
                .agent_runs
                .iter()
                .find(|r| r.id == 1)
                .expect("run");
            assert_eq!(finalized.status, RunStatus::FailedUnverified);
            assert!(
                finalized
                    .error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("run-finish")
            );
            let end = app
                .messages
                .iter()
                .find(|message| message.run_id == 1 && message.kind == MessageKind::End)
                .expect("end message");
            assert!(end.text.contains("attempt 1 unverified"));
            assert!(end.text.contains("missing finish stamp"));
        });
    }

    #[test]
    fn window_disappearance_enters_drain_state_before_finalize() {
        with_temp_root(|| {
            let session_id = "planning-drain-before-finalize";
            let session_dir = session_state::session_dir(session_id);
            let artifacts_dir = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");
            std::fs::write(artifacts_dir.join("plan.md"), "# Plan\n").expect("plan artifact");

            let run = make_planning_run(1, 1);
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::PlanningRunning;
            state.agent_runs.push(run.clone());
            let mut app = idle_app(state);
            app.current_run_id = Some(run.id);
            app.window_launched = true;
            app.models = vec![ranked_model(
                selection::VendorKind::Codex,
                "gpt-5",
                1,
                10,
                10,
            )];

            let status_path = app.run_status_path(&run);
            std::fs::create_dir_all(status_path.parent().expect("status dir")).expect("status dir");
            std::fs::write(&status_path, "0").expect("status");

            let run_key = App::run_key_for("planning", None, 1, 1);
            write_finish_stamp(&session_dir, &run_key, "head123", "stable");

            let live_summary_path = app.live_summary_path_for(&run);
            std::fs::write(&live_summary_path, "still draining\n").expect("live summary");

            app.poll_agent_window();

            let persisted = app
                .state
                .agent_runs
                .iter()
                .find(|candidate| candidate.id == run.id)
                .expect("run");
            assert_eq!(persisted.status, RunStatus::Running);
            assert_eq!(app.current_run_id, Some(run.id));
            assert!(app.pending_drain_deadline.is_some());
            assert!(app.messages.iter().any(|m| m.run_id == run.id
                && m.kind == MessageKind::Summary
                && m.text.contains("draining")));
        });
    }

    #[test]
    fn same_key_retry_waits_for_stamp_or_timeout_after_live_summary_absent() {
        with_temp_root(|| {
            let session_id = "planning-drain-timeout";
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
                        },
                        TestLaunchOutcome {
                            exit_code: 1,
                            artifact_contents: None,
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
            let stamp_path = app.finish_stamp_path_for(&first);
            let _ = std::fs::remove_file(&stamp_path);
            let _ = std::fs::remove_file(app.live_summary_path_for(&first));

            app.poll_agent_window();
            assert_eq!(app.current_run_id, Some(first.id));
            let still_first = app
                .state
                .agent_runs
                .iter()
                .find(|run| run.id == first.id)
                .expect("first run after barrier");
            assert_eq!(still_first.status, RunStatus::Running);

            app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));
            app.poll_agent_window();

            let first_done = app
                .state
                .agent_runs
                .iter()
                .find(|run| run.id == first.id)
                .expect("first finalized");
            assert_eq!(first_done.status, RunStatus::Failed);
            let second = app
                .state
                .agent_runs
                .iter()
                .find(|run| run.stage == "planning" && run.attempt == 2)
                .expect("retry attempt 2 launched");
            assert_eq!(second.status, RunStatus::Running);
            assert_eq!(app.current_run_id, Some(second.id));
        });
    }

    #[test]
    fn failed_unverified_coder_does_not_auto_retry() {
        with_temp_root(|| {
            let session_id = "coder-unverified-no-retry";
            let session_dir = session_state::session_dir(session_id);
            let round_dir = session_dir.join("rounds").join("001");
            write_review_scope(&round_dir, "base123");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ImplementationRound(1);
            state.builder.current_task = Some(1);
            let run = make_coder_run(1, 1, 1);
            state.agent_runs.push(run.clone());
            let mut app = idle_app(state);
            app.models = vec![
                ranked_model(selection::VendorKind::Codex, "gpt-5", 1, 10, 10),
                ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 2, 10, 10),
            ];

            app.finalize_current_run(&run).expect("finalize coder");

            assert_eq!(app.state.agent_runs.len(), 1);
            let finalized = app
                .state
                .agent_runs
                .iter()
                .find(|candidate| candidate.id == run.id)
                .expect("finalized run");
            assert_eq!(finalized.status, RunStatus::FailedUnverified);
            assert!(
                app.state
                    .agent_error
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with("failed_unverified:"),
                "failed_unverified should block auto-retry and surface as agent_error"
            );
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
                        },
                        TestLaunchOutcome {
                            exit_code: 1,
                            artifact_contents: None,
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
            app.poll_agent_window();

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
    fn guard_warnings_emit_only_after_drain_barrier_passes() {
        with_temp_root(|| {
            let session_id = "guard-after-drain";
            let session_dir = session_state::session_dir(session_id);
            let artifacts_dir = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");
            std::fs::write(artifacts_dir.join("plan.md"), "# Plan\n").expect("plan artifact");

            let run = make_planning_run(1, 1);
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::PlanningRunning;
            state.agent_runs.push(run.clone());
            let mut app = idle_app(state);
            app.current_run_id = Some(run.id);
            app.window_launched = true;

            let status_path = app.run_status_path(&run);
            std::fs::create_dir_all(status_path.parent().expect("status dir")).expect("status dir");
            std::fs::write(&status_path, "0").expect("status");

            let run_key = App::run_key_for("planning", None, 1, 1);
            write_finish_stamp(&session_dir, &run_key, "head123", "stable");

            let guard_dir = session_dir.join(".guards").join("planning-stage-r1-a1");
            std::fs::create_dir_all(&guard_dir).expect("guard dir");
            std::fs::write(
                guard_dir.join("snapshot.toml"),
                "head = \"\"\ngit_status = \"dirty\"\nmode = \"auto_reset\"\n\n[control_files]\n",
            )
            .expect("guard snapshot");

            let live_summary_path = app.live_summary_path_for(&run);
            std::fs::write(&live_summary_path, "awaiting drain\n").expect("live summary");
            app.poll_agent_window();

            assert!(
                !app.messages.iter().any(|message| {
                    message.run_id == run.id
                        && message.kind == MessageKind::SummaryWarn
                        && message
                            .text
                            .contains("working tree was dirty before agent launch")
                }),
                "guard diagnostics should not emit before drain barrier releases finalize"
            );

            std::fs::remove_file(&live_summary_path).expect("remove live summary");
            app.poll_agent_window();

            assert!(
                app.messages.iter().any(|message| {
                    message.run_id == run.id
                        && message.kind == MessageKind::SummaryWarn
                        && message
                            .text
                            .contains("working tree was dirty before agent launch")
                }),
                "guard diagnostics should emit after barrier passes"
            );
        });
    }

    #[test]
    fn coder_retry_exhaustion_enters_builder_recovery() {
        with_temp_root(|| {
            let session_id = "coder-retry-exhaustion";
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ImplementationRound(1);
            state.builder.pending = vec![2, 3];
            state.builder.current_task = Some(1);
            let mut app = idle_app(state);
            app.models = vec![
                ranked_model(selection::VendorKind::Claude, "claude-sonnet", 10, 1, 10),
                ranked_model(selection::VendorKind::Gemini, "gemini-2.5-pro", 10, 2, 10),
            ];
            let harness = std::sync::Arc::new(std::sync::Mutex::new(TestLaunchHarness {
                outcomes: std::collections::VecDeque::from(vec![
                    TestLaunchOutcome {
                        exit_code: 1,
                        artifact_contents: None,
                    },
                    TestLaunchOutcome {
                        exit_code: 1,
                        artifact_contents: None,
                    },
                ]),
            }));
            app.test_launch_harness = Some(harness);

            app.launch_coder();
            for _ in 0..5 {
                if app.current_run_id.is_none() {
                    break;
                }
                app.poll_agent_window();
            }

            assert!(app.current_run_id.is_none());
            assert_eq!(app.state.current_phase, Phase::BuilderRecovery(1));
            assert_eq!(app.state.builder.current_task, None);
            assert_eq!(app.state.builder.pending, vec![2, 3]);
            let summary = app
                .state
                .builder
                .recovery_trigger_summary
                .clone()
                .expect("recovery trigger summary");
            assert!(summary.starts_with("retry exhausted (2 attempts)"));
            assert!(summary.contains("attempt 1: claude/claude-sonnet"));
            assert!(summary.contains("attempt 2: gemini/gemini-2.5-pro"));
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
    fn recovery_retry_exhaustion_falls_back_to_blocked() {
        let mut state = SessionState::new("recovery-retry-cap".to_string());
        state.current_phase = Phase::BuilderRecovery(2);
        let mut app = idle_app(state);
        app.models = vec![ranked_model(
            selection::VendorKind::Claude,
            "claude-sonnet",
            1,
            10,
            10,
        )];
        let failed = RunRecord {
            id: 21,
            stage: "recovery".to_string(),
            task_id: None,
            round: 2,
            attempt: 3,
            model: "claude-sonnet".to_string(),
            vendor: "claude".to_string(),
            window_name: "[Recovery]".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            status: RunStatus::Failed,
            error: Some("artifact_invalid: x".to_string()),
            hostname: None,
            mount_device_id: None,
        };
        let handled = app.maybe_auto_retry(&failed);
        assert!(handled);
        assert_eq!(app.state.current_phase, Phase::BlockedNeedsUser);
        assert!(
            app.state
                .agent_error
                .as_deref()
                .unwrap_or_default()
                .starts_with("builder recovery retry exhausted")
        );
    }

    #[test]
    fn review_human_blocked_enters_builder_recovery() {
        with_temp_root(|| {
            let session_id = "review-blocked-recovery";
            let session_dir = session_state::session_dir(session_id);
            std::fs::create_dir_all(session_dir.join("rounds").join("001")).expect("round dir");
            std::fs::write(
                session_dir.join("rounds").join("001").join("review.toml"),
                r#"status = "human_blocked"
summary = "needs recovery"
feedback = ["task 2 is superseded"]
"#,
            )
            .expect("review file");
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ReviewRound(1);
            state.builder.current_task = Some(2);
            state.agent_runs.push(RunRecord {
                id: 1,
                stage: "reviewer".to_string(),
                task_id: Some(2),
                round: 1,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Review]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
                hostname: None,
                mount_device_id: None,
            });
            let mut app = idle_app(state);
            let run = app.state.agent_runs[0].clone();
            app.finalize_current_run(&run).expect("finalize review");
            assert_eq!(app.state.current_phase, Phase::BuilderRecovery(1));
            assert_eq!(app.state.builder.current_task, None);
            assert_eq!(app.state.builder.recovery_trigger_task_id, Some(2));
        });
    }

    #[test]
    fn review_revise_with_new_tasks_rewrites_queue_and_advances_to_inserted_task() {
        with_temp_root(|| {
            let session_id = "review-revise-new-tasks";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            let round_dir = session_dir.join("rounds").join("001");
            std::fs::create_dir_all(&artifacts).expect("artifacts dir");
            std::fs::create_dir_all(&round_dir).expect("round dir");
            std::fs::write(
                artifacts.join("tasks.toml"),
                r#"[[tasks]]
id = 1
title = "Finished"
description = "done"
test = "cargo test"
estimated_tokens = 10

[[tasks]]
id = 2
title = "Too broad"
description = "split me"
test = "cargo test"
estimated_tokens = 20

[[tasks]]
id = 3
title = "Later"
description = "preserve this"
test = "cargo test runner::"
estimated_tokens = 30

[[tasks.spec_refs]]
path = "spec.md"
lines = "1-2"
"#,
            )
            .expect("tasks file");
            std::fs::write(
                round_dir.join("review.toml"),
                r#"status = "revise"
summary = "split required"
feedback = ["split into smaller work"]

[[new_tasks]]
id = 0
title = "Split A"
description = "first half"
test = "cargo test transitions::"
estimated_tokens = 11

[[new_tasks]]
id = 0
title = "Split B"
description = "second half"
test = "cargo test runner::"
estimated_tokens = 12
"#,
            )
            .expect("review file");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ReviewRound(1);
            state.builder.reset_task_pipeline(vec![
                (1, Some("Finished".to_string())),
                (2, Some("Too broad".to_string())),
                (3, Some("Later".to_string())),
            ]);
            let _ = state
                .builder
                .set_task_status(1, PipelineItemStatus::Approved, Some(1));
            let _ = state
                .builder
                .set_task_status(2, PipelineItemStatus::Running, Some(1));
            state.builder.current_task = Some(2);
            state.agent_runs.push(RunRecord {
                id: 1,
                stage: "reviewer".to_string(),
                task_id: Some(2),
                round: 1,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Review]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
                hostname: None,
                mount_device_id: None,
            });

            let mut app = idle_app(state);
            let run = app.state.agent_runs[0].clone();
            app.finalize_current_run(&run).expect("finalize review");

            assert_eq!(app.state.current_phase, Phase::ImplementationRound(2));
            assert_eq!(
                app.state.builder.pending_task_ids().first().copied(),
                Some(4)
            );
            let parsed = tasks::validate(&artifacts.join("tasks.toml")).expect("tasks valid");
            let ids = parsed.tasks.iter().map(|task| task.id).collect::<Vec<_>>();
            assert_eq!(ids, vec![1, 4, 5, 6]);
            assert_eq!(parsed.tasks[1].title, "Split A");
            assert_eq!(parsed.tasks[2].title, "Split B");
            assert_eq!(parsed.tasks[3].title, "Later");
            assert_eq!(parsed.tasks[3].spec_refs[0].lines, "1-2");
        });
    }

    #[test]
    fn recovery_requires_parseable_recovery_artifact() {
        with_temp_root(|| {
            let session_id = "recovery-invalid-artifact";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            let round_dir = session_dir.join("rounds").join("001");
            std::fs::create_dir_all(&artifacts).expect("artifacts dir");
            std::fs::create_dir_all(&round_dir).expect("round dir");
            std::fs::write(artifacts.join("spec.md"), "# Spec\n").expect("spec");
            std::fs::write(artifacts.join("plan.md"), "# Plan\n").expect("plan");
            std::fs::write(
                artifacts.join("tasks.toml"),
                r#"[[tasks]]
id = 2
title = "Recovered"
description = "valid"
test = "cargo test"
estimated_tokens = 10
"#,
            )
            .expect("tasks");
            std::fs::write(round_dir.join("recovery.toml"), "[[[broken").expect("recovery");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BuilderRecovery(1);
            state.agent_runs.push(RunRecord {
                id: 1,
                stage: "recovery".to_string(),
                task_id: None,
                round: 1,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Recovery]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
                hostname: None,
                mount_device_id: None,
            });
            let mut app = idle_app(state);
            let run = app.state.agent_runs[0].clone();
            let reason = app
                .normalized_failure_reason(&run)
                .expect("normalized")
                .expect("failure reason");

            assert!(reason.starts_with("artifact_invalid:"), "{reason}");
        });
    }

    #[test]
    fn recovery_reconcile_replaces_pending_and_sets_retry_reset_cutoff() {
        with_temp_root(|| {
            let session_id = "recovery-reconcile-success";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            let round_dir = session_dir.join("rounds").join("002");
            std::fs::create_dir_all(&artifacts).expect("artifacts dir");
            std::fs::create_dir_all(&round_dir).expect("round dir");
            std::fs::write(
                artifacts.join("spec.md"),
                "Spec\n\n## Recovery Notes\n- superseded task 2: split into 5\n",
            )
            .expect("spec");
            std::fs::write(
                artifacts.join("plan.md"),
                "Plan\n\n## Recovery Notes\n- superseded task 2: split into 5\n",
            )
            .expect("plan");
            std::fs::write(
                artifacts.join("tasks.toml"),
                r#"[[tasks]]
id = 2
title = "Finish task 2"
description = "do it"
test = "cargo test"
estimated_tokens = 10

[[tasks]]
id = 5
title = "New follow-up"
description = "new work"
test = "cargo test"
estimated_tokens = 10
"#,
            )
            .expect("tasks");
            std::fs::write(
                round_dir.join("recovery.toml"),
                r#"status = "agent_pivot"
summary = "recovered queue"
feedback = ["split task 2"]
"#,
            )
            .expect("recovery");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BuilderRecovery(2);
            state.builder.done = vec![1, 4];
            state.builder.pending = vec![2, 3];
            state.builder.current_task = Some(2);
            state.builder.recovery_prev_max_task_id = Some(4);
            state.builder.recovery_prev_task_ids = vec![1, 2, 3, 4];
            state.agent_runs.push(RunRecord {
                id: 7,
                stage: "coder".to_string(),
                task_id: Some(2),
                round: 2,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Coder]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Done,
                error: None,
                hostname: None,
                mount_device_id: None,
            });
            state.agent_runs.push(RunRecord {
                id: 8,
                stage: "recovery".to_string(),
                task_id: None,
                round: 2,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Recovery]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
                hostname: None,
                mount_device_id: None,
            });
            let mut app = idle_app(state);
            let run = app
                .state
                .agent_runs
                .iter()
                .find(|r| r.id == 8)
                .cloned()
                .expect("recovery run");
            app.finalize_current_run(&run).expect("finalize recovery");

            // Recovery now routes through plan-review → sharding before implementation.
            assert_eq!(app.state.current_phase, Phase::BuilderRecoveryPlanReview(2));
            assert_eq!(app.state.builder.done, vec![1, 4]);
            assert_eq!(app.state.builder.pending, vec![2, 5]);
            assert_eq!(app.state.builder.current_task, None);
            assert_eq!(app.state.builder.retry_reset_run_id_cutoff, Some(8));
        });
    }

    #[test]
    fn recovery_reconcile_requires_notes_for_superseded_started_tasks() {
        with_temp_root(|| {
            let session_id = "recovery-reconcile-notes";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts).expect("artifacts dir");
            std::fs::write(artifacts.join("spec.md"), "Spec without section").expect("spec");
            std::fs::write(artifacts.join("plan.md"), "Plan without section").expect("plan");
            std::fs::write(
                artifacts.join("tasks.toml"),
                r#"[[tasks]]
id = 6
title = "Replacement"
description = "replace task 2"
test = "cargo test"
estimated_tokens = 10
"#,
            )
            .expect("tasks");

            let mut state = SessionState::new(session_id.to_string());
            state.builder.done = vec![1];
            state.builder.recovery_prev_max_task_id = Some(5);
            state.builder.recovery_prev_task_ids = vec![1, 2, 3, 4, 5];
            state.agent_runs.push(RunRecord {
                id: 1,
                stage: "coder".to_string(),
                task_id: Some(2),
                round: 1,
                attempt: 1,
                model: "gpt-5".to_string(),
                vendor: "codex".to_string(),
                window_name: "[Coder]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Done,
                error: None,
                hostname: None,
                mount_device_id: None,
            });
            let mut app = idle_app(state);
            let err = app
                .reconcile_builder_recovery(99)
                .expect_err("expected supersession rejection");
            let text = format!("{err:#}");
            assert!(text.contains("Recovery Notes"));
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
                window_name: "[Coder]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Failed,
                error: Some("exit(1)".to_string()),
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
                window_name: "[Coder]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Failed,
                error: Some("exit(1)".to_string()),
                hostname: None,
                mount_device_id: None,
            });
            state.save().expect("save");
            let app = App::new(mk_tmux(), SessionState::load(session_id).expect("load"));
            let key = ("coder".to_string(), Some(1), 1);
            let failed = app.failed_models.get(&key).expect("failed set");
            assert_eq!(failed.len(), 1);
            assert!(failed.contains(&(selection::VendorKind::Codex, "gpt-5".to_string())));
            assert!(
                !failed.contains(&(selection::VendorKind::Claude, "claude-sonnet".to_string()))
            );
        });
    }

    #[test]
    fn recovery_auto_launch_is_idempotent_on_resume() {
        with_temp_root(|| {
            let session_id = "recovery-resume-autolaunch";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts).expect("artifacts dir");
            std::fs::write(artifacts.join("spec.md"), "spec").expect("spec");
            std::fs::write(artifacts.join("plan.md"), "plan").expect("plan");
            std::fs::write(
                artifacts.join("tasks.toml"),
                r#"[[tasks]]
id = 1
title = "Task"
description = "d"
test = "t"
estimated_tokens = 1
"#,
            )
            .expect("tasks");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BuilderRecovery(1);
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
                        artifact_contents: None,
                    }]),
                },
            )));

            app.maybe_auto_launch();
            let first_run_count = app.state.agent_runs.len();
            assert_eq!(first_run_count, 1);
            assert_eq!(app.state.agent_runs[0].stage, "recovery");

            app.maybe_auto_launch();
            assert_eq!(app.state.agent_runs.len(), first_run_count);
        });
    }

    #[test]
    fn brainstorm_failure_auto_retries_with_next_model() {
        with_temp_root(|| {
            let session_id = "brainstorm-retry";
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BrainstormRunning;
            state.idea_text = Some("idea".to_string());
            let run = RunRecord {
                id: 1,
                stage: "brainstorm".to_string(),
                task_id: None,
                round: 1,
                attempt: 1,
                model: "claude-sonnet".to_string(),
                vendor: "claude".to_string(),
                window_name: "[Brainstorm]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
                hostname: None,
                mount_device_id: None,
            };
            state.agent_runs.push(run.clone());
            let mut app = idle_app(state);
            app.models = vec![
                ranked_model(selection::VendorKind::Claude, "claude-sonnet", 1, 1, 1),
                ranked_model(selection::VendorKind::Codex, "gpt-5", 1, 1, 1),
            ];
            app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
                TestLaunchHarness {
                    outcomes: std::collections::VecDeque::from(vec![TestLaunchOutcome {
                        exit_code: 0,
                        artifact_contents: None,
                    }]),
                },
            )));
            std::fs::create_dir_all(app.run_status_path(&run).parent().expect("status dir"))
                .expect("create status dir");
            std::fs::write(app.run_status_path(&run), "1").expect("write exit code");

            app.finalize_current_run(&run)
                .expect("finalize brainstorm failure");
            assert_eq!(
                app.failed_models
                    .get(&("brainstorm".to_string(), None, 1))
                    .map(|set| set.len()),
                Some(1)
            );
            assert_eq!(app.state.agent_runs.len(), 2);
            assert_eq!(app.state.agent_runs[0].status, RunStatus::Failed);
            assert_eq!(app.state.agent_runs[1].status, RunStatus::Running);
            assert_eq!(app.state.agent_runs[1].stage, "brainstorm");
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

    // ── Recovery circuit-breaker and queue validation tests ──────────────────

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
            app.enter_builder_recovery(
                1,
                Some(1),
                Some("needs human".to_string()),
                "human_blocked",
            );

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
    fn circuit_breaker_escalates_to_human_blocked_after_3_cycles() {
        with_temp_root(|| {
            let mut state = SessionState::new("circuit-breaker-test".to_string());
            state.current_phase = Phase::ReviewRound(1);
            let session_dir = session_state::session_dir("circuit-breaker-test");
            let artifacts = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts).unwrap();
            std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

            let mut app = idle_app(state);

            // First call: agent_pivot (cycle 1)
            app.enter_builder_recovery(1, None, None, "agent_pivot");
            {
                let recovery_items: Vec<_> = app
                    .state
                    .builder
                    .pipeline_items
                    .iter()
                    .filter(|i| i.stage == "recovery")
                    .collect();
                assert_eq!(recovery_items[0].trigger.as_deref(), Some("agent_pivot"));
                assert_eq!(app.state.builder.recovery_cycle_count, 1);
            }

            // Remove the recovery item and reset phase for second call
            app.state
                .builder
                .pipeline_items
                .retain(|i| i.stage != "recovery");
            app.state.current_phase = Phase::ReviewRound(1);
            // write tasks.toml again since recovery may clear state
            std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

            // Second call: agent_pivot (cycle 2)
            app.enter_builder_recovery(1, None, None, "agent_pivot");
            assert_eq!(app.state.builder.recovery_cycle_count, 2);
            {
                let recovery_items: Vec<_> = app
                    .state
                    .builder
                    .pipeline_items
                    .iter()
                    .filter(|i| i.stage == "recovery")
                    .collect();
                assert_eq!(recovery_items[0].trigger.as_deref(), Some("agent_pivot"));
            }

            // Remove and reset for third call
            app.state
                .builder
                .pipeline_items
                .retain(|i| i.stage != "recovery");
            app.state.current_phase = Phase::ReviewRound(1);
            std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

            // Third call: agent_pivot → should escalate to human_blocked
            app.enter_builder_recovery(1, None, None, "agent_pivot");
            assert_eq!(app.state.builder.recovery_cycle_count, 3);
            {
                let recovery_items: Vec<_> = app
                    .state
                    .builder
                    .pipeline_items
                    .iter()
                    .filter(|i| i.stage == "recovery")
                    .collect();
                // Must be escalated to human_blocked
                assert_eq!(
                    recovery_items[0].trigger.as_deref(),
                    Some("human_blocked"),
                    "3rd cycle must escalate to human_blocked"
                );
                assert_eq!(recovery_items[0].interactive, Some(true));
            }
        });
    }

    #[test]
    fn circuit_breaker_already_human_blocked_does_not_double_escalate() {
        with_temp_root(|| {
            let mut state = SessionState::new("circuit-breaker-hb".to_string());
            state.current_phase = Phase::ReviewRound(1);
            // Start with count=2 to be just below threshold
            state.builder.recovery_cycle_count = 2;
            let session_dir = session_state::session_dir("circuit-breaker-hb");
            let artifacts = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts).unwrap();
            std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

            let mut app = idle_app(state);
            // Count becomes 3, trigger is already human_blocked — no double-escalation message
            app.enter_builder_recovery(1, None, None, "human_blocked");
            assert_eq!(app.state.builder.recovery_cycle_count, 3);
            let recovery_items: Vec<_> = app
                .state
                .builder
                .pipeline_items
                .iter()
                .filter(|i| i.stage == "recovery")
                .collect();
            // Stays human_blocked
            assert_eq!(recovery_items[0].trigger.as_deref(), Some("human_blocked"));
        });
    }

    #[test]
    fn circuit_breaker_resets_after_approved_plan_review() {
        // Verify that recovery_cycle_count is reset to 0 when the recovery
        // plan review is approved (see handle_recovery_plan_review_completed).
        let mut builder = crate::state::BuilderState {
            recovery_cycle_count: 3,
            ..crate::state::BuilderState::default()
        };
        // Simulate the reset that happens in handle_recovery_plan_review_completed
        builder.recovery_cycle_count = 0;
        assert_eq!(builder.recovery_cycle_count, 0);
    }

    #[test]
    fn recovery_queue_validation_rejects_completed_id_collision() {
        // reconcile_builder_recovery must reject recovered task ids that
        // collide with completed task ids.
        with_temp_root(|| {
            let session_id = "recovery-collision";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            let round_dir = session_dir.join("rounds").join("001");
            std::fs::create_dir_all(&artifacts).unwrap();
            std::fs::create_dir_all(&round_dir).unwrap();

            // Write a recovery.toml
            std::fs::write(
                round_dir.join("recovery.toml"),
                "status = \"approved\"\nsummary = \"Fixed\"\nfeedback = []\n",
            )
            .unwrap();
            // Write spec.md and plan.md (no recovery notes needed since no superseded started ids)
            std::fs::write(artifacts.join("spec.md"), "# Spec\n").unwrap();
            std::fs::write(artifacts.join("plan.md"), "# Plan\n").unwrap();

            // tasks.toml has task id 1 which is ALREADY done
            std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BuilderRecovery(1);
            state.builder.done = vec![1]; // task 1 is already done

            // Add a recovery pipeline item marked Running
            state.builder.push_pipeline_item(PipelineItem {
                id: 0,
                stage: "recovery".to_string(),
                task_id: None,
                round: Some(1),
                status: PipelineItemStatus::Running,
                title: None,
                mode: None,
                trigger: Some("agent_pivot".to_string()),
                interactive: Some(false),
            });
            let app = idle_app(state);
            // The reconcile should fail because task 1 is already completed
            // but the recovered tasks.toml also has task 1.
            let mut app = app;
            let result = app.reconcile_builder_recovery(0);
            assert!(result.is_err(), "collision with completed id must fail");
            let msg = format!("{:#}", result.unwrap_err());
            assert!(
                msg.contains("completed task id"),
                "error must mention collision: {msg}"
            );
        });
    }

    #[test]
    fn recovery_queue_reconcile_preserves_completed_tasks() {
        with_temp_root(|| {
            let session_id = "recovery-preserve";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            let round_dir = session_dir.join("rounds").join("001");
            std::fs::create_dir_all(&artifacts).unwrap();
            std::fs::create_dir_all(&round_dir).unwrap();

            std::fs::write(
                round_dir.join("recovery.toml"),
                "status = \"approved\"\nsummary = \"Fixed\"\nfeedback = []\n",
            )
            .unwrap();
            std::fs::write(artifacts.join("spec.md"), "# Spec\n").unwrap();
            std::fs::write(artifacts.join("plan.md"), "# Plan\n").unwrap();

            // Recovered tasks.toml has ids 5 and 6 (new, above old max of 2)
            std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 5\ntitle = \"New A\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n\
                 [[tasks]]\nid = 6\ntitle = \"New B\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BuilderRecovery(1);
            // Tasks 1 and 2 are completed
            state.builder.done = vec![1, 2];
            state.builder.push_pipeline_item(PipelineItem {
                id: 0,
                stage: "coder".to_string(),
                task_id: Some(1),
                round: Some(1),
                status: PipelineItemStatus::Approved,
                title: Some("Old Task 1".to_string()),
                mode: None,
                trigger: None,
                interactive: None,
            });
            state.builder.push_pipeline_item(PipelineItem {
                id: 0,
                stage: "coder".to_string(),
                task_id: Some(2),
                round: Some(1),
                status: PipelineItemStatus::Approved,
                title: Some("Old Task 2".to_string()),
                mode: None,
                trigger: None,
                interactive: None,
            });
            state.builder.push_pipeline_item(PipelineItem {
                id: 0,
                stage: "recovery".to_string(),
                task_id: None,
                round: Some(1),
                status: PipelineItemStatus::Running,
                title: None,
                mode: None,
                trigger: Some("agent_pivot".to_string()),
                interactive: Some(false),
            });
            state.builder.recovery_prev_max_task_id = Some(2);
            state.builder.sync_legacy_queue_views();

            let mut app = idle_app(state);
            app.reconcile_builder_recovery(0)
                .expect("reconcile must succeed");

            // Completed tasks 1 and 2 must still be present
            let done = app.state.builder.done_task_ids();
            assert!(done.contains(&1));
            assert!(done.contains(&2));

            // New tasks 5 and 6 must be pending
            let pending = app.state.builder.pending_task_ids();
            assert!(pending.contains(&5));
            assert!(pending.contains(&6));
        });
    }

    #[test]
    fn approved_review_with_feedback_emits_advisory_message() {
        with_temp_root(|| {
            let session_id = "approved-advisory";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            let round_dir = session_dir.join("rounds").join("001");
            std::fs::create_dir_all(&artifacts).unwrap();
            std::fs::create_dir_all(&round_dir).unwrap();

            // Write tasks.toml with one task
            std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

            // Write an approved review with non-empty feedback (advisory)
            std::fs::write(
                round_dir.join("review.toml"),
                "status = \"approved\"\nsummary = \"Implementation is correct\"\nfeedback = [\"Consider caching the result for performance\"]\n",
            )
            .unwrap();

            let mut state = SessionState::new(session_id.to_string());
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
            state.agent_runs.push(RunRecord {
                id: 1,
                stage: "reviewer".to_string(),
                task_id: Some(1),
                round: 1,
                attempt: 1,
                model: "test-model".to_string(),
                vendor: "test-vendor".to_string(),
                window_name: "[Review r1]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
                hostname: None,
                mount_device_id: None,
            });

            let mut app = idle_app(state);
            app.current_run_id = Some(1);
            app.window_launched = true;

            // Write the status file to signal success
            let status_path = app.run_status_path_for("reviewer", Some(1), 1, 1);
            if let Some(parent) = status_path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&status_path, "0").unwrap();
            write_finish_stamp(
                &session_dir,
                &App::run_key_for("reviewer", Some(1), 1, 1),
                "head789",
                "stable",
            );

            app.poll_agent_window();

            // The pipeline should still advance (not halted by advisory feedback)
            assert!(
                matches!(
                    app.state.current_phase,
                    Phase::ImplementationRound(_) | Phase::Done
                ),
                "Approved verdict must advance pipeline, got {:?}",
                app.state.current_phase
            );

            // An advisory message must have been emitted
            let advisory_msgs: Vec<_> = app
                .messages
                .iter()
                .filter(|m| m.kind == MessageKind::SummaryWarn)
                .filter(|m| m.text.contains("advisory"))
                .collect();
            assert!(
                !advisory_msgs.is_empty(),
                "advisory feedback must be surfaced as SummaryWarn message"
            );
        });
    }

    #[test]
    fn recovery_prompt_interactive_requires_operator_confirmation() {
        let tmp = tempfile::tempdir().unwrap();
        let prompt = recovery_prompt(
            &tmp.path().join("spec.md"),
            &tmp.path().join("plan.md"),
            &tmp.path().join("tasks.toml"),
            Some(1),
            Some("needs human judgment"),
            &[],
            &[1],
            &tmp.path().join("live_summary.txt"),
            &tmp.path().join("recovery.toml"),
            true,
        );
        assert!(
            prompt.contains("INTERACTIVE"),
            "human_blocked prompt must be marked INTERACTIVE"
        );
        assert!(
            !prompt.contains("NON-INTERACTIVE"),
            "human_blocked prompt must not contain NON-INTERACTIVE"
        );
        assert!(
            prompt.contains("operator confirms"),
            "human_blocked prompt must require operator confirmation"
        );
    }

    #[test]
    fn recovery_prompt_non_interactive_for_agent_pivot() {
        let tmp = tempfile::tempdir().unwrap();
        let prompt = recovery_prompt(
            &tmp.path().join("spec.md"),
            &tmp.path().join("plan.md"),
            &tmp.path().join("tasks.toml"),
            Some(2),
            Some("plan is wrong"),
            &[],
            &[2],
            &tmp.path().join("live_summary.txt"),
            &tmp.path().join("recovery.toml"),
            false,
        );
        assert!(
            prompt.contains("NON-INTERACTIVE"),
            "agent_pivot prompt must be NON-INTERACTIVE"
        );
        assert!(
            !prompt.contains("INTERACTIVE — the operator"),
            "agent_pivot prompt must not be marked INTERACTIVE"
        );
    }

    #[test]
    fn launch_recovery_uses_interactive_prompt_for_human_blocked() {
        use crate::state::PipelineItemStatus;
        with_temp_root(|| {
            let session_id = "recovery-interactive-launch";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts).unwrap();
            std::fs::write(artifacts.join("spec.md"), "# Spec\n").unwrap();
            std::fs::write(artifacts.join("plan.md"), "# Plan\n").unwrap();
            std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BuilderRecovery(1);
            state.builder.recovery_trigger_task_id = Some(1);
            state.builder.recovery_trigger_summary = Some("needs human judgment".to_string());
            state.builder.push_pipeline_item(PipelineItem {
                id: 0,
                stage: "recovery".to_string(),
                task_id: None,
                round: Some(1),
                status: PipelineItemStatus::Running,
                title: Some("Human-blocked recovery".to_string()),
                mode: None,
                trigger: Some("human_blocked".to_string()),
                interactive: Some(true),
            });

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
                        artifact_contents: None,
                    }]),
                },
            )));

            let ok = app.launch_recovery_with_model(None);
            assert!(ok, "launch_recovery_with_model must succeed");

            let prompt_path = session_dir.join("prompts").join("recovery-r1.md");
            let prompt = std::fs::read_to_string(&prompt_path).unwrap();
            assert!(
                prompt.contains("INTERACTIVE"),
                "human_blocked recovery prompt file must be INTERACTIVE"
            );
            assert!(
                !prompt.contains("NON-INTERACTIVE"),
                "human_blocked recovery prompt file must not be NON-INTERACTIVE"
            );
        });
    }

    #[test]
    fn launch_recovery_uses_noninteractive_prompt_for_agent_pivot() {
        use crate::state::PipelineItemStatus;
        with_temp_root(|| {
            let session_id = "recovery-noninteractive-launch";
            let session_dir = session_state::session_dir(session_id);
            let artifacts = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts).unwrap();
            std::fs::write(artifacts.join("spec.md"), "# Spec\n").unwrap();
            std::fs::write(artifacts.join("plan.md"), "# Plan\n").unwrap();
            std::fs::write(
                artifacts.join("tasks.toml"),
                "[[tasks]]\nid = 1\ntitle = \"T\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n",
            )
            .unwrap();

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BuilderRecovery(2);
            state.builder.recovery_trigger_task_id = Some(1);
            state.builder.recovery_trigger_summary = Some("plan is wrong".to_string());
            state.builder.push_pipeline_item(PipelineItem {
                id: 0,
                stage: "recovery".to_string(),
                task_id: None,
                round: Some(2),
                status: PipelineItemStatus::Running,
                title: Some("Agent pivot recovery".to_string()),
                mode: None,
                trigger: Some("agent_pivot".to_string()),
                interactive: Some(false),
            });

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
                        artifact_contents: None,
                    }]),
                },
            )));

            let ok = app.launch_recovery_with_model(None);
            assert!(ok, "launch_recovery_with_model must succeed");

            let prompt_path = session_dir.join("prompts").join("recovery-r2.md");
            let prompt = std::fs::read_to_string(&prompt_path).unwrap();
            assert!(
                prompt.contains("NON-INTERACTIVE"),
                "agent_pivot recovery prompt file must be NON-INTERACTIVE"
            );
        });
    }

    // ---------- pending guard decision tests ----------

    fn make_brainstorm_run(id: u64) -> RunRecord {
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
            hostname: None,
            mount_device_id: None,
        }
    }

    fn write_ask_operator_snapshot(session_dir: &std::path::Path) {
        let guard_dir = session_dir.join(".guards").join("brainstorm-stage-r1-a1");
        std::fs::create_dir_all(&guard_dir).expect("guard dir");
        std::fs::write(
            guard_dir.join("snapshot.toml"),
            "head = \"0000000000000000000000000000000000000000\"\ngit_status = \"\"\nmode = \"ask_operator\"\n\n[control_files]\n",
        )
        .expect("write snapshot");
    }

    #[test]
    fn normalize_failure_reason_pending_decision_parks_run() {
        with_temp_root(|| {
            let session_id = "pending-guard-park";
            let session_dir = session_state::session_dir(session_id);
            std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BrainstormRunning;
            let run = make_brainstorm_run(42);
            state.agent_runs.push(run.clone());
            let mut app = mk_app(state);

            std::fs::write(session_dir.join("artifacts").join("spec.md"), "# Spec\n")
                .expect("write spec");
            std::fs::create_dir_all(app.run_status_path(&run).parent().expect("status parent"))
                .expect("status dir");
            std::fs::write(app.run_status_path(&run), "0").expect("write exit code");
            write_ask_operator_snapshot(&session_dir);

            let result = app.normalized_failure_reason(&run).expect("call ok");
            assert!(
                result.is_none(),
                "PendingDecision must not become a hard failure reason, got: {result:?}"
            );
            let decision = app
                .state
                .pending_guard_decision
                .as_ref()
                .expect("pending_guard_decision must be Some after PendingDecision");
            assert_eq!(decision.run_id, run.id);
            assert_eq!(decision.stage, "brainstorm");
            assert_eq!(
                decision.captured_head,
                "0000000000000000000000000000000000000000"
            );
        });
    }

    #[test]
    fn finalize_current_run_transitions_to_git_guard_pending() {
        with_temp_root(|| {
            let session_id = "pending-guard-finalize";
            let session_dir = session_state::session_dir(session_id);
            std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::BrainstormRunning;
            let run = make_brainstorm_run(1);
            state.agent_runs.push(run.clone());
            let mut app = mk_app(state);

            std::fs::write(session_dir.join("artifacts").join("spec.md"), "# Spec\n")
                .expect("write spec");
            std::fs::create_dir_all(app.run_status_path(&run).parent().expect("parent"))
                .expect("dir");
            std::fs::write(app.run_status_path(&run), "0").expect("exit code");
            write_ask_operator_snapshot(&session_dir);

            app.finalize_current_run(&run).expect("finalize ok");
            assert_eq!(
                app.state.current_phase,
                Phase::GitGuardPending,
                "phase must be GitGuardPending after parked run"
            );
            assert!(
                app.state.pending_guard_decision.is_some(),
                "pending_guard_decision must be set"
            );
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
            let warned = app.messages.iter().any(|m| {
                m.kind == MessageKind::SummaryWarn && m.text.contains("some guard warning")
            });
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

    fn make_pending_guard_state(session_id: &str, run_id: u64) -> SessionState {
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

    fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
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
            assert!(
                app.confirm_back,
                "unrelated modal keys must not fall through to normal key handling"
            );
            assert!(app.state.pending_guard_decision.is_some());
        });
    }

    #[test]
    fn pending_guard_resume_fail_closed_when_decision_missing() {
        with_temp_root(|| {
            let session_id = "pending-guard-resume-fail";
            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::GitGuardPending;
            state.save().expect("save");

            let app = App::new(
                mk_tmux(),
                SessionState::load(session_id).expect("load session"),
            );
            assert_eq!(
                app.state.current_phase,
                Phase::BlockedNeedsUser,
                "must fail closed to BlockedNeedsUser"
            );
            assert!(
                app.state.agent_error.is_some(),
                "agent_error must be set on fail-closed"
            );
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

            let app = App::new(
                mk_tmux(),
                SessionState::load(session_id).expect("load session"),
            );
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

            let app = App::new(
                mk_tmux(),
                SessionState::load(session_id).expect("load session"),
            );
            assert!(
                app.state.pending_guard_decision.is_none(),
                "stale pending_guard_decision must be cleared on resume"
            );
            assert_eq!(app.state.current_phase, Phase::BrainstormRunning);
        });
    }

    #[test]
    fn orphan_live_summary_files_removed_at_session_start() {
        with_temp_root(|| {
            let session_id = "orphan-live-summary-sweep";
            let session_dir = session_state::session_dir(session_id);
            let artifacts_dir = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");

            let mut state = SessionState::new(session_id.to_string());
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
                hostname: None,
                mount_device_id: None,
            });

            let live_txt = artifacts_dir.join("live_summary.txt");
            std::fs::write(&live_txt, "stale").expect("write live_summary.txt");
            let running_key = App::run_key_for("brainstorm", None, 1, 1);
            let running_path = artifacts_dir.join(format!("live_summary.{running_key}.txt"));
            std::fs::write(&running_path, "running").expect("write running live_summary");
            let orphan_path = artifacts_dir.join("live_summary.orphan.txt");
            std::fs::write(&orphan_path, "orphan").expect("write orphan live_summary");

            assert!(live_txt.exists());
            assert!(running_path.exists());
            assert!(orphan_path.exists());

            let _app = App::new(mk_tmux(), state);

            assert!(
                !live_txt.exists(),
                "pointer live_summary.txt must be removed at startup"
            );
            assert!(
                running_path.exists(),
                "live_summary.<run_key>.txt for Running record must be retained"
            );
            assert!(
                !orphan_path.exists(),
                "orphan live_summary.<run_key>.txt must be removed at startup"
            );
        });
    }

    #[test]
    fn resume_missing_window_honors_present_finish_stamp_for_coder() {
        with_temp_root(|| {
            let session_id = "resume-coder-stamp-present";
            let session_dir = session_state::session_dir(session_id);
            std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ImplementationRound(1);
            state.agent_runs.push(RunRecord {
                id: 1,
                stage: "coder".to_string(),
                task_id: Some(1),
                round: 1,
                attempt: 1,
                model: "m".to_string(),
                vendor: "v".to_string(),
                window_name: "[Coder r1]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
                hostname: None,
                mount_device_id: None,
            });

            let round_dir = session_dir.join("rounds").join("001");
            write_review_scope(&round_dir, "base123");
            write_finish_stamp(
                &session_dir,
                &App::run_key_for("coder", Some(1), 1, 1),
                "after",
                "stable",
            );

            let resumed = state
                .resume_running_runs(&[])
                .expect("resume")
                .expect("run id");

            let mut app = idle_app(state);
            app.current_run_id = Some(resumed);
            app.window_launched = true;
            app.poll_agent_window();

            let run = app
                .state
                .agent_runs
                .iter()
                .find(|r| r.id == 1)
                .expect("run");
            assert_eq!(run.status, RunStatus::Done);
            assert_eq!(app.state.current_phase, Phase::ReviewRound(1));
        });
    }

    #[test]
    fn resume_missing_window_missing_stamp_fails_unverified_for_coder() {
        with_temp_root(|| {
            let session_id = "resume-coder-stamp-missing";
            let session_dir = session_state::session_dir(session_id);
            std::fs::create_dir_all(session_dir.join("artifacts")).expect("artifacts dir");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::ImplementationRound(1);
            state.agent_runs.push(RunRecord {
                id: 1,
                stage: "coder".to_string(),
                task_id: Some(1),
                round: 1,
                attempt: 1,
                model: "m".to_string(),
                vendor: "v".to_string(),
                window_name: "[Coder r1]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: None,
                status: RunStatus::Running,
                error: None,
                hostname: None,
                mount_device_id: None,
            });

            let round_dir = session_dir.join("rounds").join("001");
            write_review_scope(&round_dir, "base123");

            let resumed = state
                .resume_running_runs(&[])
                .expect("resume")
                .expect("run id");

            let mut app = idle_app(state);
            app.current_run_id = Some(resumed);
            app.window_launched = true;
            app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));
            app.poll_agent_window();

            let run = app
                .state
                .agent_runs
                .iter()
                .find(|r| r.id == 1)
                .expect("run");
            assert_eq!(run.status, RunStatus::FailedUnverified);
            assert!(
                run.error
                    .as_deref()
                    .unwrap_or_default()
                    .contains("missing finish stamp"),
                "must fail closed on missing stamp"
            );
            assert_eq!(app.state.current_phase, Phase::ImplementationRound(1));
        });
    }

    #[test]
    fn resume_missing_window_missing_stamp_warns_and_finalizes_for_non_coder() {
        with_temp_root(|| {
            let session_id = "resume-planning-stamp-missing";
            let session_dir = session_state::session_dir(session_id);
            let artifacts_dir = session_dir.join("artifacts");
            std::fs::create_dir_all(&artifacts_dir).expect("artifacts dir");
            std::fs::write(artifacts_dir.join("plan.md"), "# Plan\n").expect("write plan");

            let mut state = SessionState::new(session_id.to_string());
            state.current_phase = Phase::PlanningRunning;
            state.agent_runs.push(RunRecord {
                id: 1,
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
                hostname: None,
                mount_device_id: None,
            });

            let resumed = state
                .resume_running_runs(&[])
                .expect("resume")
                .expect("run id");

            let mut app = idle_app(state);
            app.test_launch_harness = Some(std::sync::Arc::new(std::sync::Mutex::new(
                TestLaunchHarness::default(),
            )));
            app.current_run_id = Some(resumed);
            app.window_launched = true;
            app.pending_drain_deadline = Some(Instant::now() - Duration::from_millis(1));
            app.poll_agent_window();

            let run = app
                .state
                .agent_runs
                .iter()
                .find(|r| r.id == 1)
                .expect("run");
            assert_eq!(run.status, RunStatus::Done);
            assert_eq!(app.state.current_phase, Phase::PlanReviewRunning);

            let warned = app.messages.iter().any(|m| {
                m.kind == MessageKind::SummaryWarn && m.text.contains("finish_stamp_missing:")
            });
            assert!(
                warned,
                "non-coder missing stamp must warn on barrier release"
            );
        });
    }

    #[test]
    fn stamp_archival_moves_old_stamps_at_session_start() {
        use crate::runner::{FinishStamp, write_finish_stamp};

        with_temp_root(|| {
            let session_id = "stamp-archival-test";
            let mut state = SessionState::new(session_id.to_string());

            let old_time = chrono::Utc::now() - chrono::Duration::hours(2);
            let recent_time = chrono::Utc::now() - chrono::Duration::minutes(5);

            state.agent_runs.push(RunRecord {
                id: 1,
                stage: "coder".to_string(),
                task_id: Some(1),
                round: 1,
                attempt: 1,
                model: "claude".to_string(),
                vendor: "anthropic".to_string(),
                window_name: "[Coder 1]".to_string(),
                started_at: recent_time,
                ended_at: None,
                status: RunStatus::Running,
                error: None,
                hostname: None,
                mount_device_id: None,
            });
            state.save().unwrap();

            let finish_dir = session_state::session_dir(session_id)
                .join("artifacts")
                .join("run-finish");
            std::fs::create_dir_all(&finish_dir).unwrap();

            let old_stamp = FinishStamp {
                finished_at: old_time.to_rfc3339(),
                exit_code: 0,
                head_before: "aaa".to_string(),
                head_after: "bbb".to_string(),
                head_state: "stable".to_string(),
            };
            let old_stamp_path = finish_dir.join("old-stamp.toml");
            write_finish_stamp(&old_stamp_path, &old_stamp).unwrap();

            let recent_stamp = FinishStamp {
                finished_at: recent_time.to_rfc3339(),
                exit_code: 0,
                head_before: "ccc".to_string(),
                head_after: "ddd".to_string(),
                head_state: "stable".to_string(),
            };
            let recent_stamp_path = finish_dir.join("recent-stamp.toml");
            write_finish_stamp(&recent_stamp_path, &recent_stamp).unwrap();

            assert!(
                old_stamp_path.exists(),
                "old stamp should exist before App creation"
            );
            assert!(
                recent_stamp_path.exists(),
                "recent stamp should exist before App creation"
            );

            // Create App which triggers archival
            let _app = App::new(mk_tmux(), state);

            let archive_dir = finish_dir.join("archive");
            if !old_stamp_path.exists() {
                // Stamp was archived
                assert!(
                    archive_dir.exists(),
                    "archive directory should be created when stamps are archived"
                );
                assert!(
                    archive_dir.join("old-stamp.toml").exists(),
                    "old stamp should be moved to archive"
                );
            }
            assert!(
                recent_stamp_path.exists(),
                "recent stamp should remain in main directory"
            );
        });
    }

    #[test]
    fn archived_stamps_not_consulted_by_coder_gate() {
        use crate::runner::{FinishStamp, write_finish_stamp};

        with_temp_root(|| {
            let session_id = "archived-stamp-ignore";
            let mut state = SessionState::new(session_id.to_string());

            state.agent_runs.push(RunRecord {
                id: 1,
                stage: "coder".to_string(),
                task_id: Some(1),
                round: 1,
                attempt: 1,
                model: "claude".to_string(),
                vendor: "anthropic".to_string(),
                window_name: "[Coder]".to_string(),
                started_at: chrono::Utc::now(),
                ended_at: Some(chrono::Utc::now()),
                status: RunStatus::Running,
                error: None,
                hostname: None,
                mount_device_id: None,
            });
            state.save().unwrap();

            let finish_dir = session_state::session_dir(session_id)
                .join("artifacts")
                .join("run-finish");
            let archive_dir = finish_dir.join("archive");
            std::fs::create_dir_all(&archive_dir).unwrap();

            let run_key = App::run_key_for("coder", Some(1), 1, 1);
            let archived_stamp_path = archive_dir.join(format!("{run_key}.toml"));
            let archived_stamp = FinishStamp {
                finished_at: chrono::Utc::now().to_rfc3339(),
                exit_code: 0,
                head_before: "base".to_string(),
                head_after: "advanced".to_string(),
                head_state: "stable".to_string(),
            };
            write_finish_stamp(&archived_stamp_path, &archived_stamp).unwrap();

            let round_dir = session_state::session_dir(session_id)
                .join("rounds")
                .join("001");
            std::fs::create_dir_all(&round_dir).unwrap();
            std::fs::write(round_dir.join("review_scope.toml"), "base_sha = \"base\"\n").unwrap();

            let app = App::new(mk_tmux(), SessionState::load(session_id).unwrap());
            let run = &app.state.agent_runs[0];
            let reason = app.coder_gate_reason(run, &round_dir);

            assert!(
                reason.is_some(),
                "archived stamp must not be consulted; should return failure reason"
            );
            assert!(
                reason.unwrap().contains("missing finish stamp"),
                "should report missing stamp, not use archived one"
            );
        });
    }
}
