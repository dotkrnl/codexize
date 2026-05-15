use super::models::subscription_tag;
use super::{App, FailedModelSet, RetryKey};
use crate::selection::{SubscriptionKind, select_excluding};
use crate::state::{MessageKind, RunStatus, SessionState};
use std::collections::{HashMap, HashSet};
impl App {
    pub(crate) fn rebuild_failed_models(state: &SessionState) -> HashMap<RetryKey, FailedModelSet> {
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
            let Some(vendor) =
                crate::logic::selection::assemble::parse_subscription_str(&run.subscription_label)
            else {
                continue;
            };
            failed_models
                .entry((run.stage.clone(), run.task_id, run.round))
                .or_insert_with(HashSet::new)
                .insert((vendor, run.model.clone()));
        }
        failed_models
    }
    pub(crate) fn retry_exhausted_summary(&self, failed_run: &crate::state::RunRecord) -> String {
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
                run.subscription_label,
                run.model,
                run.error.unwrap_or_else(|| "unknown error".to_string())
            ));
        }
        lines.join("\n")
    }
    fn handle_retry_exhausted(
        &mut self,
        failed_run: &crate::state::RunRecord,
        summary: String,
        log_safety_cap: bool,
    ) -> bool {
        if matches!(failed_run.stage.as_str(), "coder" | "reviewer") {
            return self.enter_builder_recovery(
                failed_run.round,
                failed_run.task_id,
                Some(summary),
                "agent_pivot",
            );
        }
        let summary = if failed_run.stage == "recovery" {
            format!("builder recovery retry exhausted\n{summary}")
        } else {
            summary
        };
        self.record_agent_error(summary.clone());
        let origin = crate::state::BlockOrigin::for_stage(&failed_run.stage)
            .unwrap_or(crate::state::BlockOrigin::Implementation);
        let origin = if failed_run.stage == "recovery" {
            crate::state::BlockOrigin::BuilderRecovery
        } else {
            origin
        };
        if let Err(e) = self.transition_to_blocked(origin) {
            tracing::warn!("failed to transition to blocked after retry cap: {e}");
        }
        self.append_system_message(failed_run.id, MessageKind::End, summary);
        if log_safety_cap {
            let _ = self.state.log_event(format!(
                "auto-retry safety cap hit for {} round {} attempt {}",
                failed_run.stage, failed_run.round, failed_run.attempt
            ));
        }
        true
    }
    pub(crate) fn maybe_auto_retry(&mut self, failed_run: &crate::state::RunRecord) -> bool {
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
        let last_failed_vendor = crate::logic::selection::assemble::parse_subscription_str(
            &failed_run.subscription_label,
        );
        if let Some(vendor) = last_failed_vendor {
            self.failed_models
                .entry(key.clone())
                .or_default()
                .insert((vendor, failed_run.model.clone()));
        }
        if self.models.is_empty() {
            let _ = self.state.log_event(format!(
                "auto-retry unavailable for {} round {} attempt {}: model cache empty",
                failed_run.stage, failed_run.round, failed_run.attempt
            ));
            return false;
        }
        let max_attempts = self.models.len() as u32 + 2;
        if failed_run.attempt >= max_attempts {
            let summary = self.retry_exhausted_summary(failed_run);
            return self.handle_retry_exhausted(failed_run, summary, true);
        }
        let excluded: Vec<(SubscriptionKind, String)> = self
            .failed_models
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let next_model = select_excluding(
            &self.models,
            Self::selection_stage_for_stage(&failed_run.stage),
            &excluded,
            last_failed_vendor,
        );
        if let Some(next_model) = next_model.cloned() {
            self.append_system_message(
                failed_run.id,
                MessageKind::Started,
                format!(
                    "retrying with {}/{}",
                    subscription_tag(next_model.subscription),
                    next_model.name
                ),
            );
            // Sharding's auto-fallback retry must re-enter the
            // WaitingToImplement gate so the shell scheduler re-verifies
            // the repo-state baseline before dispatching sharding again —
            // spec §Data model line 96. BuilderRecoverySharding is exempt
            // because it is already inside a recovery sub-pipeline; the
            // scheduler handles its baseline differently.
            let sharding_pause = matches!(failed_run.stage.as_str(), "sharding")
                && !matches!(
                    self.state.current_stage,
                    crate::state::Stage::BuilderRecoverySharding(_),
                );
            if sharding_pause {
                self.clear_agent_error();
                self.current_run_id = None;
                self.run_launched = false;
                self.live_summary_cached_text.clear();
                self.live_summary_cached_mtime = None;
                return self
                    .transition_to_stage(crate::state::Stage::WaitingToImplement)
                    .is_ok();
            }
            // Pin the model-fallback choice on the App so the next
            // scheduler tick's `dispatch_start` plumbs it through the
            // stage's `launch_*_with_model` entry point. The slot is
            // consumed-once and cleared on read.
            self.next_run_model_override = Some(next_model);
            self.clear_agent_error();
            self.maybe_auto_launch();
            // Report success only if a run actually launched
            // (`run_launched` flips inside `start_run_tracking`). If the
            // scheduler tick declined to dispatch, clear the override so a
            // future tick doesn't reuse the stale pin and report failure so
            // the caller records the original error.
            if self.run_launched {
                return true;
            }
            self.next_run_model_override = None;
            return false;
        }
        let summary = self.retry_exhausted_summary(failed_run);
        self.handle_retry_exhausted(failed_run, summary, false)
    }
}
