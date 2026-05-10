use super::{App, DEFAULT_STAMP_TIMEOUT_MS, ENV_STAMP_TIMEOUT_MS, guard, status_line};
use crate::{
    selection::{SubscriptionKind, config::SelectionPhase, selection::SelectionWarning},
    state::{self as session_state, Message, MessageKind, MessageSender, Phase, RunStatus},
    tasks,
};
use anyhow::Result;
use std::time::Duration;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperatorTerminationMarker {
    Stopped,
    RetryRequested,
}
#[cfg(test)]
#[path = "run_helpers_tests.rs"]
mod tests;
impl App {
    pub(crate) fn default_acp_policy(&self) -> crate::acp::AcpLaunchPolicy {
        crate::acp::AcpLaunchPolicy::from_policy_defaults(&self.config.acp_policy_view())
    }

    /// Resolve the session directory from the loaded `[paths]` config.
    /// Mirrors the picker fallback in `main.rs`: only honor the
    /// `paths.sessions_root` view when the operator set the value
    /// explicitly. Otherwise use `state::codexize_root().join("sessions")`
    /// so the App reads from the same project-local `.codexize/sessions`
    /// tree the runner stages and `state::session_dir(...)` write to.
    pub(crate) fn session_dir(&self) -> std::path::PathBuf {
        let root = if self.config.paths.sessions_root.is_explicit() {
            self.paths.sessions_root.clone()
        } else {
            session_state::codexize_root().join("sessions")
        };
        root.join(&self.state.session_id)
    }

    /// Build a per-call `PromptMeta` from this App's loaded `Config`.
    /// Carries `memory.max_topics_per_read` and an explicit memory_root
    /// override (when the operator set `paths.memory_root`) so every
    /// prompt template renders with the operator's configured values.
    pub(crate) fn prompt_meta(&self) -> crate::app::prompts::PromptMeta {
        crate::app::prompts::PromptMeta {
            max_topics_per_read: self.memory_view.max_topics_per_read,
            memory_root: if self.config.paths.memory_root.is_explicit() {
                Some(self.paths.memory_root.clone())
            } else {
                None
            },
        }
    }

