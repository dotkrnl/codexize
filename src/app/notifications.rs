use super::App;
use crate::data::notifications::{InteractiveWaitMarker, NotificationContext, phase_needs_input};
#[cfg(test)]
use crate::data::notifications::{NotificationEvent, NotificationRuntime};
use crate::state::{BlockOrigin, MessageKind, Phase, RunRecord, SessionState};
use std::time::Duration;

const NOTIFICATION_WARNING_TTL: Duration = Duration::from_secs(8);
const NOTIFICATION_SHUTDOWN_DRAIN: Duration = Duration::from_secs(2);

impl App {
    pub(crate) fn maybe_emit_phase_notification(&mut self, phase: Phase) {
        if phase_needs_input(phase) {
            let context = self.notification_context_for_phase(phase);
            self.notification_runtime.emit_phase_wait(phase, context);
        } else if phase == Phase::Done {
            let context = self.notification_context_for_done();
            self.notification_runtime.emit_pipeline_done(phase, context);
        }
    }

    pub(crate) fn maybe_emit_interactive_wait_notification(&mut self) {
        let Some(marker) = self.current_interactive_wait_marker() else {
            self.interactive_wait_marker = None;
            return;
        };
        let is_rising_edge = self.interactive_wait_marker.is_none();
        self.interactive_wait_marker = Some(marker);
        if !is_rising_edge {
            return;
        }
        let Some(run) = self
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == marker.run_id)
            .cloned()
        else {
            return;
        };
        let context = self.notification_context_for_run(&run);
        self.notification_runtime
            .emit_interactive_wait(self.state.current_phase, context, marker);
    }

    pub(crate) fn poll_notification_reports(&mut self) {
        for failure in self.notification_runtime.poll_publish_failures() {
            let log_message = format!("notification_publish_failed: {failure}");
            let _ = self.state.log_event(log_message);
            self.push_status(
                format!("ntfy notification failed: {failure}"),
                super::status_line::Severity::Warn,
                NOTIFICATION_WARNING_TTL,
            );
        }
    }

    pub(crate) fn drain_notifications_for_shutdown(&mut self) {
        let completed = crate::data::async_bridge::block_on_io(
            self.notification_runtime
                .drain_pending_sends(NOTIFICATION_SHUTDOWN_DRAIN),
        );
        self.poll_notification_reports();
        if !completed {
            let message = "notification_publish_drain_timeout".to_string();
            let _ = self.state.log_event(message.clone());
            self.push_status(
                message,
                super::status_line::Severity::Warn,
                NOTIFICATION_WARNING_TTL,
            );
        }
    }

    fn current_interactive_wait_marker(&self) -> Option<InteractiveWaitMarker> {
        if !self.interactive_run_waiting_for_input() {
            return None;
        }
        let run_id = self.current_run_id?;
        let message_index = self
            .messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, message)| message.run_id == run_id && message.kind == MessageKind::AgentText)
            .map_or(self.messages.len(), |(index, _)| index);
        Some(InteractiveWaitMarker {
            run_id,
            message_index,
        })
    }

    fn notification_context_for_phase(&self, phase: Phase) -> NotificationContext {
        let mut context = match phase {
            Phase::BlockedNeedsUser => {
                let stage = self
                    .state
                    .block_origin
                    .map(stage_for_block_origin)
                    .or_else(|| self.running_run().map(|run| run.stage.as_str()))
                    .unwrap_or("blocked")
                    .to_string();
                self.notification_context(stage, None, phase_round(phase), None, None)
            }
            Phase::SpecReviewPaused => self.notification_context_for_stage("spec-review"),
            Phase::PlanReviewPaused => self.notification_context_for_stage("plan-review"),
            Phase::SkipToImplPending => {
                // Skip confirmation is a modal rather than a RunRecord stage;
                // a stable pseudo-stage keeps its dedupe identity reviewable.
                self.notification_context_for_stage("skip-to-impl")
            }
            Phase::GitGuardPending => {
                if let Some(decision) = &self.state.pending_guard_decision {
                    self.notification_context(
                        decision.stage.clone(),
                        decision.task_id,
                        Some(decision.round),
                        Some(decision.attempt),
                        Some(decision.run_id),
                    )
                } else {
                    self.notification_context_for_stage("git-guard")
                }
            }
            _ => self.notification_context(
                phase.label().to_ascii_lowercase().replace(' ', "-"),
                None,
                phase_round(phase),
                None,
                None,
            ),
        };
        // Phase waits surface "what was the agent doing when this fired?" —
        // the live-summary line is the right answer. Skip-to-impl and
        // git-guard prompts are modal decisions where the live summary
        // would just repeat the prompt, so omit it there.
        if phase_carries_live_summary(phase) {
            context.last_live_summary = self.last_live_summary_text();
        }
        context
    }

    fn notification_context_for_done(&self) -> NotificationContext {
        let mut context = self.notification_context_for_stage("pipeline");
        context.last_live_summary = self.last_live_summary_text();
        context
    }

    fn notification_context_for_run(&self, run: &RunRecord) -> NotificationContext {
        let mut context = self.notification_context(
            run.stage.clone(),
            run.task_id,
            Some(run.round),
            Some(run.attempt),
            Some(run.id),
        );
        // Interactive-run waits surface the agent's question itself, so
        // the last `AgentText` for this run is what the operator wants
        // to read on the lock screen.
        context.last_agent_response = self.last_agent_response_text(run.id);
        context
    }

    fn notification_context_for_stage(&self, stage: &str) -> NotificationContext {
        self.notification_context(stage.to_string(), None, None, None, None)
    }

    fn notification_context(
        &self,
        stage: String,
        task_id: Option<u32>,
        round: Option<u32>,
        attempt: Option<u32>,
        run_id: Option<u64>,
    ) -> NotificationContext {
        NotificationContext {
            session_id: self.state.session_id.clone(),
            session_label: session_label(&self.state),
            stage,
            task_id,
            round,
            attempt,
            run_id,
            last_live_summary: None,
            last_agent_response: None,
        }
    }

    fn last_live_summary_text(&self) -> Option<String> {
        let cached = self.live_summary_cached_text.trim();
        if !cached.is_empty() {
            return Some(self.live_summary_cached_text.clone());
        }
        self.messages
            .iter()
            .rev()
            .find(|message| matches!(message.kind, MessageKind::Brief))
            .map(|message| message.text.clone())
    }

    fn last_agent_response_text(&self, run_id: u64) -> Option<String> {
        self.messages
            .iter()
            .rev()
            .find(|message| {
                message.run_id == run_id && matches!(message.kind, MessageKind::AgentText)
            })
            .map(|message| message.text.clone())
    }

    #[cfg(test)]
    pub(crate) fn enable_notifications_for_test(&mut self) {
        self.notification_runtime = NotificationRuntime::enabled_for_test();
        self.interactive_wait_marker = None;
    }

    #[cfg(test)]
    pub(crate) fn notification_events_for_test(&self) -> &[NotificationEvent] {
        self.notification_runtime.events()
    }
}

