// lifecycle/mod.rs
mod init;
mod poll;
mod retry;
mod viewport;
mod viewport_layout;
use super::prompts::*;
use super::*;
use crate::{
    artifacts::{ArtifactKind, Spec},
    state::{self as session_state, BlockOrigin, MessageKind, Phase, RunStatus},
    tasks,
    tui::AppTerminal,
};
use anyhow::Result;
use std::time::Duration;
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
                ModalKind::FinalValidationBlocked => RuntimeModalKind::FinalValidationBlocked,
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
            Phase::BlockedNeedsUser
                if self.state.block_origin == Some(BlockOrigin::FinalValidation) =>
            {
                Some(ModalKind::FinalValidationBlocked)
            }
            _ => None,
        }
    }
    pub(crate) fn interactive_exit_prompt_key(&self) -> Option<(u64, usize)> {
        let run_id = self.current_run_id?;
        self.state.agent_runs.iter().find(|run| {
            run.id == run_id && run.status == RunStatus::Running && run.modes.interactive
        })?;
        if !self.runner_supervisor.run_is_waiting_for_input(run_id) {
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
    pub fn run(&mut self, terminal: &mut AppTerminal) -> Result<()> {
        crate::app_runtime::run_terminal_app(self, terminal)
    }
    /// Pre-data-drain phase of the per-tick coordination. The runtime calls
    /// this, then drains and routes any [`DataEvent`](crate::data::events::DataEvent)s,
    /// then finishes the tick via [`Self::runtime_tick_after_data_drain`].
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
            self.runner_supervisor.shutdown_all_runs();
            return Ok(true);
        }
        self.maybe_yolo_auto_resolve();
        self.maybe_auto_launch();
        self.update_agent_progress();
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
        self.complete_run_finalization(&run, Some(Reason::ForbiddenHeadAdvance.to_string()))
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
