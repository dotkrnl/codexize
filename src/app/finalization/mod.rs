// finalization/mod.rs
mod complete;
mod reasons;
mod recovery;

use super::*;
use crate::{
    selection::{
        self, VendorKind,
        config::SelectionPhase,
        selection::{SelectionWarning, select_excluding},
    },
    state::{
        self as session_state, Message, MessageKind, MessageSender, Phase, RunStatus, SessionState,
    },
    tasks,
};
use anyhow::Result;

use super::models::vendor_tag;

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperatorTerminationMarker {
    Stopped,
    RetryRequested,
}

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

    pub(crate) fn attempt_for(&self, stage: &str, task_id: Option<u32>, round: u32) -> u32 {
        self.state
            .agent_runs
            .iter()
            .filter(|run| run.stage == stage && run.task_id == task_id && run.round == round)
            .map(|run| run.attempt)
            .max()
            .unwrap_or(0)
            + 1
    }

    pub(crate) fn completed_rounds(&self, stage: &str) -> u32 {
        self.state
            .agent_runs
            .iter()
            .filter(|run| run.stage == stage && run.status == RunStatus::Done)
            .map(|run| run.round)
            .max()
            .unwrap_or(0)
    }

    pub(crate) fn running_run(&self) -> Option<&crate::state::RunRecord> {
        self.current_run_id.and_then(|run_id| {
            self.state
                .agent_runs
                .iter()
                .find(|run| run.id == run_id && run.status == RunStatus::Running)
        })
    }

    pub(crate) fn has_running_agent(&self) -> bool {
        self.state
            .agent_runs
            .iter()
            .any(|run| run.status == RunStatus::Running)
    }

    pub(crate) fn active_run_exists(&self, window_name: &str) -> bool {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return false;
        }
        crate::runner::run_label_is_active(window_name)
    }

    pub(crate) fn retry_key_for_run(run: &crate::state::RunRecord) -> (String, Option<u32>, u32) {
        (run.stage.clone(), run.task_id, run.round)
    }

    /// Project a list of completed runs into the (vendors, (vendor,model)) shape
    /// expected by `select_for_review` and `select_excluding`. Runs with an
    /// unrecognised vendor string are dropped.
    pub(crate) fn used_review_pairs(
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

    pub(crate) fn phase_for_stage(stage: &str) -> SelectionPhase {
        match stage {
            "brainstorm" => SelectionPhase::Idea,
            "spec-review" => SelectionPhase::Review,
            "planning" => SelectionPhase::Planning,
            "plan-review" => SelectionPhase::Review,
            "sharding" => SelectionPhase::Planning,
            "recovery" => SelectionPhase::Planning,
            "coder" => SelectionPhase::Build,
            "reviewer" => SelectionPhase::Review,
            "simplifier" => SelectionPhase::Build,
            _ => SelectionPhase::Build,
        }
    }

    pub(crate) fn run_key_for(
        stage: &str,
        task_id: Option<u32>,
        round: u32,
        attempt: u32,
    ) -> String {
        let task = task_id
            .map(|id| format!("task-{id}"))
            .unwrap_or_else(|| "stage".to_string());
        format!("{stage}-{task}-r{round}-a{attempt}")
    }

    pub(crate) fn live_summary_path_for_run(
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

    pub(crate) fn live_summary_path_for(
        &self,
        run: &crate::state::RunRecord,
    ) -> std::path::PathBuf {
        self.live_summary_path_for_run(&run.stage, run.task_id, run.round, run.attempt)
    }

    pub(crate) fn finish_stamp_path_for_run(
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

    pub(crate) fn finish_stamp_path_for(
        &self,
        run: &crate::state::RunRecord,
    ) -> std::path::PathBuf {
        self.finish_stamp_path_for_run(&run.stage, run.task_id, run.round, run.attempt)
    }

    pub(crate) fn stamp_timeout_duration() -> Duration {
        std::env::var(ENV_STAMP_TIMEOUT_MS)
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_millis(DEFAULT_STAMP_TIMEOUT_MS))
    }

    pub(crate) fn guard_dir_for(
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
    pub(crate) fn capture_run_guard(
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
        // Simplifier is code-producing like the coder: it must be allowed to
        // advance HEAD via `refactor:`/`style:` commits, while still being
        // forbidden from editing orchestrator control files.
        if stage == "coder" || stage == "simplifier" {
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
                // Reviewer runs only inspect committed base..HEAD now; coder dirt fails earlier.
                false,
            );
            dirty
        }
    }

    pub(crate) fn enforce_run_guard(&self, run: &crate::state::RunRecord) -> guard::VerifyResult {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return guard::VerifyResult::Ok { warnings: vec![] };
        }
        let dir = self.guard_dir_for(&run.stage, run.task_id, run.round, run.attempt);
        guard::verify(&dir, &run.stage)
    }

    pub(crate) fn read_exit_status_code(&self, run: &crate::state::RunRecord) -> Option<i32> {
        crate::runner::read_finish_stamp(&self.finish_stamp_path_for(run))
            .ok()
            .map(|stamp| stamp.exit_code)
    }

    pub(crate) fn artifact_present(path: &std::path::Path) -> bool {
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.len() > 0)
            .unwrap_or(false)
    }

    /// Capture HEAD at round start so the reviewer (and the simplifier) can
    /// inspect `base_sha..HEAD`. Idempotent on resume: the original base is
    /// preserved.
    pub(crate) fn capture_round_base(&self, round_dir: &std::path::Path) {
        let scope_file = round_dir.join("review_scope.toml");
        if scope_file.exists() {
            return;
        }
        if let Some(parent) = scope_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Use a deterministic placeholder in test builds so transitions that
        // capture the round base never shell out to `git` from the test
        // process; production callers always go through `git_rev_parse_head`.
        #[cfg(test)]
        let _ = std::fs::write(&scope_file, "base_sha = \"test-base\"\n");
        #[cfg(not(test))]
        if let Some(sha) = super::prompts::git_rev_parse_head() {
            let _ = std::fs::write(&scope_file, format!("base_sha = \"{sha}\"\n"));
        }
    }

    pub(crate) fn append_system_message(&mut self, run_id: u64, kind: MessageKind, text: String) {
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

    pub(crate) fn emit_dirty_tree_warning(&mut self) {
        if let Some(run_id) = self.current_run_id {
            self.append_system_message(
                run_id,
                MessageKind::SummaryWarn,
                "working tree is dirty \u{2014} agent will run against uncommitted changes"
                    .to_string(),
            );
        }
    }

    pub(crate) fn emit_selection_warning(&mut self, warning: Option<SelectionWarning>) {
        let Some(SelectionWarning::CheapFallback { phase, reason }) = warning else {
            return;
        };
        let message = format!("cheap_fallback: phase={} reason={reason}", phase.name());
        let _ = self.state.log_event(message.clone());
        self.push_status(message, status_line::Severity::Warn, Duration::from_secs(8));
    }

    pub(crate) fn toggle_cheap_mode(&mut self, source: &str) {
        self.set_cheap_mode(!self.state.modes.cheap, source);
    }

    pub(crate) fn set_cheap_mode(&mut self, value: bool, source: &str) {
        session_state::transitions::set_cheap_mode(&mut self.state, value);
        if let Err(err) = self.state.save() {
            self.record_agent_error(format!("failed to save cheap mode: {err:#}"));
            return;
        }
        let _ = self.state.log_event(format!(
            "mode_toggled: mode=cheap value={value} source={source}"
        ));
        let status = if value {
            "cheap: ON  (next agent launch limited to sonnet/kimi/codex-low/flash)"
        } else {
            "cheap: OFF"
        };
        self.push_status(
            status.to_string(),
            status_line::Severity::Info,
            Duration::from_secs(5),
        );
    }

    pub(crate) fn ensure_builder_task_for_round(&mut self, round: u32) -> Option<u32> {
        let task_id =
            session_state::transitions::ensure_builder_task_for_round(&mut self.state, round)?;
        let round_dir = session_state::session_dir(&self.state.session_id)
            .join("rounds")
            .join(format!("{round:03}"));
        let _ = std::fs::create_dir_all(&round_dir);
        Some(task_id)
    }

    /// Launch the non-interactive recovery-mode plan review agent.
    pub(crate) fn finalize_run_record(
        &mut self,
        run_id: u64,
        success: bool,
        error: Option<String>,
    ) {
        // Drop the watchdog entry on every finalization (success, organic
        // failure, or watchdog-induced kill). Spec §3.8.
        self.watchdog.remove(run_id);
        let Some(finished) =
            session_state::transitions::finish_run_record(&mut self.state, run_id, success, error)
        else {
            return;
        };

        let duration = finished.ended_at.signed_duration_since(finished.started_at);
        let total_seconds = duration.num_seconds().max(0);
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        let text = if success {
            format!(
                "done in {minutes}m{seconds:02}s · {} ({})",
                finished.model, finished.vendor
            )
        } else if finished.unverified {
            format!(
                "attempt {} unverified: {}",
                finished.attempt,
                finished
                    .error
                    .unwrap_or_else(|| "unknown error".to_string())
            )
        } else {
            format!(
                "attempt {} failed: {}",
                finished.attempt,
                finished
                    .error
                    .unwrap_or_else(|| "unknown error".to_string())
            )
        };
        let message = Message {
            ts: finished.ended_at,
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

    pub(crate) fn finalize_current_run(&mut self, run: &crate::state::RunRecord) -> Result<()> {
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

    /// Route a converged round into the simplifier (normal path) or jump
    /// directly to `Done` (yolo). The simplifier is the gate for every
    /// non-yolo entry into `FinalValidation`; yolo continues to bypass both
    /// stages because the operator has waived the safety net.
    ///
    /// The cap-to-block branch inside `enter_simplification` populates
    /// `block_origin = Simplification`, which intentionally does *not*
    /// unlock force-ship — that escape hatch remains tied to
    /// `BlockOrigin::FinalValidation`.
    fn enter_simplification_or_done(&mut self, round: u32, yolo: bool) -> Result<()> {
        if yolo {
            self.transition_to_phase(Phase::Done)?;
            return Ok(());
        }

        let _ = session_state::transitions::enter_simplification(&mut self.state, round)?;
        Ok(())
    }

    fn append_goal_gap_tasks(
        &mut self,
        session_dir: &std::path::Path,
        new_tasks: &[tasks::Task],
    ) -> Result<()> {
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");
        let mut parsed = tasks::validate(&tasks_path)?;
        // REVIEWER: validator gap tasks are appended in emitted order because the
        // spec requires conservative ingestion rather than local re-prioritizing.
        parsed.tasks.extend(new_tasks.iter().cloned());
        let text = toml::to_string_pretty(&parsed)?;
        std::fs::write(&tasks_path, text)?;
        session_state::transitions::append_final_validation_gap_tasks(
            &mut self.state,
            new_tasks.iter().map(|task| (task.id, task.title.clone())),
        );
        Ok(())
    }
}