fn session_label(state: &SessionState) -> String {
    let title = state
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty());
    let idea = state
        .idea_text
        .as_deref()
        .filter(|idea| !idea.trim().is_empty());
    title.or(idea).map_or_else(
        || state.session_id.clone(),
        |value| value.chars().take(80).collect(),
    )
}

fn stage_for_block_origin(origin: BlockOrigin) -> &'static str {
    match origin {
        BlockOrigin::Brainstorm => "brainstorm",
        BlockOrigin::SpecReview => "spec-review",
        BlockOrigin::SkipToImpl => "skip-to-impl",
        BlockOrigin::Planning => "planning",
        BlockOrigin::PlanReview => "plan-review",
        BlockOrigin::Sharding => "sharding",
        BlockOrigin::Implementation => "coder",
        BlockOrigin::Review => "reviewer",
        BlockOrigin::BuilderRecovery => "recovery",
        BlockOrigin::GitGuard => "git-guard",
        BlockOrigin::FinalValidation => "final-validation",
        BlockOrigin::Simplification => "simplifier",
        BlockOrigin::Dreaming => "dreaming",
    }
}

/// True when the phase represents "the agent paused, here's what it was
/// doing": a live-summary excerpt is then the right context to surface in
/// the notification body. Skip-to-impl and git-guard prompts are modal
/// decisions where the live summary would just echo the prompt itself, so
/// they opt out and let the lead sentence stand alone.
fn phase_carries_live_summary(phase: Phase) -> bool {
    !matches!(phase, Phase::SkipToImplPending | Phase::GitGuardPending)
}

fn phase_round(phase: Phase) -> Option<u32> {
    match phase {
        Phase::ImplementationRound(round)
        | Phase::ReviewRound(round)
        | Phase::BuilderRecovery(round)
        | Phase::BuilderRecoveryPlanReview(round)
        | Phase::BuilderRecoverySharding(round)
        | Phase::FinalValidation(round)
        | Phase::Dreaming(round)
        | Phase::Simplification(round) => Some(round),
        _ => None,
    }
}
