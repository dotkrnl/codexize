// lifecycle/mod.rs
mod init;
#[cfg(test)]
mod init_tests;
mod poll;
mod retry;
#[cfg(test)]
mod retry_tests;
mod viewport;
mod viewport_layout;
use super::prompts::*;
use super::*;
use crate::{
    artifacts::{ArtifactKind, Spec},
    state::{
        self as session_state, BlockOrigin, DreamingDecisionKind, MessageKind, Phase, RunStatus,
    },
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
                StageId::FinalValidation => RuntimeStageId::FinalValidation,
                StageId::Dreaming => RuntimeStageId::Dreaming,
            }
        }
        fn modal_kind(modal: ModalKind) -> RuntimeModalKind {
            match modal {
                ModalKind::SkipToImpl => RuntimeModalKind::SkipToImpl,
                ModalKind::GitGuard => RuntimeModalKind::GitGuard,
                ModalKind::QuitRunningAgent => RuntimeModalKind::QuitRunningAgent,
                ModalKind::CancelSession => RuntimeModalKind::CancelSession,
                ModalKind::InteractiveExitPrompt => RuntimeModalKind::InteractiveExitPrompt,
                ModalKind::SpecReviewPaused => RuntimeModalKind::SpecReviewPaused,
                ModalKind::PlanReviewPaused => RuntimeModalKind::PlanReviewPaused,
                ModalKind::StageError(stage) => RuntimeModalKind::StageError(stage_id(stage)),
                ModalKind::FinalValidationBlocked => RuntimeModalKind::FinalValidationBlocked,
                ModalKind::DreamingDecision => RuntimeModalKind::DreamingDecision,
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
        if self.pending_cancel_confirmation {
            return Some(ModalKind::CancelSession);
        }
        if self.interactive_exit_prompt_key().is_some() {
            return Some(ModalKind::InteractiveExitPrompt);
        }
        match self.state.current_phase {
            Phase::SkipToImplPending => Some(ModalKind::SkipToImpl),
            Phase::GitGuardPending => Some(ModalKind::GitGuard),
            Phase::SpecReviewPaused => Some(ModalKind::SpecReviewPaused),
            Phase::PlanReviewPaused => Some(ModalKind::PlanReviewPaused),
            Phase::DreamingPending
                if self
                    .state
                    .dreaming_decision
                    .as_ref()
                    .is_some_and(|decision| decision.kind == DreamingDecisionKind::Pending) =>
            {
                Some(ModalKind::DreamingDecision)
            }
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
            Phase::FinalValidation(_) if self.state.agent_error.is_some() => {
                Some(ModalKind::StageError(StageId::FinalValidation))
            }
            Phase::Dreaming(_) if self.state.agent_error.is_some() => {
                Some(ModalKind::StageError(StageId::Dreaming))
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
    /// If the operator has queued an artifact path for external review,
    /// take it. The terminal runtime drives the actual editor launch so it
    /// can stop the crossterm input worker first — leaving the worker
    /// running would race vim for keystrokes off the same TTY (see
    /// [`Self::run_external_view_editor`]).
    pub(crate) fn take_pending_view_path(&mut self) -> Option<std::path::PathBuf> {
        self.pending_view_path.take()
    }
    /// Hand the terminal to `$EDITOR` (default `vim`) for `path`, prepending
    /// a review banner that is stripped when the editor exits. Must only be
    /// called with the crossterm input worker already shut down — the
    /// runtime arranges that around the call.
    pub(crate) fn run_external_view_editor(
        &mut self,
        terminal: &mut AppTerminal,
        path: &std::path::Path,
    ) {
        let banner_inserted = prepend_review_banner(path);
        let _ = crate::tui::run_foreground(terminal, || {
            let _ = std::process::Command::new(
                std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string()),
            )
            .arg(path)
            .status();
            Ok(())
        });
        if banner_inserted {
            let _ = strip_review_banner(path);
        }
    }
    /// Pre-data-drain phase of the per-tick coordination. The runtime calls
    /// this, then drains and routes any [`DataEvent`](crate::data::events::DataEvent)s,
    /// then finishes the tick via [`Self::runtime_tick_after_data_drain`].
    pub(crate) fn runtime_tick_before_data_drain(&mut self) -> Result<bool> {
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
        self.maybe_emit_interactive_wait_notification();
        self.poll_notification_reports();
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
    /// Recompute `App::slim_phase` from `state.current_phase`.
    ///
    /// The slim phase is a derived projection — it never lives on disk and
    /// must be refreshed every time the legacy phase mutates. This helper is
    /// the single refresh point so the derived slim phase stays in sync with
    /// the authoritative legacy phase.
    pub(crate) fn refresh_slim_phase(&mut self) {
        self.slim_phase = crate::lifecycle::slim_phase_for(&self.state.current_phase);
    }

    /// Project the App's agent-run history into the slim
    /// [`crate::lifecycle::RunHistoryEntry`] shape `LifecycleOps` consumes.
    /// Outcomes are best-effort approximations of the legacy `RunStatus`
    /// values. Used by restart's `build_spec` lookup, which doesn't inspect
    /// the outcome variant.
    pub(crate) fn slim_run_history(&self) -> Vec<crate::lifecycle::RunHistoryEntry> {
        use crate::state::RunStatus;
        self.state
            .agent_runs
            .iter()
            .map(|run| {
                let outcome = match run.status {
                    RunStatus::Running => None,
                    RunStatus::Done => Some(crate::lifecycle::Outcome::Done),
                    RunStatus::Failed => Some(crate::lifecycle::Outcome::Failed(
                        run.error.clone().unwrap_or_default(),
                    )),
                    RunStatus::FailedUnverified => Some(
                        crate::lifecycle::Outcome::FailedUnverified(
                            run.error.clone().unwrap_or_default(),
                        ),
                    ),
                };
                let stage_id = crate::lifecycle::stage_id_for_run(&run.stage, &run.window_name)
                    .unwrap_or(crate::lifecycle::StageId::Coder);
                crate::lifecycle::RunHistoryEntry {
                    stage_id,
                    task_id: run.task_id,
                    round: run.round,
                    attempt: run.attempt,
                    outcome,
                }
            })
            .collect()
    }

    /// Build an [`crate::lifecycle::OpsCtx`] in-place and hand it to `f`.
    ///
    /// OpsCtx borrows multiple App fields disjointly which is awkward to
    /// express as a returned struct. The closure form keeps the borrow
    /// scope tight and lets the caller invoke any of the four
    /// [`crate::lifecycle::LifecycleOps`] members inline. Snapshots
    /// `prior_runs` to a `Vec` and `session_dir` to a `PathBuf` before the
    /// borrow because they thread through `StageCtx` as references.
    ///
    /// In 5a only `:stop` and `:restart` use this; both paths leave
    /// `phase` unchanged, so we point `OpsCtx::phase` at a local mirror
    /// of `self.slim_phase` rather than at the field directly. 5b's
    /// rewind path will swap that for a real mutable borrow.
    pub(crate) fn with_running_agent_ops_ctx<R>(
        &mut self,
        f: impl FnOnce(&mut crate::lifecycle::OpsCtx<'_>) -> R,
    ) -> R {
        let session_dir = self.session_dir();
        let session_id = self.state.session_id.clone();
        let prior_runs = self.slim_run_history();
        let now = chrono::Utc::now();
        let yolo = self.state.modes.yolo;
        let cheap = self.state.modes.cheap;
        let mut phase_local = self.slim_phase;
        let stage_ctx = crate::lifecycle::StageCtx {
            session_id: session_id.as_str(),
            session_dir: session_dir.as_path(),
            phase: phase_local,
            prior_runs: prior_runs.as_slice(),
            pending_task_ids: &[],
            yolo,
            cheap,
            recovery_active: false,
            simplification_requested: false,
            dreaming_accepted: false,
        };
        let mut ctx = crate::lifecycle::OpsCtx {
            fsm: &mut self.fsm,
            phase: &mut phase_local,
            paused_at_phase: &mut self.paused_at_phase,
            pending_decisions: &mut self.pending_decisions,
            registry: self.scheduler.registry(),
            stage_ctx,
            now,
        };
        f(&mut ctx)
    }

    /// Apply the side effects of a [`crate::lifecycle::OpOutcome`] from any
    /// operator command (`:stop`, `:retry`, `:back`, tree-row rewind).
    ///
    /// 5a handled the [`crate::lifecycle::OpAction::PendingStop`] paths for
    /// `AfterStop::GoIdle` and `AfterStop::Restart`. 5b extends the surface
    /// to handle [`crate::lifecycle::OpAction::Immediate`] (for rewinds
    /// while the FSM is idle) and `AfterStop::Rewind` (for rewinds while an
    /// agent is live; the deferred cleanup/launch fires from
    /// `finalize_run_record` via [`Self::apply_after_stop_rewind`]).
    /// Cancel remains routed through the legacy `confirm_cancel_session`
    /// path until 5c migrates it.
    pub(crate) fn apply_op_outcome(
        &mut self,
        outcome: crate::lifecycle::OpOutcome,
        label: &'static str,
    ) {
        use crate::lifecycle::{AfterStop, OpAction, OpOutcome};
        match outcome {
            OpOutcome::NoOp(reason) => {
                self.push_status(reason, Severity::Info, Duration::from_secs(3));
            }
            OpOutcome::Staged(OpAction::Immediate {
                phase_change,
                cleanup,
                clear_paused,
                clear_pending,
                start_spec,
            }) => {
                self.apply_immediate_op_action(
                    label,
                    phase_change,
                    cleanup,
                    clear_paused,
                    clear_pending,
                    start_spec,
                );
            }
            OpOutcome::Staged(OpAction::PendingStop {
                after,
                cleanup,
                phase_change,
                clear_paused,
                clear_pending,
            }) => match after {
                AfterStop::GoIdle => self.apply_pending_stop_go_idle(label),
                AfterStop::Restart { .. } => self.apply_pending_stop_restart(label),
                AfterStop::Rewind { .. } => {
                    self.apply_pending_stop_rewind(
                        label,
                        cleanup,
                        phase_change,
                        clear_paused,
                        clear_pending,
                    );
                }
                AfterStop::Cancel => {
                    self.apply_pending_stop_cancel(label);
                }
            },
        }
    }

    /// Apply an [`crate::lifecycle::OpAction::Immediate`] plan synchronously.
    ///
    /// Used by operator rewinds (`:back`, tree-row Enter) when no agent is
    /// active. The cleanup is applied first so the launch dispatcher sees
    /// the post-rewind tree shape; the legacy [`crate::state::Phase`] is
    /// then driven through the canonical transition helper so any other
    /// observers stay in sync.
    fn apply_immediate_op_action(
        &mut self,
        label: &'static str,
        phase_change: Option<crate::lifecycle::Phase>,
        cleanup: crate::lifecycle::CleanupPlan,
        clear_paused: bool,
        clear_pending: bool,
        start_spec: Option<crate::lifecycle::StageSpec>,
    ) {
        // The in-line project-lane gate was removed; the shell scheduler's
        // per-tick lane-occupancy check
        // (`AppShell::apply_implementation_decision`) is now the single
        // throat for cross-session implementation-lane gating; the FSM
        // applies the rewind locally and a subsequent scheduler tick
        // returns `Blocked(ProjectLane)` if another session holds the
        // lane.
        // Apply file-level cleanup first so the launcher sees the post-
        // rewind tree shape.
        Self::apply_cleanup_plan(&cleanup);
        self.apply_legacy_phase_change(phase_change);
        if clear_paused {
            self.paused_at_phase = None;
        }
        if clear_pending {
            // Clear any pending operator decisions so they don't linger
            // across phase changes.
            self.pending_decisions = crate::lifecycle::PendingDecisions::default();
        }
        // The Immediate path means the FSM was Idle; reset the per-launch
        // mirror state the legacy code resets in retry.rs.
        self.clear_agent_error();
        self.current_run_id = None;
        self.run_launched = false;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        self.save_state();
        self.rebuild_tree_view(None);
        let _ = self.state.log_event(format!("lifecycle_op: {label} immediate"));
        if let Some(spec) = start_spec {
            // The FSM is still Idle at this point; clear paused_at_phase so
            // the legacy auto-launch dispatcher (driven by current_phase)
            // doesn't double-fire.
            self.dispatch_start(&spec);
        }
    }

    /// Apply an [`crate::lifecycle::AfterStop::Rewind`] resolution once the
    /// runner confirms the previously-running agent is dead.
    ///
    /// Mirror of the Immediate path with one difference: cancel_run was
    /// already issued at request-time via `apply_pending_stop_rewind`, so
    /// the runner side is already winding down. Cleanup happens here so the
    /// dead run's artifacts don't race the cleanup; the deferred launch
    /// dispatches once cleanup completes.
    pub(crate) fn apply_after_stop_rewind(
        &mut self,
        target: crate::lifecycle::Phase,
        spec: Option<crate::lifecycle::StageSpec>,
        cleanup: crate::lifecycle::CleanupPlan,
        clear_pending: bool,
    ) {
        // 8a removed the in-line lane gate that used to short-circuit the
        // post-stop rewind apply. The shell scheduler's per-tick lane gate
        // (`AppShell::apply_implementation_decision`) is authoritative for
        // cross-session implementation-lane occupancy; the rewind lands
        // here and a subsequent scheduler tick returns
        // `Blocked(ProjectLane)` if another session holds the lane.
        Self::apply_cleanup_plan(&cleanup);
        self.apply_legacy_phase_change(Some(target));
        self.paused_at_phase = None;
        if clear_pending {
            self.pending_decisions = crate::lifecycle::PendingDecisions::default();
        }
        self.clear_agent_error();
        self.current_run_id = None;
        self.run_launched = false;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        self.save_state();
        self.rebuild_tree_view(None);
        if let Some(spec) = spec {
            self.dispatch_start(&spec);
        }
    }

    /// Issue the runner cancel and stash the rewind plan as a
    /// `PendingRewindApply` so the deferred apply runs once the dead-run
    /// signal lands. The FSM carries the operator's intent inside
    /// `AfterStop::Rewind`; `finalize_run_record` reads it back via the
    /// `confirm_dead` resolution and merges into the parked apply.
    fn apply_pending_stop_rewind(
        &mut self,
        _label: &'static str,
        cleanup: crate::lifecycle::CleanupPlan,
        phase_change: Option<crate::lifecycle::Phase>,
        _clear_paused: bool,
        clear_pending: bool,
    ) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        let target = phase_change.unwrap_or(self.slim_phase);
        let marker = format!("lifecycle_op_rewind_requested: run_id={run_id}");
        if !self.marker_already_logged(&marker) {
            let _ = self.state.log_event(marker);
        }
        self.pending_quit_confirmation_run_id = None;
        // Park the rewind side effects on the App so finalize_run_record can
        // pick them up when the runner confirms the agent is dead. The slot
        // is per-App and only one rewind is in flight at a time (the FSM is
        // already in Stopping, which precludes additional starts).
        self.pending_rewind_apply = Some(PendingRewindApply {
            target,
            spec: None, // The FSM's confirm_dead carries the spec; see below.
            cleanup,
            clear_pending,
        });
        // The FSM holds the operator's spec inside AfterStop::Rewind. When
        // confirm_dead resolves, finalize_run_record reads the spec back out
        // of the resolution and merges it into the parked apply.
        self.runner_supervisor.cancel_run(run_id);
        self.push_status(
            "Rewinding session...".to_string(),
            Severity::Warn,
            Duration::from_secs(5),
        );
    }

    /// Run an `OpsCtx` closure and apply its [`crate::lifecycle::OpOutcome`]
    /// uniformly. Used by all operator commands — `:stop`, `:retry`, `:back`,
    /// tree-row rewinds — so the apply path is one throat.
    pub(crate) fn run_lifecycle_op<F>(&mut self, label: &'static str, op: F)
    where
        F: FnOnce(&mut crate::lifecycle::OpsCtx<'_>) -> crate::lifecycle::OpOutcome,
    {
        self.ensure_fsm_running_mirror();
        let outcome = self.with_running_agent_ops_ctx(op);
        self.apply_op_outcome(outcome, label);
    }

    /// Apply a [`crate::lifecycle::CleanupPlan`] to disk.
    ///
    /// Mirrors the legacy `go_back` semantics: missing files / directories
    /// are not an error (rewind is best-effort cleanup); `restore_backups`
    /// move a backup to its destination only when the backup actually
    /// exists, then remove the backup so a second rewind doesn't replay it.
    fn apply_cleanup_plan(plan: &crate::lifecycle::CleanupPlan) {
        for path in &plan.delete {
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
        for (backup, dest) in &plan.restore_backups {
            if backup.exists() && std::fs::copy(backup, dest).is_ok() {
                let _ = std::fs::remove_file(backup);
            }
        }
    }

    /// Drive the legacy [`crate::state::Phase`] to the variant matching the
    /// rewind target and refresh the slim-phase mirror.
    ///
    /// Rewinds frequently cross the phase-graph's forward-only edges (e.g.
    /// Implementation(r) → Idea on the skip-to-impl path), so the validated
    /// [`Self::transition_to_phase`] would silently reject them. The legacy
    /// `go_back` lived with that — it just discarded the error. 5b uses the
    /// unchecked [`session_state::set_phase_for_operator_retry`] so the
    /// rewind actually lands, then triggers the same downstream effects
    /// `transition_to_phase` would have fired.
    fn apply_legacy_phase_change(&mut self, target: Option<crate::lifecycle::Phase>) {
        let Some(target) = target else {
            return;
        };
        let legacy = crate::lifecycle::slim_to_old_phase(target);
        let leaving_blocked = matches!(self.state.current_phase, Phase::BlockedNeedsUser)
            && !matches!(legacy, Phase::BlockedNeedsUser);
        session_state::set_phase_for_operator_retry(&mut self.state, legacy);
        if leaving_blocked {
            // Mirror the `execute_transition` invariant: leaving
            // BlockedNeedsUser clears the block_origin so a subsequent
            // re-block records a fresh provenance. Required for the cancel
            // path from a final-validation block — the lane gate would
            // otherwise let stale provenance satisfy the force-ship guard.
            self.state.block_origin = None;
        }
        self.refresh_slim_phase();
        self.agent_line_count = 0;
        self.rebuild_tree_view(None);
        self.enable_progress_follow_and_refocus();
        self.set_follow_tail(true);
        self.maybe_emit_phase_notification(legacy);
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        // The validated path captures the round base at Implementation(r)
        // entry; mirror that so the simplifier still sees a fresh base_sha.
        if let crate::state::Phase::ImplementationRound(round) = legacy {
            let round_dir = self
                .session_dir()
                .join("rounds")
                .join(format!("{round:03}"));
            self.capture_round_base(&round_dir);
        }
    }

    /// Bridge a [`crate::lifecycle::StageSpec`] to the matching legacy
    /// `launch_X` entry point. Called by [`Self::apply_op_outcome`] (for
    /// rewind / restart launches) and by [`Self::maybe_auto_launch`] (for
    /// the per-tick scheduler dispatch).
    ///
    /// Consumes `App::next_run_model_override` (the model-fallback chain's
    /// pinned next-attempt model) when set, handing it to the matching
    /// `launch_*_with_model` entry point. Stages whose model picker is
    /// non-overridable (`Planning` with the round-fallback flag, the
    /// Sharding deferral) simply ignore the override; the slot is cleared
    /// unconditionally so a stale value can't bleed into the next launch.
    ///
    /// 5d will make `launch_*` take the spec as a parameter; until then we
    /// route by `stage_id`.
    pub(crate) fn dispatch_start(&mut self, spec: &crate::lifecycle::StageSpec) {
        use crate::lifecycle::StageId as L;
        let override_model = self.next_run_model_override.take();
        match spec.stage_id {
            L::Brainstorm => {
                let idea = self.state.idea_text.clone().unwrap_or_default();
                let _ = self.launch_brainstorm_with_model(idea, override_model);
            }
            L::SpecReview => {
                let _ = self.launch_spec_review_with_model(override_model);
            }
            L::Planning => {
                let _ = self.launch_planning_with_model(override_model, true);
            }
            L::PlanReview => {
                let _ = self.launch_plan_review_with_model(override_model);
            }
            L::Sharding => {
                // The slim `Phase::Plan` covers PlanningRunning..ShardingRunning;
                // operator rewinds that land on Plan must defer Sharding to the
                // shell scheduler so `decide_waiting_dispatch` can re-verify the
                // baseline. The auto-launch path only fires this branch when
                // legacy `current_phase == ShardingRunning`, which already
                // encodes a passed baseline check.
                if matches!(self.state.current_phase, crate::state::Phase::ShardingRunning) {
                    let _ = self.launch_sharding_with_model(override_model);
                }
            }
            L::Coder => {
                let _ = self.launch_coder_with_model(override_model);
            }
            L::Reviewer => {
                let _ = self.launch_reviewer_with_model(override_model);
            }
            L::Recovery => {
                let _ = self.launch_recovery_with_model(override_model);
            }
            L::RecoveryPlanReview => {
                let _ = self.launch_recovery_plan_review_with_model(override_model);
            }
            L::RecoverySharding => {
                let _ = self.launch_recovery_sharding_with_model(override_model);
            }
            L::FinalValidation => {
                let _ = self.launch_final_validation_with_model(override_model);
            }
            L::Simplification => {
                let _ = self.launch_simplifier_with_model(override_model);
            }
            L::Dreaming => {
                let _ = self.launch_dreaming_with_model(override_model);
            }
            L::RepoStateUpdate => {
                let _ = self.launch_repo_state_update_with_model(override_model);
            }
        }
    }

    /// Apply [`crate::lifecycle::AfterStop::Cancel`] when the operator
    /// cancels a session while an agent is live.
    ///
    /// The FSM is already in `Stopping(Cancel)`; the runner supervisor still
    /// needs an explicit cancel so the agent process terminates. The legacy
    /// `current_phase` is left untouched here — `complete_run_finalization`
    /// observes the FSM state once `confirm_dead` lands and drives the
    /// transition to [`Phase::Cancelled`] from there.
    fn apply_pending_stop_cancel(&mut self, _label: &'static str) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        let marker = format!("session_cancel_requested_by_user: run_id={run_id}");
        if !self.marker_already_logged(&marker) {
            let _ = self.state.log_event(marker);
        }
        self.pending_quit_confirmation_run_id = None;
        self.pending_cancel_confirmation = false;
        self.runner_supervisor.cancel_run(run_id);
        self.push_status(
            "Cancelling session...".to_string(),
            Severity::Warn,
            Duration::from_secs(5),
        );
    }

    fn apply_pending_stop_go_idle(&mut self, _label: &'static str) {
        // The FSM is already in `Stopping(GoIdle)`; the runner needs an
        // explicit cancel so the agent process dies, and the failure-path
        // in `handle_run_finalization_failure` reads the FSM state to
        // decide what to do after the run finalizes.
        let Some(run_id) = self.current_run_id else {
            return;
        };
        let marker = format!("agent_stopped_by_user: run_id={run_id}");
        if !self.marker_already_logged(&marker) {
            let _ = self.state.log_event(marker);
        }
        self.pending_quit_confirmation_run_id = None;
        self.runner_supervisor.cancel_run(run_id);
        self.push_status(
            "Stopping agent...".to_string(),
            Severity::Warn,
            Duration::from_secs(5),
        );
    }

    fn apply_pending_stop_restart(&mut self, _label: &'static str) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        // Validate that the run is retryable before issuing the cancel; the
        // post-finalize handler reads the FSM state to know to relaunch but
        // can't tell the operator their stage is unretryable from there.
        // The lifecycle stage registry is the source of truth: a stage is
        // restartable iff its [`crate::lifecycle::Stage::supports_restart`]
        // returns `true`. Falls back to "not restartable" when the run's
        // stage string doesn't map to a registered StageId.
        let stage_supports_restart = self
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == run_id)
            .and_then(|run| crate::lifecycle::stage_id_for_run(&run.stage, &run.window_name))
            .and_then(|id| self.scheduler.registry().get(id))
            .is_some_and(|stage| stage.supports_restart());
        if !stage_supports_restart {
            self.push_status(
                "restart: current run is not restartable".to_string(),
                Severity::Warn,
                Duration::from_secs(3),
            );
            return;
        }
        let marker = format!("agent_retry_requested_by_user: run_id={run_id}");
        if !self.marker_already_logged(&marker) {
            let _ = self.state.log_event(marker);
        }
        self.pending_quit_confirmation_run_id = None;
        self.runner_supervisor.cancel_run(run_id);
        self.push_status(
            "Stopping agent and queuing restart...".to_string(),
            Severity::Warn,
            Duration::from_secs(5),
        );
    }

    /// Synchronize the lifecycle FSM into [`AgentState::Running`] for the
    /// current legacy `Running` agent, if any.
    ///
    /// 5a leaves the legacy launch path (start_run_tracking) authoritative,
    /// so the FSM is still Idle from construction when an operator hits
    /// `:stop` after a TUI restart (resume) or in tests that synthesize a
    /// Running `RunRecord` without going through launch. This helper
    /// reconciles by synthesizing a [`StageSpec`] / [`ActiveRun`] from the
    /// legacy `RunRecord` and pushing the FSM through `start` +
    /// `confirm_running`. Already-Running and Stopping states are no-ops.
    /// All FSM errors are absorbed: the mirroring shim is best-effort
    /// until 5b/5c make it authoritative.
    pub(crate) fn ensure_fsm_running_mirror(&mut self) {
        use crate::lifecycle::AgentState;
        if !matches!(self.fsm.view(), AgentState::Idle) {
            return;
        }
        let Some(run) = self.running_run().cloned().or_else(|| {
            self.state
                .agent_runs
                .iter()
                .find(|r| r.status == RunStatus::Running)
                .cloned()
        }) else {
            return;
        };
        let Some(stage_id) = crate::lifecycle::stage_id_for_run(&run.stage, &run.window_name)
        else {
            tracing::warn!(
                "ensure_fsm_running_mirror: unknown stage '{}' / window '{}'",
                run.stage,
                run.window_name
            );
            return;
        };
        let spec = crate::lifecycle::StageSpec {
            stage_id,
            round: run.round,
            task_id: run.task_id,
            attempt: run.attempt,
            window_name: run.window_name.clone(),
        };
        if self.fsm_start_mirroring(spec.clone()).is_err() {
            return;
        }
        let active = crate::lifecycle::ActiveRun {
            run_id: run.id,
            spec,
            started_at: run.started_at,
        };
        let _ = self.fsm_confirm_running_mirroring(active);
    }

    /// Mirror [`crate::lifecycle::Fsm::start`] into legacy fields.
    ///
    /// Does not yet set `current_run_id` — the run id isn't known until
    /// `confirm_running`. Used by the launch-time mirroring shim
    /// installed in `start_run_tracking`. Errors from the FSM are logged
    /// and discarded; the legacy path is authoritative for persistence,
    /// so a misordered FSM transition isn't fatal.
    pub(crate) fn fsm_start_mirroring(
        &mut self,
        spec: crate::lifecycle::StageSpec,
    ) -> Result<(), crate::lifecycle::FsmError> {
        match self.fsm.start(spec) {
            Ok(()) => Ok(()),
            Err(err) => {
                tracing::warn!(
                    "fsm_start_mirroring: legal path desync, FSM rejected start ({err:?})"
                );
                Err(err)
            }
        }
    }

    /// Mirror [`crate::lifecycle::Fsm::confirm_running`] and sync the
    /// legacy `current_run_id` field. Errors are logged and discarded.
    pub(crate) fn fsm_confirm_running_mirroring(
        &mut self,
        run: crate::lifecycle::ActiveRun,
    ) -> Result<(), crate::lifecycle::FsmError> {
        let run_id = run.run_id;
        match self.fsm.confirm_running(run) {
            Ok(()) => {
                self.current_run_id = Some(run_id);
                Ok(())
            }
            Err(err) => {
                tracing::warn!(
                    "fsm_confirm_running_mirroring: legal path desync ({err:?}); \
                     keeping legacy current_run_id"
                );
                Err(err)
            }
        }
    }

    /// Mirror [`crate::lifecycle::Fsm::confirm_dead`] and clear the legacy
    /// `current_run_id` / `run_launched` fields.
    pub(crate) fn fsm_confirm_dead_mirroring(
        &mut self,
        outcome: crate::lifecycle::Outcome,
    ) -> Result<crate::lifecycle::StopResolution, crate::lifecycle::FsmError> {
        match self.fsm.confirm_dead(outcome) {
            Ok(resolution) => {
                self.current_run_id = None;
                self.run_launched = false;
                Ok(resolution)
            }
            Err(err) => {
                tracing::warn!(
                    "fsm_confirm_dead_mirroring: legal path desync ({err:?}); \
                     leaving legacy current_run_id unchanged"
                );
                Err(err)
            }
        }
    }

    pub(crate) fn transition_to_phase(&mut self, next_phase: Phase) -> Result<()> {
        session_state::execute_transition(&mut self.state, next_phase)?;
        self.refresh_slim_phase();
        // Pin the round's review scope at round entry — including the
        // skip-to-impl path that has no reviewer stage and goal-gap re-runs
        // that create a fresh implementation round — so the simplifier can
        // consistently use `base_sha..HEAD`. `capture_round_base` is
        // idempotent on resume.
        if let Phase::ImplementationRound(round) = next_phase {
            let round_dir = self
                .session_dir()
                .join("rounds")
                .join(format!("{round:03}"));
            self.capture_round_base(&round_dir);
        }
        self.agent_line_count = 0;
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
        // Notify before clearing the live-summary cache so the outgoing
        // phase's last "what I was doing" line lands in the body of the
        // ntfy push — that's the answer to "what just happened?" for a
        // review pause or a final pipeline-done.
        self.maybe_emit_phase_notification(next_phase);
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
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
        session_state::record_agent_error(&mut self.state, message);
    }
    pub(crate) fn clear_agent_error(&mut self) {
        session_state::clear_agent_error(&mut self.state);
    }
    pub(crate) fn clear_builder_recovery_context(&mut self) {
        session_state::clear_builder_recovery_context(&mut self.state);
    }
    pub fn accept_skip_to_implementation(&mut self) -> Result<()> {
        use crate::artifacts::SkipToImplKind;
        use crate::synthetic_artifacts::generate_synthetic_artifacts;
        use anyhow::Context;
        let session_dir = self.session_dir();
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
        session_state::initialize_task_pipeline(
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
        session_state::clear_skip_to_impl_proposal(&mut self.state);
        let target = if kind == Some(SkipToImplKind::NothingToDo) {
            Phase::BrainstormRunning
        } else {
            Phase::SpecReviewRunning
        };
        self.transition_to_phase(target)?;
        self.state.save()?;
        Ok(())
    }
    pub fn skip_suggested_dreaming(&mut self) -> Result<()> {
        let mut decision = self
            .state
            .dreaming_decision
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing pending dreaming decision"))?;
        decision.kind = DreamingDecisionKind::OperatorSkipped;
        self.state.dreaming_decision = Some(decision);
        self.clear_agent_error();
        self.state.save()?;
        self.transition_to_phase(Phase::Done)?;
        Ok(())
    }
    pub fn accept_suggested_dreaming(&mut self) -> Result<()> {
        let mut decision = self
            .state
            .dreaming_decision
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing pending dreaming decision"))?;
        decision.kind = DreamingDecisionKind::OperatorAccepted;
        let round = decision.round;
        self.state.dreaming_decision = Some(decision);
        self.state.save()?;
        self.transition_to_phase(Phase::Dreaming(round))?;
        Ok(())
    }
    pub fn accept_guard_reset(&mut self) -> Result<()> {
        let decision =
            session_state::take_pending_guard_decision(&mut self.state, "accept_guard_reset")?;
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
        self.save_state();
        self.complete_run_finalization(&run, Some(Reason::ForbiddenHeadAdvance.to_string()))
    }
    pub fn accept_guard_keep(&mut self) -> Result<()> {
        let decision =
            session_state::take_pending_guard_decision(&mut self.state, "accept_guard_keep")?;
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
        self.save_state();
        // Artifact was valid (PendingDecision only fires on valid-artifact path).
        // complete_run_finalization dispatches on current_phase; restore the
        // originating running phase so the correct success successor fires.
        let originating = match decision.stage.as_str() {
            "brainstorm" => Phase::BrainstormRunning,
            "planning" => Phase::PlanningRunning,
            "recovery" => Phase::BuilderRecovery(decision.round),
            other => anyhow::bail!("accept_guard_keep: unexpected stage '{other}'"),
        };
        session_state::restore_guard_originating_phase(&mut self.state, originating);
        self.refresh_slim_phase();
        self.complete_run_finalization(&run, None)
    }
    pub(crate) fn editable_artifact(&self) -> Option<std::path::PathBuf> {
        let session_dir = self.session_dir();
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
            | Phase::WaitingToImplement
            | Phase::RepoStateUpdateRunning
            | Phase::SkipToImplPending
            | Phase::GitGuardPending
            | Phase::FinalValidation(_)
            | Phase::DreamingPending
            | Phase::Dreaming(_)
            | Phase::Simplification(_)
            | Phase::Cancelled => {
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
        let path = self.session_dir().join("artifacts").join(filename);
        if path.exists() {
            self.pending_view_path = Some(path);
        }
    }
    pub(crate) fn can_go_back(&self) -> bool {
        !matches!(self.state.current_phase, Phase::IdeaInput | Phase::Done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_keeps_backup_when_restore_copy_fails() {
        let temp = tempfile::TempDir::new().unwrap();
        let backup = temp.path().join("plan.pre-review-1.md");
        let dest = temp.path().join("missing-parent").join("plan.md");
        std::fs::write(&backup, "reviewed plan").unwrap();

        App::apply_cleanup_plan(&crate::lifecycle::CleanupPlan {
            delete: Vec::new(),
            restore_backups: vec![(backup.clone(), dest)],
        });

        assert!(backup.exists(), "failed restore must not delete backup");
    }
}