    /// Resolve the memory root for this App's session.
    ///
    /// When `paths.memory_root` is explicitly set in `~/.codexize/config.toml`,
    /// the override is authoritative — every memory consumer (dreaming
    /// stage, prompt context, finalization reasons, picker bootstrap)
    /// reads from the configured location. When the value is the baked
    /// default we fall back to `memory_root_from_session_path` so that
    /// tests and standalone tools whose session dirs sit under an
    /// arbitrary `.codexize/sessions` ancestor (e.g. tempdir-based
    /// fixtures) keep deriving a sibling `memory/` directory beside
    /// their session tree.
    pub(crate) fn memory_root(&self) -> std::path::PathBuf {
        if self.config.paths.memory_root.is_explicit() {
            self.paths.memory_root.clone()
        } else {
            crate::logic::memory::memory_root_from_session_path(&self.session_dir())
        }
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
    /// 1-based ordinal of `round` within this task's coder-round
    /// history. The orchestrator's `Phase::ImplementationRound` round
    /// counter is global — it ticks across tasks — so a task that
    /// starts at global round 4 (because earlier tasks consumed rounds
    /// 1-3) is on its 1st task-round, not its 4th. Used as the input
    /// to `auto_tough_effort` so the auto-promotion threshold counts
    /// rounds spent on this task only.
    pub(crate) fn task_round_index(&self, task_id: u32, round: u32) -> u32 {
        use std::collections::BTreeSet;
        let mut rounds: BTreeSet<u32> = self
            .state
            .agent_runs
            .iter()
            .filter(|run| run.stage == "coder" && run.task_id == Some(task_id))
            .map(|run| run.round)
            .collect();
        rounds.insert(round);
        rounds
            .iter()
            .position(|&candidate| candidate == round)
            .map(|pos| (pos + 1) as u32)
            .unwrap_or(1)
    }
    /// Effort to launch a coder/reviewer at, applying the per-task
    /// auto-tough rule on top of the task's declared effort. Both the
    /// coder and reviewer launches read this so the pair stays in
    /// agreement.
    pub(crate) fn task_effort_for_round(
        &self,
        session_dir: &std::path::Path,
        task_id: u32,
        round: u32,
    ) -> crate::adapters::EffortLevel {
        use crate::app::prompts::{auto_tough_effort, task_effort_for};
        let declared = task_effort_for(session_dir, task_id);
        auto_tough_effort(declared, self.task_round_index(task_id, round))
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
    pub(crate) fn active_run_exists(&self, run_id: u64) -> bool {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return false;
        }
        self.runner_supervisor.run_is_active(run_id)
    }
    pub(crate) fn retry_key_for_run(run: &crate::state::RunRecord) -> (String, Option<u32>, u32) {
        (run.stage.clone(), run.task_id, run.round)
    }
    pub(crate) fn used_review_pairs(
        runs: &[crate::state::RunRecord],
    ) -> (Vec<SubscriptionKind>, Vec<(SubscriptionKind, String)>) {
        let mut vendors = Vec::new();
        let mut models = Vec::new();
        for run in runs {
            let Some(vendor) =
                crate::logic::selection::assemble::parse_subscription_str(&run.subscription_label)
            else {
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
        self.session_dir()
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
        self.session_dir()
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
        self.session_dir()
            .join(".guards")
            .join(format!("{stage}-{task}-r{round}-a{attempt}"))
    }
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
        let session_dir = self.session_dir();
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
    pub(crate) fn capture_round_base(&self, round_dir: &std::path::Path) {
        let scope_file = round_dir.join("review_scope.toml");
        if scope_file.exists() {
            return;
        }
        if let Some(parent) = scope_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        #[cfg(test)]
        let _ = std::fs::write(&scope_file, "base_sha = \"test-base\"\n");
        #[cfg(not(test))]
        if let Some(sha) = crate::app::prompts::git_rev_parse_head() {
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
        session_state::set_cheap_mode(&mut self.state, value);
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
        let task_id = session_state::ensure_builder_task_for_round(&mut self.state, round)?;
        let round_dir = self
            .session_dir()
            .join("rounds")
            .join(format!("{round:03}"));
        let _ = std::fs::create_dir_all(&round_dir);
        Some(task_id)
    }
    pub(crate) fn finalize_run_record(
        &mut self,
        run_id: u64,
        success: bool,
        error: Option<String>,
    ) {
        self.watchdog.remove(run_id);
        let Some(finished) =
            session_state::finish_run_record(&mut self.state, run_id, success, error)
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
                finished.model, finished.subscription_label
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
    pub(crate) fn enter_simplification_or_done(&mut self, round: u32, yolo: bool) -> Result<()> {
        if yolo {
            self.transition_to_phase(Phase::Done)?;
            return Ok(());
        }
        let _ = session_state::enter_simplification(&mut self.state, round)?;
        Ok(())
    }
    pub(crate) fn append_goal_gap_tasks(
        &mut self,
        session_dir: &std::path::Path,
        new_tasks: &[tasks::Task],
    ) -> Result<()> {
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");
        let mut parsed = tasks::validate(&tasks_path)?;
        parsed.tasks.extend(new_tasks.iter().cloned());
        let text = toml::to_string_pretty(&parsed)?;
        std::fs::write(&tasks_path, text)?;
        let next_iteration = self
            .state
            .builder
            .pipeline_items
            .iter()
            .map(|item| item.iteration)
            .max()
            .unwrap_or(1)
            + 1;
        session_state::append_final_validation_gap_tasks(
            &mut self.state,
            new_tasks.iter().map(|task| (task.id, task.title.clone())),
            next_iteration,
        );
        Ok(())
    }
}
