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
    data::artifacts::{ArtifactKind, Spec},
    state::{
        self as session_state, BlockOrigin, DreamingDecisionKind, MessageKind, RunStatus, Stage,
    },
    tasks,
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

/// Owned App-side snapshot used to build a borrowed lifecycle [`StageCtx`].
///
/// The App still carries the persisted [`crate::state::Stage`] as persisted
/// authority. This projection is the single place where that persisted shape is
/// translated into the lifecycle inputs a stage scheduler needs.
#[derive(Debug)]
pub(crate) struct LifecycleStageProjection {
    session_id: String,
    session_dir: std::path::PathBuf,
    stage: crate::lifecycle::Stage,
    prior_runs: Vec<crate::lifecycle::RunHistoryEntry>,
    pending_task_ids: Vec<u32>,
    yolo: bool,
    cheap: bool,
    gates: LifecycleStageGates,
}

impl LifecycleStageProjection {
    fn stage_ctx(&self) -> crate::lifecycle::StageCtx<'_> {
        crate::lifecycle::StageCtx {
            session_id: self.session_id.as_str(),
            session_dir: self.session_dir.as_path(),
            stage: self.stage,
            prior_runs: self.prior_runs.as_slice(),
            pending_task_ids: self.pending_task_ids.as_slice(),
            yolo: self.yolo,
            cheap: self.cheap,
            recovery_active: self.gates.recovery_active,
            simplification_requested: self.gates.simplification_requested,
            dreaming_accepted: self.gates.dreaming_accepted,
        }
    }
}

/// Stage-selection gates derived from persisted session state for lifecycle
/// scheduler projection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LifecycleStageGates {
    recovery_active: bool,
    simplification_requested: bool,
    dreaming_accepted: bool,
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

