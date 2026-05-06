use super::App;
use crate::data::notifications::{InteractiveWaitMarker, NotificationContext, phase_needs_input};
#[cfg(test)]
use crate::data::notifications::{NotificationEvent, NotificationRuntime};
use crate::state::{BlockOrigin, MessageKind, Phase, RunRecord, SessionState};

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
            .map(|(index, _)| index)
            .unwrap_or(self.messages.len());
        Some(InteractiveWaitMarker {
            run_id,
            message_index,
        })
    }

    fn notification_context_for_phase(&self, phase: Phase) -> NotificationContext {
        match phase {
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
            Phase::SpecReviewPaused => {
                self.notification_context("spec-review".to_string(), None, None, None, None)
            }
            Phase::PlanReviewPaused => {
                self.notification_context("plan-review".to_string(), None, None, None, None)
            }
            Phase::SkipToImplPending => {
                // Skip confirmation is a modal rather than a RunRecord stage;
                // a stable pseudo-stage keeps its dedupe identity reviewable.
                self.notification_context("skip-to-impl".to_string(), None, None, None, None)
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
                    self.notification_context("git-guard".to_string(), None, None, None, None)
                }
            }
            _ => self.notification_context(
                phase.label().to_ascii_lowercase().replace(' ', "-"),
                None,
                phase_round(phase),
                None,
                None,
            ),
        }
    }

    fn notification_context_for_done(&self) -> NotificationContext {
        self.notification_context("pipeline".to_string(), None, None, None, None)
    }

    fn notification_context_for_run(&self, run: &RunRecord) -> NotificationContext {
        self.notification_context(
            run.stage.clone(),
            run.task_id,
            Some(run.round),
            Some(run.attempt),
            Some(run.id),
        )
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
        }
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
    title
        .or(idea)
        .map(|value| value.chars().take(80).collect())
        .unwrap_or_else(|| state.session_id.clone())
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
    }
}

fn phase_round(phase: Phase) -> Option<u32> {
    match phase {
        Phase::ImplementationRound(round)
        | Phase::ReviewRound(round)
        | Phase::BuilderRecovery(round)
        | Phase::BuilderRecoveryPlanReview(round)
        | Phase::BuilderRecoverySharding(round)
        | Phase::FinalValidation(round)
        | Phase::Simplification(round) => Some(round),
        _ => None,
    }
}
