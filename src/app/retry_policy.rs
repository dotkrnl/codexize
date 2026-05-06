use super::models::vendor_tag;
use super::{App, FailedModelSet, RetryKey};
use crate::selection::{self, VendorKind, selection::select_excluding};
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
                run.vendor,
                run.model,
                run.error.unwrap_or_else(|| "unknown error".to_string())
            ));
        }
        lines.join("\n")
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
                self.record_agent_error(summary.clone());
                let _ = self.transition_to_blocked(crate::state::BlockOrigin::BuilderRecovery);
                self.append_system_message(failed_run.id, MessageKind::End, summary);
                return true;
            }
            self.record_agent_error(summary.clone());
            let origin = crate::state::BlockOrigin::for_stage(&failed_run.stage)
                .unwrap_or(crate::state::BlockOrigin::Implementation);
            let _ = self.transition_to_blocked(origin);
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
            self.record_agent_error(summary.clone());
            let _ = self.transition_to_blocked(crate::state::BlockOrigin::BuilderRecovery);
            self.append_system_message(failed_run.id, MessageKind::End, summary);
            return true;
        }
        self.record_agent_error(summary.clone());
        let origin = crate::state::BlockOrigin::for_stage(&failed_run.stage)
            .unwrap_or(crate::state::BlockOrigin::Implementation);
        let _ = self.transition_to_blocked(origin);
        self.append_system_message(failed_run.id, MessageKind::End, summary);
        true
    }
}