/// Operator-visible stage-error target for a persisted running stage.
///
/// The persisted persisted stage is still the authority for "what just failed";
/// this projection keeps the stage-error modal and its retry action from
/// guessing across the lifecycle's broader stages.
fn stage_error_target_for_stage(stage: Stage) -> Option<StageId> {
    Some(match stage {
        Stage::BrainstormRunning => StageId::Brainstorm,
        Stage::SpecReviewRunning => StageId::SpecReview,
        Stage::PlanningRunning => StageId::Planning,
        Stage::PlanReviewRunning => StageId::PlanReview,
        Stage::RepoStateUpdateRunning => StageId::RepoStateUpdate,
        Stage::ShardingRunning => StageId::Sharding,
        Stage::ImplementationRound(_) => StageId::Implementation,
        Stage::BuilderRecovery(_) => StageId::Recovery,
        Stage::BuilderRecoveryPlanReview(_) => StageId::RecoveryPlanReview,
        Stage::BuilderRecoverySharding(_) => StageId::RecoverySharding,
        Stage::ReviewRound(_) => StageId::Review,
        Stage::Simplification(_) => StageId::Simplification,
        Stage::FinalValidation(_) => StageId::FinalValidation,
        Stage::Dreaming(_) => StageId::Dreaming,
        Stage::IdeaInput
        | Stage::SpecReviewPaused
        | Stage::PlanReviewPaused
        | Stage::WaitingToImplement
        | Stage::SkipToImplPending
        | Stage::GitGuardPending
        | Stage::DreamingPending
        | Stage::Done
        | Stage::Cancelled
        | Stage::BlockedNeedsUser => return None,
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
                StageId::RepoStateUpdate => RuntimeStageId::RepoStateUpdate,
                StageId::Sharding => RuntimeStageId::Sharding,
                StageId::Implementation => RuntimeStageId::Implementation,
                StageId::Recovery => RuntimeStageId::Recovery,
                StageId::RecoveryPlanReview => RuntimeStageId::RecoveryPlanReview,
                StageId::RecoverySharding => RuntimeStageId::RecoverySharding,
                StageId::Review => RuntimeStageId::Review,
                StageId::Simplification => RuntimeStageId::Simplification,
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
                ModalKind::StageError(id) => RuntimeModalKind::StageError(stage_id(id)),
                ModalKind::FinalValidationBlocked => RuntimeModalKind::FinalValidationBlocked,
                ModalKind::DreamingDecision => RuntimeModalKind::DreamingDecision,
            }
        }
        AppView {
            session_id: Arc::from(self.state.session_id.as_str()),
            stage: self.state.current_stage,
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
            config_panel: self
                .config_panel
                .as_ref()
                .map(|p| p.current_view())
                .unwrap_or_default(),
        }
    }

    pub(crate) fn current_session_view(&self) -> crate::app_runtime::SessionView {
        use crate::app_runtime::views::*;
        use std::sync::Arc;

        SessionView {
            tree: TreeView {
                rows: Arc::from(
                    self.visible_rows
                        .iter()
                        .map(|r| tree::VisibleNodeRow {
                            depth: r.depth,
                            label: Arc::from(format!("{:?}", r.kind)), // Placeholder
                            status: match r.status {
                                crate::state::NodeStatus::Pending => tree::TreeNodeStatus::Pending,
                                crate::state::NodeStatus::Running => tree::TreeNodeStatus::Running,
                                crate::state::NodeStatus::Done => tree::TreeNodeStatus::Success,
                                crate::state::NodeStatus::Failed => tree::TreeNodeStatus::Failure,
                                crate::state::NodeStatus::Skipped => tree::TreeNodeStatus::Skipped,
                                crate::state::NodeStatus::WaitingUser => {
                                    tree::TreeNodeStatus::Pending
                                }
                                _ => tree::TreeNodeStatus::Pending,
                            },
                            has_children: r.has_children,
                            is_expanded: false, // Not available on VisibleNodeRow directly
                            run_id: r.backing_leaf_run_id,
                        })
                        .collect::<Vec<_>>()
                        .as_slice(),
                ),
                selected_index: Some(self.selected),
            },
            chat: ChatView {
                messages: Arc::from(
                    self.messages
                        .iter()
                        .map(|m| chat::ChatMessage {
                            kind: match m.kind {
                                crate::state::MessageKind::Started => {
                                    chat::ChatMessageKind::Started
                                }
                                crate::state::MessageKind::Brief => chat::ChatMessageKind::Brief,
                                crate::state::MessageKind::UserInput => {
                                    chat::ChatMessageKind::UserInput
                                }
                                crate::state::MessageKind::AgentText => {
                                    chat::ChatMessageKind::AgentText
                                }
                                crate::state::MessageKind::AgentThought => {
                                    chat::ChatMessageKind::AgentThought
                                }
                                crate::state::MessageKind::Summary => {
                                    chat::ChatMessageKind::Summary
                                }
                                crate::state::MessageKind::SummaryWarn => {
                                    chat::ChatMessageKind::SummaryWarn
                                }
                                crate::state::MessageKind::End => chat::ChatMessageKind::End,
                            },
                            content: Arc::from(m.text.as_str()),
                            timestamp: Arc::from(m.ts.to_rfc3339()),
                        })
                        .collect::<Vec<_>>()
                        .as_slice(),
                ),
                scroll: chat::ChatScrollWindow::default(),
                follow_tail: self.split_follow_tail,
            },
            modal: self.active_modal().map(|m| match m {
                crate::app::ModalKind::SkipToImpl => modal::ModalKind::SkipToImpl,
                crate::app::ModalKind::GitGuard => modal::ModalKind::GitGuard,
                crate::app::ModalKind::QuitRunningAgent => modal::ModalKind::QuitRunningAgent,
                crate::app::ModalKind::CancelSession => modal::ModalKind::CancelSession,
                crate::app::ModalKind::InteractiveExitPrompt => {
                    modal::ModalKind::InteractiveExitPrompt
                }
                crate::app::ModalKind::SpecReviewPaused => modal::ModalKind::SpecReviewPaused,
                crate::app::ModalKind::PlanReviewPaused => modal::ModalKind::PlanReviewPaused,
                crate::app::ModalKind::StageError(id) => modal::ModalKind::StageError(match id {
                    crate::app::StageId::Brainstorm => modal::StageId::Brainstorm,
                    crate::app::StageId::SpecReview => modal::StageId::SpecReview,
                    crate::app::StageId::Planning => modal::StageId::Planning,
                    crate::app::StageId::PlanReview => modal::StageId::PlanReview,
                    crate::app::StageId::RepoStateUpdate => modal::StageId::RepoStateUpdate,
                    crate::app::StageId::Sharding => modal::StageId::Sharding,
                    crate::app::StageId::Implementation => modal::StageId::Implementation,
                    crate::app::StageId::Recovery => modal::StageId::Recovery,
                    crate::app::StageId::RecoveryPlanReview => modal::StageId::RecoveryPlanReview,
                    crate::app::StageId::RecoverySharding => modal::StageId::RecoverySharding,
                    crate::app::StageId::Review => modal::StageId::Review,
                    crate::app::StageId::Simplification => modal::StageId::Simplification,
                    crate::app::StageId::FinalValidation => modal::StageId::FinalValidation,
                    crate::app::StageId::Dreaming => modal::StageId::Dreaming,
                }),
                crate::app::ModalKind::FinalValidationBlocked => {
                    modal::ModalKind::FinalValidationBlocked
                }
                crate::app::ModalKind::DreamingDecision => modal::ModalKind::DreamingDecision,
            }),
            agent_runs: Arc::from(
                self.state
                    .agent_runs
                    .iter()
                    .map(session::AgentRunSummary::from_record)
                    .collect::<Vec<_>>(),
            ),
            modes: session::ModeFlags {
                yolo: self.state.modes.yolo,
                cheap: self.state.modes.cheap,
            },
            stage: self.state.current_stage,
            status: self.status_line.borrow().current_message().map(|m| {
                status_line::StatusMessage {
                    text: Arc::from(m.text.as_str()),
                    severity: match m.severity {
                        crate::app::status_line::Severity::Info => {
                            status_line::StatusSeverity::Info
                        }
                        crate::app::status_line::Severity::Warn => {
                            status_line::StatusSeverity::Warn
                        }
                        crate::app::status_line::Severity::Error => {
                            status_line::StatusSeverity::Error
                        }
                    },
                }
            }),
            ..Default::default()
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
        match self.state.current_stage {
            Stage::SkipToImplPending => Some(ModalKind::SkipToImpl),
            Stage::GitGuardPending => Some(ModalKind::GitGuard),
            Stage::SpecReviewPaused => Some(ModalKind::SpecReviewPaused),
            Stage::PlanReviewPaused => Some(ModalKind::PlanReviewPaused),
            Stage::DreamingPending
                if self
                    .state
                    .dreaming_decision
                    .as_ref()
                    .is_some_and(|decision| decision.kind == DreamingDecisionKind::Pending) =>
            {
                Some(ModalKind::DreamingDecision)
            }
            stage if self.state.agent_error.is_some() => {
                stage_error_target_for_stage(stage).map(ModalKind::StageError)
            }
            Stage::BlockedNeedsUser
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
        path: &std::path::Path,
        run_foreground: impl FnOnce(&mut dyn FnMut()),
    ) {
        let banner_inserted = prepend_review_banner(path);
        let mut run_editor = || {
            let _ = std::process::Command::new(
                std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string()),
            )
            .arg(path)
            .status();
        };
        run_foreground(&mut run_editor);
        if banner_inserted {
            let _ = strip_review_banner(path);
        }
    }
    /// Pre-data-drain stage of the per-tick coordination. The runtime calls
    /// this, then drains and routes any [`DataEvent`](crate::data::events::DataEvent)s,
    /// then finishes the tick via [`Self::runtime_tick_after_data_drain`].
    pub(crate) fn runtime_tick_before_data_drain(&mut self) -> bool {
        self.refresh_models_if_due();
        self.poll_agent_run();
        if self.pending_app_exit {
            self.runner_supervisor.shutdown_all_runs();
            return true;
        }
        self.maybe_yolo_auto_resolve();
        self.maybe_auto_launch();
        self.update_agent_progress();
        false
    }
    /// Post-data-drain stage: watchdog evaluation and split-target sync after
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
    /// Recompute `App::lifecycle_stage` from `state.current_stage`.
    ///
    /// The lifecycle stage is a derived projection — it never lives on disk and
    /// must be refreshed every time the persisted stage mutates. This helper is
    /// the single refresh point so the derived lifecycle stage stays in sync with
    /// the authoritative persisted stage.
    pub(crate) fn refresh_lifecycle_stage(&mut self) {
        self.lifecycle_stage = self.state.current_stage.to_lifecycle_stage();
    }

    /// Project the App's agent-run history into the lifecycle
    /// [`crate::lifecycle::RunHistoryEntry`] shape `LifecycleOps` consumes.
    /// Outcomes are best-effort approximations of the persisted `RunStatus`
    /// values. Used by restart's `build_spec` lookup, which doesn't inspect
    /// the outcome variant.
    pub(crate) fn lifecycle_run_history(&self) -> Vec<crate::lifecycle::RunHistoryEntry> {
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
                    RunStatus::FailedUnverified => {
                        Some(crate::lifecycle::Outcome::FailedUnverified(
                            run.error.clone().unwrap_or_default(),
                        ))
                    }
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

    /// Task ids visible to the lifecycle scheduler for `stage`.
    ///
    /// The persisted builder queue has two different task concepts:
    /// - implementation launches pick from the current running task first
    ///   (failed/retry path), then selectable pending/revise tasks;
    /// - review launches operate on the current running task only.
    ///
    /// Keeping that projection here prevents each lifecycle call site from
    /// guessing which slice to feed into [`crate::lifecycle::StageCtx`].
    fn lifecycle_task_ids_for_stage(&self, stage: crate::lifecycle::Stage) -> Vec<u32> {
        match stage {
            crate::lifecycle::Stage::Implementation(_) => {
                let mut task_ids = Vec::new();
                if let Some(task_id) = self.state.builder.current_task_id() {
                    task_ids.push(task_id);
                }
                for task_id in self.state.builder.pending_task_ids() {
                    if !task_ids.contains(&task_id) {
                        task_ids.push(task_id);
                    }
                }
                task_ids
            }
            crate::lifecycle::Stage::Review(_) => {
                self.state.builder.current_task_id().into_iter().collect()
            }
            _ => Vec::new(),
        }
    }

    pub(crate) fn with_lifecycle_stage_ctx<R>(
        &self,
        stage: crate::lifecycle::Stage,
        f: impl FnOnce(crate::lifecycle::StageCtx<'_>) -> R,
    ) -> R {
        let projection = self.lifecycle_stage_projection(stage);
        f(projection.stage_ctx())
    }

    pub(crate) fn lifecycle_stage_projection(
        &self,
        stage: crate::lifecycle::Stage,
    ) -> LifecycleStageProjection {
        LifecycleStageProjection {
            session_id: self.state.session_id.clone(),
            session_dir: self.session_dir(),
            stage,
            prior_runs: self.lifecycle_run_history(),
            pending_task_ids: self.lifecycle_task_ids_for_stage(stage),
            yolo: self.state.modes.yolo,
            cheap: self.state.modes.cheap,
            gates: self.lifecycle_stage_gates(),
        }
    }

    fn lifecycle_stage_gates(&self) -> LifecycleStageGates {
        LifecycleStageGates {
            recovery_active: matches!(
                self.state.current_stage,
                Stage::BuilderRecovery(_)
                    | Stage::BuilderRecoveryPlanReview(_)
                    | Stage::BuilderRecoverySharding(_)
            ),
            simplification_requested: matches!(self.state.current_stage, Stage::Simplification(_)),
            dreaming_accepted: matches!(self.state.current_stage, Stage::Dreaming(_)),
        }
    }

    /// Build an [`crate::lifecycle::OpsCtx`] in-place and hand it to `f`.
    ///
    /// OpsCtx borrows multiple App fields disjointly which is awkward to
    /// express as a returned struct. The closure form keeps the borrow
    /// scope tight and lets the caller invoke any of the four
    /// [`crate::lifecycle::LifecycleOps`] members inline. Snapshots
    /// `prior_runs` to a `Vec` and `session_dir` to a `PathBuf` before the
    /// borrow because they thread through `StageCtx` as references.
    pub(crate) fn with_lifecycle_ops_ctx<R>(
        &mut self,
        f: impl FnOnce(&mut crate::lifecycle::OpsCtx<'_>) -> R,
    ) -> R {
        let projection = self.lifecycle_stage_projection(self.lifecycle_stage);
        let now = chrono::Utc::now();
        let mut stage_local = projection.stage;
        let stage_ctx = projection.stage_ctx();
        let mut ctx = crate::lifecycle::OpsCtx {
            fsm: &mut self.fsm,
            stage: &mut stage_local,
            paused_at_stage: &mut self.paused_at_stage,
            pending_decisions: &mut self.pending_decisions,
            registry: self.scheduler.registry(),
            stage_ctx,
            now,
        };
        f(&mut ctx)
    }

    /// Apply the side effects of a [`crate::lifecycle::OpOutcome`] from any
    /// operator command (`:stop`, `:retry`, `:back`, tree-row rewind).
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
                stage_change,
                cleanup,
                clear_paused,
                clear_pending,
                start_spec,
            }) => {
                self.apply_immediate_op_action(
                    label,
                    stage_change,
                    cleanup,
                    clear_paused,
                    clear_pending,
                    start_spec,
                );
            }
            OpOutcome::Staged(OpAction::PendingStop {
                after,
                cleanup,
                stage_change,
                clear_paused,
                clear_pending,
            }) => match after {
                AfterStop::GoIdle => self.apply_pending_stop_go_idle(label),
                AfterStop::Restart { .. } => self.apply_pending_stop_restart(label),
                AfterStop::Rewind { .. } => {
                    self.apply_pending_stop_rewind(
                        label,
                        cleanup,
                        stage_change,
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
    /// the post-rewind tree shape; the persisted [`crate::state::Stage`] is
    /// then driven through the canonical transition helper so any other
    /// observers stay in sync.
    fn apply_immediate_op_action(
        &mut self,
        label: &'static str,
        stage_change: Option<crate::lifecycle::Stage>,
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
        self.apply_stage_change(stage_change);
        if clear_paused {
            self.paused_at_stage = None;
        }
        if clear_pending {
            // Clear any pending operator decisions so they don't linger
            // across stage changes.
            self.pending_decisions = crate::lifecycle::PendingDecisions::default();
        }
        // The Immediate path means the FSM was Idle; reset the per-launch
        // mirror state the persisted code resets in retry.rs.
        self.clear_agent_error();
        self.current_run_id = None;
        self.run_launched = false;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        self.save_state();
        self.rebuild_tree_view(None);
        let _ = self
            .state
            .log_event(format!("lifecycle_op: {label} immediate"));
        if let Some(spec) = start_spec {
            // The FSM is still Idle at this point; clear paused_at_stage so
            // the persisted auto-launch dispatcher (driven by current_stage)
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
        target: crate::lifecycle::Stage,
        spec: Option<crate::lifecycle::StageSpec>,
        cleanup: crate::lifecycle::CleanupPlan,
        clear_pending: bool,
    ) {
        // The shell scheduler's per-tick lane gate
        // (`AppShell::apply_implementation_decision`) is authoritative for
        // cross-session implementation-lane occupancy; this rewind lands
        // locally, and the scheduler reports `Blocked(ProjectLane)` if
        // another session holds the lane.
        Self::apply_cleanup_plan(&cleanup);
        self.apply_stage_change(Some(target));
        self.paused_at_stage = None;
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
        stage_change: Option<crate::lifecycle::Stage>,
        _clear_paused: bool,
        clear_pending: bool,
    ) {
        let Some(run_id) = self.current_run_id else {
            return;
        };
        let target = stage_change.unwrap_or(self.lifecycle_stage);
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
        self.sync_fsm_running_state();
        let outcome = self.with_lifecycle_ops_ctx(op);
        self.apply_op_outcome(outcome, label);
    }

    /// Apply a [`crate::lifecycle::CleanupPlan`] to disk.
    ///
    /// Mirrors the persisted `go_back` semantics: missing files / directories
    /// are not an error (rewind is best-effort cleanup); `restore_backups`
    /// move a backup to its destination only when the backup actually
    /// exists, then remove the backup so a second rewind doesn't replay it.
    fn apply_cleanup_plan(plan: &crate::lifecycle::CleanupPlan) {
        for path in &plan.delete {
            let res = if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            if let Err(e) = res {
                tracing::debug!("cleanup remove failed for {}: {e}", path.display());
            }
        }
        for (backup, dest) in &plan.restore_backups {
            if backup.exists()
                && std::fs::copy(backup, dest).is_ok()
                && let Err(e) = std::fs::remove_file(backup)
            {
                tracing::debug!("cleanup backup remove failed for {}: {e}", backup.display());
            }
        }
    }

    /// Drive the persisted [`crate::state::Stage`] to the variant matching the
    /// rewind target and refresh the lifecycle-stage mirror.
    ///
    /// Rewinds frequently cross the stage-graph's forward-only edges (e.g.
    /// Implementation(r) → Idea on the skip-to-impl path), so this uses
    /// unchecked [`session_state::set_stage_for_operator_retry`] and then
    /// triggers the same downstream effects `transition_to_stage` would have
    /// fired for an allowed forward transition.
    fn apply_stage_change(&mut self, target: Option<crate::lifecycle::Stage>) {
        let Some(target) = target else {
            return;
        };
        let persisted = Stage::from_lifecycle_stage(target);
        // from_lifecycle_stage never returns BlockedNeedsUser, so any blocked
        // current_stage is always leaving the blocked state.
        let leaving_blocked = matches!(self.state.current_stage, Stage::BlockedNeedsUser);
        session_state::set_stage_for_operator_retry(&mut self.state, persisted);
        if leaving_blocked {
            // Mirror the `execute_transition` invariant: leaving
            // BlockedNeedsUser clears the block_origin so a subsequent
            // re-block records a fresh provenance. Required for the cancel
            // path from a final-validation block — the lane gate would
            // otherwise let stale provenance satisfy the force-ship guard.
            self.state.block_origin = None;
        }
        self.refresh_lifecycle_stage();
        self.agent_line_count = 0;
        self.rebuild_tree_view(None);
        self.enable_progress_follow_and_refocus();
        self.set_follow_tail(true);
        self.maybe_emit_stage_notification(persisted);
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        // The validated path captures the round base at Implementation(r)
        // entry; mirror that so the simplifier still sees a fresh base_sha.
        if let Stage::ImplementationRound(round) = persisted {
            let round_dir = self
                .session_dir()
                .join("rounds")
                .join(format!("{round:03}"));
            self.capture_round_base(&round_dir);
        }
    }

    /// Route a [`crate::lifecycle::StageSpec`] to the matching persisted
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
    /// The persisted launchers still derive some details from
    /// `state.current_stage`; `StageSpec` is the dispatch key while that
    /// launch path remains in place.
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
                // The lifecycle `Stage::Plan` covers PlanningRunning..ShardingRunning;
                // operator rewinds that land on Plan must defer Sharding to the
                // shell scheduler so `decide_waiting_dispatch` can re-verify the
                // baseline. The auto-launch path only fires this branch when
                // persisted `current_stage == ShardingRunning`, which already
                // encodes a passed baseline check.
                if matches!(self.state.current_stage, Stage::ShardingRunning) {
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
    /// needs an explicit cancel so the agent process terminates. The persisted
    /// `current_stage` is left untouched here — `complete_run_finalization`
    /// observes the FSM state once `confirm_dead` lands and drives the
    /// transition to [`Stage::Cancelled`] from there.
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
    /// current persisted `Running` agent, if any.
    ///
    /// The persisted launch path (`start_run_tracking`) is still authoritative,
    /// so the FSM can be Idle when an operator hits `:stop` after a TUI
    /// restart or in tests that synthesize a Running `RunRecord` without
    /// going through launch. This helper
    /// reconciles by synthesizing a [`StageSpec`] / [`ActiveRun`] from the
    /// persisted `RunRecord` and pushing the FSM through `start` +
    /// `confirm_running`. Already-Running and Stopping states are no-ops.
    /// All FSM errors are absorbed: this sync is best-effort until the FSM
    /// owns launch persistence end to end.
    pub(crate) fn sync_fsm_running_state(&mut self) {
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
                "sync_fsm_running_state: unknown stage '{}' / window '{}'",
                run.stage,
                run.window_name
            );
            return;
        };
        let spec = crate::lifecycle::StageSpec::from_run_record(&run)
            .expect("known stage_id_for_run must build a StageSpec");
        debug_assert_eq!(spec.stage_id, stage_id);
        if self.sync_fsm_start(spec).is_err() {
            return;
        }
        let active = crate::lifecycle::ActiveRun::from_run_record(&run)
            .expect("known stage_id_for_run must build an ActiveRun");
        let _ = self.sync_fsm_confirm_running(active);
    }

    /// Mirror [`crate::lifecycle::Fsm::start`] into persisted fields.
    ///
    /// Does not set `current_run_id`; the run id is known at
    /// `confirm_running`. Used by the launch-time sync hook in
    /// `start_run_tracking`. Errors from the FSM are logged and discarded;
    /// the persisted path is authoritative for persistence, so a misordered
    /// FSM transition isn't fatal.
    pub(crate) fn sync_fsm_start(
        &mut self,
        spec: crate::lifecycle::StageSpec,
    ) -> Result<(), crate::lifecycle::FsmError> {
        match self.fsm.start(spec) {
            Ok(()) => Ok(()),
            Err(err) => {
                tracing::warn!("sync_fsm_start: legal path desync, FSM rejected start ({err:?})");
                Err(err)
            }
        }
    }

    /// Mirror [`crate::lifecycle::Fsm::confirm_running`] and sync the
    /// persisted `current_run_id` field. Errors are logged and discarded.
    pub(crate) fn sync_fsm_confirm_running(
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
                    "sync_fsm_confirm_running: legal path desync ({err:?}); \
                     keeping persisted current_run_id"
                );
                Err(err)
            }
        }
    }

    /// Mirror [`crate::lifecycle::Fsm::confirm_dead`] and clear the persisted
    /// `current_run_id` / `run_launched` fields.
    pub(crate) fn sync_fsm_confirm_dead(
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
                    "sync_fsm_confirm_dead: legal path desync ({err:?}); \
                     leaving persisted current_run_id unchanged"
                );
                Err(err)
            }
        }
    }

    pub(crate) fn transition_to_stage(&mut self, next_stage: Stage) -> Result<()> {
        session_state::execute_transition(&mut self.state, next_stage)?;
        self.refresh_lifecycle_stage();
        // Pin the round's review scope at round entry — including the
        // skip-to-impl path that has no reviewer stage and goal-gap re-runs
        // that create a fresh implementation round — so the simplifier can
        // consistently use `base_sha..HEAD`. `capture_round_base` is
        // idempotent on resume.
        if let Stage::ImplementationRound(round) = next_stage {
            let round_dir = self
                .session_dir()
                .join("rounds")
                .join(format!("{round:03}"));
            self.capture_round_base(&round_dir);
        }
        self.agent_line_count = 0;
        self.rebuild_tree_view(None);
        // Stage transitions are an automatic re-enable point for progress
        // follow: re-engage and snap focus to the running stage / latest run.
        // The collapsed-ancestor fallback inside `maybe_refocus_to_progress`
        // matches the pre-existing single-stage cursor move when no run is
        // active yet.
        self.enable_progress_follow_and_refocus();
        // Re-engage tail-follow on stage change so the new stage's transcript
        // streams into view.
        self.set_follow_tail(true);
        // Notify before clearing the live-summary cache so the outgoing
        // stage's last "what I was doing" line lands in the body of the
        // ntfy push — that's the answer to "what just happened?" for a
        // review pause or a final pipeline-done.
        self.maybe_emit_stage_notification(next_stage);
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        Ok(())
    }
    /// Like [`Self::transition_to_stage`], but logs failures instead of
    /// returning them. Use at call sites where the transition is best-effort
    /// and the show must go on (e.g., stage success/fallback paths).
    pub(crate) fn transition_to_stage_logged(&mut self, next_stage: Stage) {
        if let Err(e) = self.transition_to_stage(next_stage) {
            tracing::warn!("failed to transition to {next_stage:?}: {e}");
        }
    }
    /// Set `block_origin` on the session and transition into
    /// `BlockedNeedsUser`. The single throat for entering a block from app
    /// code so the persisted provenance is always populated and the
    /// force-ship guard has a value to read.
    pub(crate) fn transition_to_blocked(&mut self, origin: BlockOrigin) -> Result<()> {
        self.state.block_origin = Some(origin);
        self.transition_to_stage(Stage::BlockedNeedsUser)
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
        use crate::data::artifacts::SkipToImplKind;
        use crate::data::synthetic_artifacts::generate_synthetic_artifacts;
        use anyhow::Context;
        let session_dir = self.session_dir();
        if self.state.skip_to_impl_kind == Some(SkipToImplKind::NothingToDo) {
            self.transition_to_stage(Stage::Done)?;
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
        self.transition_to_stage(Stage::ImplementationRound(1))?;
        self.state.save()?; // Persist state after transition and builder setup
        Ok(())
    }
    pub fn decline_skip_to_implementation(&mut self) -> Result<()> {
        use crate::data::artifacts::SkipToImplKind;
        let kind = self.state.skip_to_impl_kind;
        session_state::clear_skip_to_impl_proposal(&mut self.state);
        let target = if kind == Some(SkipToImplKind::NothingToDo) {
            Stage::BrainstormRunning
        } else {
            Stage::SpecReviewRunning
        };
        self.transition_to_stage(target)?;
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
        self.transition_to_stage(Stage::Done)?;
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
        self.transition_to_stage(Stage::Dreaming(round))?;
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
        // complete_run_finalization dispatches on current_stage; restore the
        // originating running stage so the correct success successor fires.
        let originating = match decision.stage.as_str() {
            "brainstorm" => Stage::BrainstormRunning,
            "planning" => Stage::PlanningRunning,
            "recovery" => Stage::BuilderRecovery(decision.round),
            other => anyhow::bail!("accept_guard_keep: unexpected stage '{other}'"),
        };
        session_state::restore_guard_originating_stage(&mut self.state, originating);
        self.refresh_lifecycle_stage();
        self.complete_run_finalization(&run, None)
    }
    pub(crate) fn editable_artifact(&self) -> Option<std::path::PathBuf> {
        let session_dir = self.session_dir();
        let artifacts = session_dir.join("artifacts");
        let path = match self.state.current_stage {
            Stage::BrainstormRunning | Stage::SpecReviewRunning | Stage::SpecReviewPaused => {
                artifacts.join("spec.md")
            }
            Stage::PlanningRunning | Stage::PlanReviewRunning | Stage::PlanReviewPaused => {
                artifacts.join("plan.md")
            }
            Stage::ShardingRunning | Stage::BuilderRecoverySharding(_) => {
                artifacts.join("tasks.toml")
            }
            Stage::BuilderRecovery(_) => artifacts.join("tasks.toml"),
            Stage::BuilderRecoveryPlanReview(_) => artifacts.join("plan_review.toml"),
            Stage::ImplementationRound(r) | Stage::ReviewRound(r) => session_dir
                .join("rounds")
                .join(format!("{r:03}"))
                .join("task.toml"),
            Stage::IdeaInput
            | Stage::Done
            | Stage::BlockedNeedsUser
            | Stage::WaitingToImplement
            | Stage::RepoStateUpdateRunning
            | Stage::SkipToImplPending
            | Stage::GitGuardPending
            | Stage::FinalValidation(_)
            | Stage::DreamingPending
            | Stage::Dreaming(_)
            | Stage::Simplification(_)
            | Stage::Cancelled => {
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
        !matches!(self.state.current_stage, Stage::IdeaInput | Stage::Done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::mk_app;
    use crate::lifecycle::{AgentState, TickInput, TickOutcome};
    use crate::state::{PipelineItem, PipelineItemStatus, SessionState};

    fn item(task_id: u32, status: PipelineItemStatus) -> PipelineItem {
        PipelineItem {
            id: task_id,
            stage: "coder".to_string(),
            task_id: Some(task_id),
            round: None,
            status,
            title: None,
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 1,
        }
    }

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

    #[test]
    fn lifecycle_task_projection_matches_stage_lane() {
        let mut state = SessionState::new("tasks-for-stage".to_string());
        state.builder.pipeline_items = vec![
            item(10, PipelineItemStatus::Pending),
            item(20, PipelineItemStatus::Running),
        ];
        let app = mk_app(state);

        assert_eq!(
            app.lifecycle_task_ids_for_stage(crate::lifecycle::Stage::Implementation(1)),
            vec![20, 10],
            "implementation retries the running task first, then pending work",
        );
        assert_eq!(
            app.lifecycle_task_ids_for_stage(crate::lifecycle::Stage::Review(1)),
            vec![20],
            "review operates on the current running builder task",
        );
        assert!(
            app.lifecycle_task_ids_for_stage(crate::lifecycle::Stage::Plan)
                .is_empty(),
            "non-task stages should not receive builder task ids",
        );
    }

    #[test]
    fn lifecycle_projection_names_stage_gate_flags_from_persisted_stage() {
        let mut state = SessionState::new("stage-gates".to_string());
        state.current_stage = Stage::BuilderRecoveryPlanReview(2);
        let app = mk_app(state);

        let projection = app.lifecycle_stage_projection(crate::lifecycle::Stage::Implementation(2));

        assert!(projection.gates.recovery_active);
        assert!(!projection.gates.simplification_requested);
        assert!(!projection.gates.dreaming_accepted);

        let mut state = SessionState::new("simplification-gate".to_string());
        state.current_stage = Stage::Simplification(3);
        let app = mk_app(state);

        let projection = app.lifecycle_stage_projection(crate::lifecycle::Stage::Review(3));

        assert!(!projection.gates.recovery_active);
        assert!(projection.gates.simplification_requested);
        assert!(!projection.gates.dreaming_accepted);
    }

    #[test]
    fn lifecycle_projection_builds_stage_ctx_with_owned_snapshots() {
        let mut state = SessionState::new("projection-context".to_string());
        state.current_stage = Stage::ImplementationRound(4);
        state.builder.pipeline_items = vec![
            item(10, PipelineItemStatus::Pending),
            item(20, PipelineItemStatus::Running),
        ];
        state.modes.yolo = true;
        state.modes.cheap = true;
        let app = mk_app(state);

        let projection = app.lifecycle_stage_projection(crate::lifecycle::Stage::Implementation(4));
        let ctx = projection.stage_ctx();

        assert_eq!(ctx.session_id, "projection-context");
        assert_eq!(ctx.stage, crate::lifecycle::Stage::Implementation(4));
        assert_eq!(ctx.pending_task_ids, &[20, 10]);
        assert!(ctx.yolo);
        assert!(ctx.cheap);
        assert!(!ctx.recovery_active);
        assert!(!ctx.simplification_requested);
        assert!(!ctx.dreaming_accepted);
    }

    #[test]
    fn active_modal_preserves_agent_error_stage_identity_for_lifecycle_substages() {
        for (stage, expected) in [
            (
                Stage::RepoStateUpdateRunning,
                ModalKind::StageError(StageId::RepoStateUpdate),
            ),
            (
                Stage::BuilderRecovery(2),
                ModalKind::StageError(StageId::Recovery),
            ),
            (
                Stage::BuilderRecoveryPlanReview(2),
                ModalKind::StageError(StageId::RecoveryPlanReview),
            ),
            (
                Stage::BuilderRecoverySharding(2),
                ModalKind::StageError(StageId::RecoverySharding),
            ),
            (
                Stage::Simplification(2),
                ModalKind::StageError(StageId::Simplification),
            ),
        ] {
            let mut state = SessionState::new(format!("stage-error-{stage:?}"));
            state.current_stage = stage;
            state.agent_error = Some("failed".to_string());
            let app = mk_app(state);

            assert_eq!(app.active_modal(), Some(expected));
        }
    }

    #[test]
    fn scheduler_sees_builder_tasks_for_coder_dispatch() {
        let mut state = SessionState::new("coder-dispatch".to_string());
        state.current_stage = Stage::ImplementationRound(1);
        state.builder.pipeline_items = vec![item(10, PipelineItemStatus::Pending)];
        let app = mk_app(state);

        let outcome = app.with_lifecycle_stage_ctx(app.lifecycle_stage, |ctx| {
            app.scheduler.plan(TickInput {
                agent: &AgentState::Idle,
                stage: app.lifecycle_stage,
                paused_at_stage: app.paused_at_stage,
                pending_decisions: &app.pending_decisions,
                project_lane_allows: true,
                ctx,
            })
        });

        match outcome {
            TickOutcome::Dispatch(spec) => {
                assert_eq!(spec.stage_id, crate::lifecycle::StageId::Coder);
                assert_eq!(spec.task_id, Some(10));
            }
            other => panic!("expected coder dispatch, got {other:?}"),
        }
    }

    #[test]
    fn scheduler_sees_current_task_for_reviewer_dispatch() {
        let mut state = SessionState::new("reviewer-dispatch".to_string());
        state.current_stage = Stage::ReviewRound(1);
        state.builder.pipeline_items = vec![item(20, PipelineItemStatus::Running)];
        let app = mk_app(state);

        let outcome = app.with_lifecycle_stage_ctx(app.lifecycle_stage, |ctx| {
            app.scheduler.plan(TickInput {
                agent: &AgentState::Idle,
                stage: app.lifecycle_stage,
                paused_at_stage: app.paused_at_stage,
                pending_decisions: &app.pending_decisions,
                project_lane_allows: true,
                ctx,
            })
        });

        match outcome {
            TickOutcome::Dispatch(spec) => {
                assert_eq!(spec.stage_id, crate::lifecycle::StageId::Reviewer);
                assert_eq!(spec.task_id, Some(20));
            }
            other => panic!("expected reviewer dispatch, got {other:?}"),
        }
    }
}
