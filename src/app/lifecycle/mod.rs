// lifecycle/mod.rs
mod poll;
mod retry;
mod viewport;

use super::*;
use crate::{
    artifacts::{ArtifactKind, Spec},
    cache,
    selection::{self, ranking::build_version_index},
    state::{self as session_state, MessageKind, Phase, RunStatus, SessionState},
    tasks,
    tui::AppTerminal,
};
use anyhow::Result;

use super::{
    models::spawn_refresh,
    prompts::*,
    state::ModelRefreshState,
    tree::{build_tree, current_node_index, node_key_at_path},
};

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant},
};

fn parse_task_label_id(label: &str) -> Option<u32> {
    let rest = label.strip_prefix("Task ")?;
    let digits = rest.split(':').next()?.split_whitespace().next()?;
    digits.parse().ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetryTarget {
    Task(u32),
    Stage(&'static str),
}

impl RetryTarget {
    fn label(&self) -> String {
        match self {
            Self::Task(task_id) => format!("task {task_id}"),
            Self::Stage(stage) => stage.replace('-', " "),
        }
    }
}

fn retry_stage_for_label(label: &str) -> Option<&'static str> {
    match label {
        "Brainstorm" => Some("brainstorm"),
        "Spec Review" => Some("spec-review"),
        "Planning" => Some("planning"),
        "Plan Review" => Some("plan-review"),
        "Sharding" => Some("sharding"),
        _ => None,
    }
}

fn retry_target_for_run(run: &crate::state::RunRecord) -> Option<RetryTarget> {
    crate::logic::rules::retry_target_for_run(run).map(|target| match target {
        crate::logic::rules::RetryTarget::Task(task_id) => RetryTarget::Task(task_id),
        crate::logic::rules::RetryTarget::Stage(stage) => RetryTarget::Stage(stage),
    })
}

impl App {
    pub(crate) fn current_app_view(&self) -> crate::app_runtime::AppView {
        use crate::app_runtime::{
            AgentRunSummary, AppView, ModalKind as RuntimeModalKind, ModeFlags,
            StageId as RuntimeStageId,
        };
        use std::sync::Arc;

        fn stage_id(stage: StageId) -> RuntimeStageId {
            match stage {
                StageId::Brainstorm => RuntimeStageId::Brainstorm,
                StageId::SpecReview => RuntimeStageId::SpecReview,
                StageId::Planning => RuntimeStageId::Planning,
                StageId::PlanReview => RuntimeStageId::PlanReview,
                StageId::Sharding => RuntimeStageId::Sharding,
                StageId::Implementation => RuntimeStageId::Implementation,
                StageId::Review => RuntimeStageId::Review,
            }
        }

        fn modal_kind(modal: ModalKind) -> RuntimeModalKind {
            match modal {
                ModalKind::SkipToImpl => RuntimeModalKind::SkipToImpl,
                ModalKind::GitGuard => RuntimeModalKind::GitGuard,
                ModalKind::QuitRunningAgent => RuntimeModalKind::QuitRunningAgent,
                ModalKind::InteractiveExitPrompt => RuntimeModalKind::InteractiveExitPrompt,
                ModalKind::SpecReviewPaused => RuntimeModalKind::SpecReviewPaused,
                ModalKind::PlanReviewPaused => RuntimeModalKind::PlanReviewPaused,
                ModalKind::StageError(stage) => RuntimeModalKind::StageError(stage_id(stage)),
            }
        }

        AppView {
            session_id: Arc::from(self.state.session_id.as_str()),
            phase: self.state.current_phase,
            modal: self.active_modal().map(modal_kind),
            status: None,
            agent_runs: Arc::from(
                self.state
                    .agent_runs
                    .iter()
                    .map(AgentRunSummary::from_record)
                    .collect::<Vec<_>>(),
            ),
            follow_tail: self.split_follow_tail,
            agent_running: self.has_running_agent(),
            modes: ModeFlags {
                yolo: self.state.modes.yolo,
                cheap: self.state.modes.cheap,
            },
        }
    }

    pub(crate) fn active_modal(&self) -> Option<ModalKind> {
        if self.pending_quit_confirmation_run_id.is_some() {
            return Some(ModalKind::QuitRunningAgent);
        }
        if self.interactive_exit_prompt_key().is_some() {
            return Some(ModalKind::InteractiveExitPrompt);
        }
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

    pub(crate) fn interactive_exit_prompt_key(&self) -> Option<(u64, usize)> {
        let run_id = self.current_run_id?;
        let run = self.state.agent_runs.iter().find(|run| {
            run.id == run_id && run.status == RunStatus::Running && run.modes.interactive
        })?;
        if !crate::runner::run_label_is_waiting_for_input(&run.window_name) {
            return None;
        }
        let (message_index, message) =
            self.messages
                .iter()
                .enumerate()
                .rev()
                .find(|(_, message)| {
                    message.run_id == run_id && message.kind == MessageKind::AgentText
                })?;
        if !message.text.contains("/exit") {
            return None;
        }
        let key = (run_id, message_index);
        if self.interactive_exit_prompt_dismissed_at == Some(key) {
            return None;
        }
        Some(key)
    }

    pub fn new(state: SessionState) -> Self {
        Self::new_with_startup_origin(state, AppStartupOrigin::Default)
    }

    pub fn new_with_startup_origin(
        mut state: SessionState,
        startup_origin: AppStartupOrigin,
    ) -> Self {
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
            split_target: None,
            split_follow_tail: true,
            split_scroll_offset: 0,
            split_fullscreen: false,
            input_mode: false,
            input_buffer: String::new(),
            input_cursor: 0,
            pending_view_path: None,
            confirm_back: false,
            startup_origin,
            run_launched: false,
            quota_errors: Vec::new(),
            quota_retry_delay: Duration::from_secs(60),
            agent_line_count: 0,
            agent_content_hash: 0,
            agent_last_change: None,
            spinner_tick: 0,
            live_summary_spinner_visible: false,
            live_summary_path: None,
            live_summary_watcher: None,
            live_summary_change_events: None,
            live_summary_cached_text: String::new(),
            live_summary_cached_mtime: None,
            pending_drain_deadline: None,
            pending_termination: None,
            pending_quit_confirmation_run_id: None,
            interactive_exit_prompt_dismissed_at: None,
            pending_app_exit: false,
            current_run_id: None,
            failed_models,
            pending_yolo_toggle_gate: None,
            yolo_exit_issued: HashSet::new(),
            yolo_exit_observations: HashMap::new(),
            watchdog: super::watchdog::WatchdogRegistry::from_env(),
            #[cfg(test)]
            test_launch_harness: None,
            messages,
            status_line: Rc::new(RefCell::new(status_line::StatusLine::new())),
            prev_models_mode: models_area::ModelsAreaMode::default(),
            palette: palette::PaletteState::default(),
            command_return_target: None,
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
        if let Ok(run_id) = session_state::transitions::resume_running_runs(&mut app.state) {
            app.current_run_id = run_id;
            app.run_launched = run_id.is_some();
            if let Some(rid) = run_id {
                if let Some(run) = app.state.agent_runs.iter().find(|r| r.id == rid).cloned() {
                    app.live_summary_path = Some(app.live_summary_path_for(&run));
                    app.prime_yolo_exit_tracking(&run);
                }
                app.read_live_summary_pipeline();
            }
            app.messages = SessionState::load_messages(&app.state.session_id).unwrap_or_default();
            app.rebuild_tree_view(None);
            app.maybe_refocus_to_progress();
        }
        // Resume validation: if the session was interrupted mid-guard-decision,
        // restore the modal or fail closed.
        if app.state.current_phase == Phase::GitGuardPending {
            if app.state.pending_guard_decision.is_none() {
                app.record_agent_error("guard pending state missing on resume".to_string());
                app.clear_builder_recovery_context();
                let _ = app.transition_to_blocked(crate::state::BlockOrigin::GitGuard);
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
        crate::app_runtime::run_terminal_app(self, terminal)
    }

    /// Pre-data-drain phase of the per-tick coordination. The runtime calls
    /// this, then drains any [`DataEvent`](crate::data::events::DataEvent)s
    /// it owns (e.g. tool-call transitions) and routes them per-event, then
    /// finishes the tick via [`Self::runtime_tick_after_data_drain`].
    pub(crate) fn runtime_tick_before_data_drain(
        &mut self,
        terminal: &mut AppTerminal,
    ) -> Result<bool> {
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
        self.poll_agent_run();
        if self.pending_app_exit {
            crate::runner::shutdown_all_runs();
            return Ok(true);
        }
        self.maybe_yolo_auto_resolve();
        self.maybe_auto_launch();
        self.update_agent_progress();
        self.process_live_summary_changes();
        Ok(false)
    }

    /// Post-data-drain phase: watchdog evaluation and split-target sync after
    /// the runtime has applied any drained `DataEvent`s. Watchdog state is
    /// observed *after* the drain so tool-call transitions land on the
    /// state the watchdog evaluates this tick.
    pub(crate) fn runtime_tick_after_data_drain(&mut self) {
        self.tick_watchdog();
        self.synchronize_split_target();
    }

    /// Called once per successful frame draw to advance spinner-driven UI state.
    pub(crate) fn on_frame_drawn(&mut self) {
        // Consume the picker-created startup origin only after a successful
        // draw so cached models cannot auto-launch brainstorm before the first
        // visible session frame lands on screen.
        if self.startup_origin == AppStartupOrigin::PickerCreated {
            self.startup_origin = AppStartupOrigin::Default;
        }
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
    }

    pub(crate) fn event_poll_duration(&self) -> Duration {
        if self.live_summary_spinner_visible {
            Duration::from_millis(LIVE_SUMMARY_EVENT_POLL_MS)
        } else {
            Duration::from_millis(DEFAULT_EVENT_POLL_MS)
        }
    }


    pub(crate) fn transition_to_phase(&mut self, next_phase: Phase) -> Result<()> {
        session_state::transitions::execute_transition(&mut self.state, next_phase)?;
        // Pin the round's review scope at round entry — including the
        // skip-to-impl path that has no reviewer stage and goal-gap re-runs
        // that create a fresh implementation round — so the simplifier can
        // consistently use `base_sha..HEAD`. `capture_round_base` is
        // idempotent on resume.
        if let Phase::ImplementationRound(round) = next_phase {
            let round_dir = session_state::session_dir(&self.state.session_id)
                .join("rounds")
                .join(format!("{round:03}"));
            self.capture_round_base(&round_dir);
        }
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

    /// Set `block_origin` on the session and transition into
    /// `BlockedNeedsUser`. The single throat for entering a block from app
    /// code so the persisted provenance is always populated and the
    /// force-ship guard has a value to read.
    pub(crate) fn transition_to_blocked(
        &mut self,
        origin: crate::state::BlockOrigin,
    ) -> Result<()> {
        self.state.block_origin = Some(origin);
        self.transition_to_phase(Phase::BlockedNeedsUser)
    }

    pub(crate) fn record_agent_error(&mut self, message: impl Into<String>) {
        session_state::transitions::record_agent_error(&mut self.state, message);
    }

    pub(crate) fn clear_agent_error(&mut self) {
        session_state::transitions::clear_agent_error(&mut self.state);
    }

    pub(crate) fn clear_builder_recovery_context(&mut self) {
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

    pub(crate) fn editable_artifact(&self) -> Option<std::path::PathBuf> {
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
            | Phase::GitGuardPending
            | Phase::FinalValidation(_)
            | Phase::Simplification(_) => {
                return None;
            }
        };
        if path.exists() { Some(path) } else { None }
    }

    pub(crate) fn open_editable_artifact(&mut self) {
        let Some(path) = self.editable_artifact() else {
            return;
        };
        self.pending_view_path = Some(path);
    }

    pub(crate) fn queue_view_of_current_artifact(&mut self, filename: &str) {
        let path = session_state::session_dir(&self.state.session_id)
            .join("artifacts")
            .join(filename);
        if path.exists() {
            self.pending_view_path = Some(path);
        }
    }

    pub(crate) fn can_go_back(&self) -> bool {
        !matches!(self.state.current_phase, Phase::IdeaInput | Phase::Done)
    }

}
