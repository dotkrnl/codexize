// finalization.rs
use super::*;
use crate::{
    artifacts::{ArtifactKind, SkipToImplProposal},
    coder_summary, review,
    selection::{
        self, VendorKind,
        config::SelectionPhase,
        selection::{SelectionWarning, select_excluding},
    },
    state::{
        self as session_state, Message, MessageKind, MessageSender, PendingGuardDecision, Phase,
        PipelineItem, PipelineItemStatus, RunStatus, SessionState,
    },
    tasks, tmux,
};
use anyhow::Result;

use super::{models::vendor_tag, prompts::*};

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    time::Duration,
};
impl App {
    pub(super) fn rebuild_failed_models(state: &SessionState) -> HashMap<RetryKey, FailedModelSet> {
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

    pub(super) fn attempt_for(&self, stage: &str, task_id: Option<u32>, round: u32) -> u32 {
        self.state
            .agent_runs
            .iter()
            .filter(|run| run.stage == stage && run.task_id == task_id && run.round == round)
            .map(|run| run.attempt)
            .max()
            .unwrap_or(0)
            + 1
    }

    pub(super) fn completed_rounds(&self, stage: &str) -> u32 {
        self.state
            .agent_runs
            .iter()
            .filter(|run| run.stage == stage && run.status == RunStatus::Done)
            .map(|run| run.round)
            .max()
            .unwrap_or(0)
    }

    pub(super) fn running_run(&self) -> Option<&crate::state::RunRecord> {
        self.current_run_id.and_then(|run_id| {
            self.state
                .agent_runs
                .iter()
                .find(|run| run.id == run_id && run.status == RunStatus::Running)
        })
    }

    pub(super) fn has_running_agent(&self) -> bool {
        self.state
            .agent_runs
            .iter()
            .any(|run| run.status == RunStatus::Running)
    }

    pub(super) fn window_exists(&self, window_name: &str) -> bool {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return false;
        }
        tmux::window_exists(window_name)
    }

    pub(super) fn retry_key_for_run(run: &crate::state::RunRecord) -> (String, Option<u32>, u32) {
        (run.stage.clone(), run.task_id, run.round)
    }

    /// Project a list of completed runs into the (vendors, (vendor,model)) shape
    /// expected by `select_for_review` and `select_excluding`. Runs with an
    /// unrecognised vendor string are dropped.
    pub(super) fn used_review_pairs(
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

    pub(super) fn phase_for_stage(stage: &str) -> SelectionPhase {
        match stage {
            "brainstorm" => SelectionPhase::Idea,
            "spec-review" => SelectionPhase::Review,
            "planning" => SelectionPhase::Planning,
            "plan-review" => SelectionPhase::Review,
            "sharding" => SelectionPhase::Planning,
            "recovery" => SelectionPhase::Planning,
            "coder" => SelectionPhase::Build,
            "reviewer" => SelectionPhase::Review,
            _ => SelectionPhase::Build,
        }
    }

    pub(super) fn run_status_path_for(
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
            .join("artifacts")
            .join("run-status")
            .join(format!("{stage}-{task}-r{round}-a{attempt}.txt"))
    }

    pub(super) fn run_status_path(&self, run: &crate::state::RunRecord) -> std::path::PathBuf {
        self.run_status_path_for(&run.stage, run.task_id, run.round, run.attempt)
    }

    pub(super) fn run_key_for(
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

    pub(super) fn live_summary_path_for_run(
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

    pub(super) fn live_summary_path_for(
        &self,
        run: &crate::state::RunRecord,
    ) -> std::path::PathBuf {
        self.live_summary_path_for_run(&run.stage, run.task_id, run.round, run.attempt)
    }

    pub(super) fn finish_stamp_path_for_run(
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

    pub(super) fn finish_stamp_path_for(
        &self,
        run: &crate::state::RunRecord,
    ) -> std::path::PathBuf {
        self.finish_stamp_path_for_run(&run.stage, run.task_id, run.round, run.attempt)
    }

    pub(super) fn stamp_timeout_duration() -> Duration {
        std::env::var(ENV_STAMP_TIMEOUT_MS)
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|ms| *ms > 0)
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_millis(DEFAULT_STAMP_TIMEOUT_MS))
    }

    pub(super) fn guard_dir_for(
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
    pub(super) fn capture_run_guard(
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
        if stage == "coder" {
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

    pub(super) fn enforce_run_guard(&self, run: &crate::state::RunRecord) -> guard::VerifyResult {
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return guard::VerifyResult::Ok { warnings: vec![] };
        }
        let dir = self.guard_dir_for(&run.stage, run.task_id, run.round, run.attempt);
        guard::verify(&dir, &run.stage)
    }

    pub(super) fn read_exit_status_code(&self, run: &crate::state::RunRecord) -> Option<i32> {
        std::fs::read_to_string(self.run_status_path(run))
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
    }

    pub(super) fn artifact_present(path: &std::path::Path) -> bool {
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.len() > 0)
            .unwrap_or(false)
    }

    /// Capture HEAD at round start so the reviewer can inspect `base_sha..HEAD`.
    /// Idempotent on resume: the original base is preserved.
    pub(super) fn capture_round_base(&self, round_dir: &std::path::Path) {
        let scope_file = round_dir.join("review_scope.toml");
        if scope_file.exists() {
            return;
        }
        if let Some(parent) = scope_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            let _ = std::fs::write(&scope_file, "base_sha = \"test-base\"\n");
            return;
        }
        if let Some(sha) = git_rev_parse_head() {
            let _ = std::fs::write(&scope_file, format!("base_sha = \"{sha}\"\n"));
        }
    }

    pub(super) fn failed_unverified_reason(
        stamp_path: &std::path::Path,
        detail: impl AsRef<str>,
    ) -> String {
        format!(
            "failed_unverified: {} at {}",
            detail.as_ref(),
            stamp_path.display()
        )
    }

    pub(super) fn coder_gate_reason(
        &self,
        run: &crate::state::RunRecord,
        round_dir: &std::path::Path,
    ) -> Option<String> {
        let scope_file = round_dir.join("review_scope.toml");
        #[cfg(test)]
        if self.test_launch_harness.is_some() {
            return (!Self::artifact_present(&scope_file)).then(|| "base_missing".to_string());
        }
        if !Self::artifact_present(&scope_file) {
            return Some("base_missing".to_string());
        }
        let base = match read_review_scope_base_sha(&scope_file) {
            Ok(s) => s,
            Err(_) => return Some("base_missing".to_string()),
        };
        if base.is_empty() {
            return Some("base_missing".to_string());
        }
        let stamp_path = self.finish_stamp_path_for(run);
        if !Self::artifact_present(&stamp_path) {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                "missing finish stamp",
            ));
        }
        let stamp = match crate::runner::read_finish_stamp(&stamp_path) {
            Ok(stamp) => stamp,
            Err(_) => {
                return Some(Self::failed_unverified_reason(
                    &stamp_path,
                    "malformed finish stamp",
                ));
            }
        };
        if stamp.head_state != "stable" {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                format!("head_state={}", stamp.head_state),
            ));
        }
        if !stamp.working_tree_clean {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                "working tree not clean on exit",
            ));
        }
        if stamp.exit_code == 0 && stamp.head_after.trim().is_empty() {
            return Some(Self::failed_unverified_reason(
                &stamp_path,
                "empty stable head_after",
            ));
        }
        let summary_path = round_dir.join("coder_summary.toml");
        if summary_path.exists() {
            let summary = match coder_summary::validate(&summary_path) {
                Ok(summary) => summary,
                Err(_) => return Some("invalid_coder_summary".to_string()),
            };
            return match summary.status {
                coder_summary::CoderStatus::Done => None,
                coder_summary::CoderStatus::Partial => Some("coder_partial".to_string()),
            };
        }
        if stamp.head_after == base {
            // No commit and no coder_summary.toml. The no-commit alone is
            // legitimate when the coder declares it (status = "done", with
            // an explanation); the contract violation here is the missing
            // summary that would have explained why nothing was committed.
            return Some("missing_coder_summary".to_string());
        }
        None
    }

    pub(super) fn normalized_failure_reason(
        &mut self,
        run: &crate::state::RunRecord,
    ) -> Result<Option<String>> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let (has_artifact_check, artifact_reason) = match run.stage.as_str() {
            "brainstorm" => {
                let spec_path = session_dir.join("artifacts").join("spec.md");
                (
                    true,
                    (!Self::artifact_present(&spec_path)).then(|| "artifact_missing".to_string()),
                )
            }
            "spec-review" => {
                let review_path = session_dir
                    .join("artifacts")
                    .join(format!("spec-review-{}.md", run.round));
                (
                    true,
                    (!Self::artifact_present(&review_path)).then(|| "artifact_missing".to_string()),
                )
            }
            "planning" => {
                let plan_path = session_dir.join("artifacts").join("plan.md");
                (
                    true,
                    (!Self::artifact_present(&plan_path)).then(|| "artifact_missing".to_string()),
                )
            }
            "plan-review" => {
                let review_path = session_dir
                    .join("artifacts")
                    .join(format!("plan-review-{}.md", run.round));
                (
                    true,
                    (!Self::artifact_present(&review_path)).then(|| "artifact_missing".to_string()),
                )
            }
            "sharding" => {
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                let reason = if !Self::artifact_present(&tasks_path) {
                    Some("artifact_missing".to_string())
                } else {
                    tasks::validate(&tasks_path)
                        .err()
                        .map(|err| format!("artifact_invalid: {err}"))
                };
                (true, reason)
            }
            "recovery" => {
                let spec_path = session_dir.join("artifacts").join("spec.md");
                let plan_path = session_dir.join("artifacts").join("plan.md");
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                let recovery_path = session_dir
                    .join("rounds")
                    .join(format!("{:03}", run.round))
                    .join("recovery.toml");
                let reason = if !Self::artifact_present(&spec_path)
                    || !Self::artifact_present(&plan_path)
                    || !Self::artifact_present(&tasks_path)
                    || !Self::artifact_present(&recovery_path)
                {
                    Some("artifact_missing".to_string())
                } else if let Err(err) =
                    validate_stage_toml_writes(&session_dir, "recovery", run.round)
                {
                    Some(format!("artifact_invalid: {err}"))
                } else {
                    tasks::validate(&tasks_path)
                        .err()
                        .map(|err| format!("artifact_invalid: {err}"))
                };
                (true, reason)
            }
            "coder" => {
                // Coder's real deliverable is a git commit, not a file. We
                let round_dir = session_dir.join("rounds").join(format!("{:03}", run.round));
                (false, self.coder_gate_reason(run, &round_dir))
            }
            "reviewer" => {
                let review_path = session_dir
                    .join("rounds")
                    .join(format!("{:03}", run.round))
                    .join("review.toml");
                let reason = if !Self::artifact_present(&review_path) {
                    Some("artifact_missing".to_string())
                } else {
                    review::validate(&review_path)
                        .err()
                        .map(|err| format!("artifact_invalid: {err}"))
                };
                (true, reason)
            }
            _ => (false, None),
        };

        // If the stage produced a valid artifact, treat the run as successful
        // regardless of the wrapped pipeline's exit code. Agent commands like
        // `codex exec --json | jq ...` can return non-zero (e.g., a stray
        // non-JSON line from the agent makes jq exit 4/5) even after the
        // agent has already written a well-formed artifact. Warnings are
        // emitted for dirty-tree changes; a hard guard error (HEAD advance)
        // still fails the run.
        if has_artifact_check && artifact_reason.is_none() {
            if let Some(code) = self.read_exit_status_code(run)
                && code != 0
            {
                let _ = self.state.log_event(format!(
                    "run {} ({}) exited {code} but produced a valid artifact; treating as success",
                    run.id, run.stage
                ));
            }
            match self.enforce_run_guard(run) {
                guard::VerifyResult::Ok { warnings } => {
                    for w in warnings {
                        self.append_system_message(run.id, MessageKind::SummaryWarn, w);
                    }
                    return Ok(None);
                }
                guard::VerifyResult::HardError { reason, warnings } => {
                    for w in warnings {
                        self.append_system_message(run.id, MessageKind::SummaryWarn, w);
                    }
                    return Ok(Some(reason));
                }
                guard::VerifyResult::PendingDecision {
                    captured_head,
                    current_head,
                    warnings,
                } => {
                    // Park the run: populate pending decision and return Ok(None).
                    // Warnings are NOT appended yet — they replay at resolution time.
                    // The finalization caller detects the populated field and
                    // transitions to GitGuardPending instead of completing normally.
                    self.state.pending_guard_decision = Some(PendingGuardDecision {
                        stage: run.stage.clone(),
                        task_id: run.task_id,
                        round: run.round,
                        attempt: run.attempt,
                        run_id: run.id,
                        captured_head,
                        current_head,
                        warnings,
                    });
                    return Ok(None);
                }
            }
        }

        // No artifact (unknown stage) or artifact missing/invalid: exit code
        // takes precedence so the operator sees the real failure first.
        if let Some(code) = self.read_exit_status_code(run)
            && code != 0
        {
            if code > 128 {
                let signal_num = code - 128;
                let stamp_path = self.finish_stamp_path_for(run);
                let signal_received = crate::runner::read_finish_stamp(&stamp_path)
                    .map(|s| s.signal_received)
                    .unwrap_or_default();
                // Reviewer note: legacy stamps also deserialize to an empty
                // signal marker, so this branch means the wrapper recorded no
                // trapped signal, not that we can distinguish historical gaps.
                let detail = if signal_received.is_empty() {
                    format!("agent exited {code}")
                } else {
                    format!("wrapper trapped {signal_received}")
                };
                let log_suffix = if signal_received.is_empty() && code == 129 {
                    " (agent CLI exited 129 on its own; wrapper trapped no signal)"
                } else {
                    ""
                };
                let _ = self.state.log_event(format!(
                    "run {} ({}) exited {code}: signal_received={signal_received}{log_suffix}",
                    run.id, run.stage
                ));
                return Ok(Some(format!("killed({signal_num}) [{detail}]")));
            }
            return Ok(Some(format!("exit({code})")));
        }

        // Guard reason beats artifact reason (coder control-file edits are a
        // real protocol violation; non-coder HEAD advances are hard errors).
        // PendingDecision here means artifact was missing/invalid — the run is
        // already a failure from the artifact check, so treat it as no guard error.
        let (mut guard_reason, guard_warnings) = match self.enforce_run_guard(run) {
            guard::VerifyResult::Ok { warnings } => (None, warnings),
            guard::VerifyResult::HardError { reason, warnings } => (Some(reason), warnings),
            guard::VerifyResult::PendingDecision { warnings, .. } => (None, warnings),
        };
        for w in guard_warnings {
            self.append_system_message(run.id, MessageKind::SummaryWarn, w);
        }
        if run.stage == "coder"
            && artifact_reason.is_none()
            && run.modes.yolo
            && guard_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("forbidden_control_edit"))
        {
            self.log_yolo_auto_approved("path_violation");
            guard_reason = None;
        }
        Ok(guard_reason.or(artifact_reason))
    }

    pub(super) fn append_system_message(&mut self, run_id: u64, kind: MessageKind, text: String) {
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

    pub(super) fn emit_dirty_tree_warning(&mut self) {
        if let Some(run_id) = self.current_run_id {
            self.append_system_message(
                run_id,
                MessageKind::SummaryWarn,
                "working tree is dirty \u{2014} agent will run against uncommitted changes"
                    .to_string(),
            );
        }
    }

    pub(super) fn emit_selection_warning(&mut self, warning: Option<SelectionWarning>) {
        let Some(SelectionWarning::CheapFallback { phase, reason }) = warning else {
            return;
        };
        let message = format!("cheap_fallback: phase={} reason={reason}", phase.name());
        let _ = self.state.log_event(message.clone());
        self.push_status(message, status_line::Severity::Warn, Duration::from_secs(8));
    }

    pub(super) fn toggle_cheap_mode(&mut self, source: &str) {
        self.set_cheap_mode(!self.state.modes.cheap, source);
    }

    pub(super) fn set_cheap_mode(&mut self, value: bool, source: &str) {
        self.state.modes.cheap = value;
        if let Err(err) = self.state.save() {
            self.state.agent_error = Some(format!("failed to save cheap mode: {err:#}"));
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

    pub(super) fn ensure_builder_task_for_round(&mut self, round: u32) -> Option<u32> {
        let task_id = self.state.builder.ensure_task_for_round(round)?;
        let round_dir = session_state::session_dir(&self.state.session_id)
            .join("rounds")
            .join(format!("{round:03}"));
        let _ = std::fs::create_dir_all(&round_dir);
        Some(task_id)
    }

    /// Enter builder recovery.  Preserves `builder.done`/`builder.pending` and
    /// records recovery context.  `trigger` must be `"human_blocked"` or
    /// `"agent_pivot"`; `"human_blocked"` produces an interactive recovery stage.
    ///
    /// Circuit breaker: if `recovery_cycle_count` reaches 3 the trigger is
    /// automatically escalated to `"human_blocked"` and a pipeline message is
    /// emitted identifying the loop.
    ///
    /// Returns true so callers can treat this like other auto-retry paths.
    pub(super) fn enter_builder_recovery(
        &mut self,
        triggering_round: u32,
        trigger_task_id: Option<u32>,
        trigger_summary: Option<String>,
        trigger: &str,
    ) -> bool {
        if self.current_run_id.is_some() || self.window_launched {
            let _ = self.state.log_event(
                "enter_builder_recovery called while a run window is still marked active"
                    .to_string(),
            );
        }

        let session_dir = session_state::session_dir(&self.state.session_id);
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");
        let (prev_task_ids, prev_max) = tasks::validate(&tasks_path)
            .ok()
            .map(|f| {
                let ids = f.tasks.iter().map(|t| t.id).collect::<Vec<_>>();
                let max = ids.iter().copied().max();
                (ids, max)
            })
            .unwrap_or_default();

        // Circuit breaker: after 3 consecutive recovery cycles without an approved
        // plan review, force human_blocked so a human can break the loop.
        self.state.builder.recovery_cycle_count += 1;
        let effective_trigger = if self.state.builder.recovery_cycle_count >= 3
            && trigger != "human_blocked"
        {
            let loop_msg = format!(
                "recovery loop: {} consecutive recovery cycles without approval — escalating to human_blocked",
                self.state.builder.recovery_cycle_count
            );
            let _ = self.state.log_event(loop_msg.clone());
            let msg = Message {
                ts: chrono::Utc::now(),
                run_id: self.current_run_id.unwrap_or(0),
                kind: MessageKind::SummaryWarn,
                sender: MessageSender::System,
                text: loop_msg,
            };
            if let Err(err) = self.state.append_message(&msg) {
                let _ = self.state.log_event(format!(
                    "failed to append circuit-breaker escalation message: {err}"
                ));
            } else {
                self.messages.push(msg);
            }
            "human_blocked"
        } else {
            trigger
        };

        self.state.builder.recovery_trigger_task_id =
            trigger_task_id.or(self.state.builder.current_task_id());
        self.state.builder.recovery_prev_max_task_id = prev_max;
        self.state.builder.recovery_prev_task_ids = prev_task_ids;
        self.state.builder.recovery_trigger_summary = trigger_summary;
        if let Some(current_task_id) = self.state.builder.current_task_id() {
            let status = if self.state.builder.pipeline_items.is_empty() {
                PipelineItemStatus::Pending
            } else {
                PipelineItemStatus::Failed
            };
            let _ =
                self.state
                    .builder
                    .set_task_status(current_task_id, status, Some(triggering_round));
        }
        let interactive = effective_trigger == "human_blocked";
        let title = if interactive {
            "Human-blocked recovery"
        } else {
            "Agent pivot recovery"
        };
        self.state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "recovery".to_string(),
            task_id: None,
            round: Some(triggering_round),
            status: PipelineItemStatus::Running,
            title: Some(title.to_string()),
            mode: None,
            trigger: Some(effective_trigger.to_string()),
            interactive: Some(interactive),
        });
        self.state.agent_error = None;

        if let Err(err) = self.transition_to_phase(Phase::BuilderRecovery(triggering_round)) {
            self.state.agent_error = Some(format!("failed to enter builder recovery: {err}"));
            self.clear_builder_recovery_context();
            let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
        }
        true
    }

    pub(super) fn started_builder_task_ids(&self) -> BTreeSet<u32> {
        self.state
            .agent_runs
            .iter()
            .filter(|run| matches!(run.stage.as_str(), "coder" | "reviewer"))
            .filter_map(|run| run.task_id)
            .collect()
    }

    pub(super) fn recovery_notes_document_started_supersession(
        text: &str,
        superseded_ids: &BTreeSet<u32>,
    ) -> Result<()> {
        if !text.contains("Recovery Notes") {
            anyhow::bail!("missing required `Recovery Notes` section");
        }
        for id in superseded_ids {
            let needle = id.to_string();
            let mut found = false;
            for (idx, _) in text.match_indices(&needle) {
                // REVIEWER: spec requires superseded ids be explicitly named but does not
                // prescribe formatting; treat any standalone numeric token match as explicit.
                let prev = idx
                    .checked_sub(1)
                    .and_then(|p| text.as_bytes().get(p).copied())
                    .map(char::from);
                let next = text
                    .as_bytes()
                    .get(idx + needle.len())
                    .copied()
                    .map(char::from);
                let prev_digit = prev.is_some_and(|ch| ch.is_ascii_digit());
                let next_digit = next.is_some_and(|ch| ch.is_ascii_digit());
                if !prev_digit && !next_digit {
                    found = true;
                    break;
                }
            }
            if !found {
                anyhow::bail!("`Recovery Notes` missing superseded started task id {id}");
            }
        }
        Ok(())
    }

    pub(super) fn reconcile_builder_recovery(&mut self, recovery_run_id: u64) -> Result<()> {
        use anyhow::Context;

        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let parsed = tasks::validate(&tasks_path)
            .with_context(|| format!("invalid {}", tasks_path.display()))?;

        let done_ids = self
            .state
            .builder
            .done
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let started_ids = self.started_builder_task_ids();
        let prev_task_ids = self
            .state
            .builder
            .recovery_prev_task_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let historical_max = self
            .state
            .builder
            .recovery_prev_max_task_id
            .into_iter()
            .chain(done_ids.iter().copied())
            .chain(started_ids.iter().copied())
            .max()
            .unwrap_or(0);

        let recovered_ids = parsed.tasks.iter().map(|t| t.id).collect::<Vec<_>>();
        let recovered_set = recovered_ids.iter().copied().collect::<BTreeSet<_>>();

        if let Some(collision) = recovered_ids.iter().find(|id| done_ids.contains(id)) {
            anyhow::bail!("recovered unfinished tasks include completed task id {collision}");
        }

        let historical_ids = prev_task_ids
            .iter()
            .copied()
            .chain(done_ids.iter().copied())
            .chain(started_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        for id in &recovered_ids {
            if !historical_ids.contains(id) && *id <= historical_max {
                anyhow::bail!(
                    "new recovery task id {id} must be greater than prior max id {historical_max}"
                );
            }
        }

        let superseded_started = started_ids
            .difference(&done_ids)
            .copied()
            .collect::<BTreeSet<_>>()
            .difference(&recovered_set)
            .copied()
            .collect::<BTreeSet<_>>();
        if !superseded_started.is_empty() {
            let spec_text = std::fs::read_to_string(&spec_path)
                .with_context(|| format!("cannot read {}", spec_path.display()))?;
            Self::recovery_notes_document_started_supersession(&spec_text, &superseded_started)
                .with_context(|| format!("invalid {}", spec_path.display()))?;

            let plan_text = std::fs::read_to_string(&plan_path)
                .with_context(|| format!("cannot read {}", plan_path.display()))?;
            Self::recovery_notes_document_started_supersession(&plan_text, &superseded_started)
                .with_context(|| format!("invalid {}", plan_path.display()))?;
        }

        let completed_ids = self.state.builder.done_task_ids();
        let completed_set = completed_ids.iter().copied().collect::<BTreeSet<_>>();
        let mut next_items = self
            .state
            .builder
            .pipeline_items
            .iter()
            .filter(|item| {
                item.stage == "coder"
                    && item
                        .task_id
                        .is_some_and(|task_id| completed_set.contains(&task_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        if next_items.is_empty() {
            for task_id in &completed_ids {
                next_items.push(PipelineItem {
                    id: 0,
                    stage: "coder".to_string(),
                    task_id: Some(*task_id),
                    round: None,
                    status: PipelineItemStatus::Approved,
                    title: self.state.builder.task_titles.get(task_id).cloned(),
                    mode: None,
                    trigger: None,
                    interactive: None,
                });
            }
        }
        for task in &parsed.tasks {
            self.state
                .builder
                .task_titles
                .insert(task.id, task.title.clone());
            if !completed_set.contains(&task.id) {
                next_items.push(PipelineItem {
                    id: 0,
                    stage: "coder".to_string(),
                    task_id: Some(task.id),
                    round: None,
                    status: PipelineItemStatus::Pending,
                    title: Some(task.title.clone()),
                    mode: None,
                    trigger: None,
                    interactive: None,
                });
            }
        }
        self.state.builder.pipeline_items = next_items;
        self.state.builder.sync_legacy_queue_views();
        if let Some(item) = self
            .state
            .builder
            .pipeline_items
            .iter_mut()
            .rev()
            .find(|item| item.stage == "recovery" && item.status == PipelineItemStatus::Running)
        {
            item.status = PipelineItemStatus::Done;
        }
        self.state.builder.retry_reset_run_id_cutoff = Some(recovery_run_id);
        self.clear_builder_recovery_context();
        Ok(())
    }

    /// Called when a recovery-mode plan review agent run completes.
    ///
    /// Reads `artifacts/plan_review.toml`, applies the verdict, and either
    /// advances to recovery sharding (approved) or re-runs recovery
    /// (revise/human_blocked/agent_pivot with circuit-breaker).
    pub(super) fn handle_recovery_plan_review_completed(
        &mut self,
        run: &crate::state::RunRecord,
        round: u32,
    ) -> Result<()> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let plan_review_path = session_dir.join("artifacts").join("plan_review.toml");

        // Mark the recovery plan-review pipeline item as done/completed.
        if let Some(item) = self
            .state
            .builder
            .pipeline_items
            .iter_mut()
            .rev()
            .find(|i| i.stage == "plan-review" && i.status == PipelineItemStatus::Running)
        {
            item.status = PipelineItemStatus::Done;
        }

        match review::validate(&plan_review_path) {
            Ok(verdict) => {
                let summary_text = verdict.summary.trim().to_string();
                if !summary_text.is_empty() {
                    let kind = match verdict.status {
                        review::ReviewStatus::Approved => MessageKind::Summary,
                        _ => MessageKind::SummaryWarn,
                    };
                    let msg = Message {
                        ts: chrono::Utc::now(),
                        run_id: run.id,
                        kind,
                        sender: MessageSender::Agent {
                            model: run.model.clone(),
                            vendor: run.vendor.clone(),
                        },
                        text: summary_text,
                    };
                    if let Err(err) = self.state.append_message(&msg) {
                        let _ = self.state.log_event(format!(
                            "failed to append recovery plan review message: {err}"
                        ));
                    } else {
                        self.messages.push(msg);
                    }
                }
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                match verdict.status {
                    review::ReviewStatus::Approved | review::ReviewStatus::Refine => {
                        // Reset circuit-breaker: recovery reached an approved plan review.
                        // Refine is treated as approval here — recovery has no
                        // "next implementation" to carry forward to.
                        self.state.builder.recovery_cycle_count = 0;
                        self.queue_recovery_sharding_pipeline_item(round);
                        self.transition_to_phase(Phase::BuilderRecoverySharding(round))?;
                    }
                    review::ReviewStatus::Revise
                    | review::ReviewStatus::HumanBlocked
                    | review::ReviewStatus::AgentPivot => {
                        let trigger_str = match verdict.status {
                            review::ReviewStatus::HumanBlocked => "human_blocked",
                            review::ReviewStatus::AgentPivot => "agent_pivot",
                            _ => "agent_pivot",
                        };
                        let summary = verdict.feedback.join("\n");
                        let trigger_summary = (!summary.trim().is_empty()).then_some(summary);
                        self.enter_builder_recovery(round, None, trigger_summary, trigger_str);
                    }
                }
            }
            Err(err) => {
                let reason = format!("recovery_plan_review_failed: {err:#}");
                self.finalize_run_record(run.id, false, Some(reason.clone()));
                let failed_run = self
                    .state
                    .agent_runs
                    .iter()
                    .find(|r| r.id == run.id)
                    .cloned()
                    .unwrap_or_else(|| run.clone());
                if !self.maybe_auto_retry(&failed_run) {
                    self.state.agent_error = Some(reason);
                }
            }
        }
        Ok(())
    }

    /// Called when a recovery-mode sharding agent run completes.
    ///
    /// Validates the regenerated `tasks.toml` against the completed task history,
    /// rebuilds the pipeline queue, and advances to the next implementation round.
    pub(super) fn handle_recovery_sharding_completed(
        &mut self,
        run: &crate::state::RunRecord,
        round: u32,
    ) -> Result<()> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");

        // Mark the recovery sharding pipeline item as done.
        if let Some(item) = self
            .state
            .builder
            .pipeline_items
            .iter_mut()
            .rev()
            .find(|i| i.stage == "sharding" && i.status == PipelineItemStatus::Running)
        {
            item.status = PipelineItemStatus::Done;
        }

        match tasks::validate(&tasks_path) {
            Ok(parsed) => {
                let done_ids = self
                    .state
                    .builder
                    .done_task_ids()
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>();

                // Enforce: every new task id must be strictly greater than the
                // highest id ever seen (completed, started, or in any recovery
                // snapshot). Prevents reuse of ids that carry historical state
                // — not just the no-collision-with-completed weaker check.
                let max_seen = self.state.builder.max_task_id();
                for task in &parsed.tasks {
                    if task.id <= max_seen {
                        let reason = format!(
                            "recovery sharding produced task id {} but new ids must be > {} (max id ever seen)",
                            task.id, max_seen
                        );
                        self.finalize_run_record(run.id, false, Some(reason.clone()));
                        self.state.agent_error = Some(reason);
                        self.clear_builder_recovery_context();
                        let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
                        return Ok(());
                    }
                }

                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;

                // Rebuild pipeline: completed tasks stay as-is, add pending from recovered tasks.
                let mut next_items: Vec<PipelineItem> = self
                    .state
                    .builder
                    .pipeline_items
                    .iter()
                    .filter(|item| {
                        item.stage == "coder"
                            && item.task_id.is_some_and(|id| done_ids.contains(&id))
                    })
                    .cloned()
                    .collect();

                if next_items.is_empty() {
                    for &tid in &done_ids {
                        next_items.push(PipelineItem {
                            id: 0,
                            stage: "coder".to_string(),
                            task_id: Some(tid),
                            round: None,
                            status: PipelineItemStatus::Approved,
                            title: self.state.builder.task_titles.get(&tid).cloned(),
                            mode: None,
                            trigger: None,
                            interactive: None,
                        });
                    }
                }

                for task in &parsed.tasks {
                    self.state
                        .builder
                        .task_titles
                        .insert(task.id, task.title.clone());
                    if !done_ids.contains(&task.id) {
                        next_items.push(PipelineItem {
                            id: 0,
                            stage: "coder".to_string(),
                            task_id: Some(task.id),
                            round: None,
                            status: PipelineItemStatus::Pending,
                            title: Some(task.title.clone()),
                            mode: None,
                            trigger: None,
                            interactive: None,
                        });
                    }
                }
                self.state.builder.pipeline_items = next_items;
                self.state.builder.sync_legacy_queue_views();

                let pipeline_msg = format!(
                    "recovery sharding complete: {} pending tasks",
                    self.state.builder.pending_task_ids().len()
                );
                self.append_system_message(run.id, MessageKind::Summary, pipeline_msg);

                self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
            }
            Err(err) => {
                let reason = format!("recovery_sharding_failed: {err:#}");
                self.finalize_run_record(run.id, false, Some(reason.clone()));
                let failed_run = self
                    .state
                    .agent_runs
                    .iter()
                    .find(|r| r.id == run.id)
                    .cloned()
                    .unwrap_or_else(|| run.clone());
                if !self.maybe_auto_retry(&failed_run) {
                    self.state.agent_error = Some(reason);
                }
            }
        }
        Ok(())
    }

    /// Launch the non-interactive recovery-mode plan review agent.
    pub(super) fn finalize_run_record(
        &mut self,
        run_id: u64,
        success: bool,
        error: Option<String>,
    ) {
        let Some(run) = self
            .state
            .agent_runs
            .iter_mut()
            .find(|run| run.id == run_id)
        else {
            return;
        };
        let ended_at = chrono::Utc::now();
        run.ended_at = Some(ended_at);
        let unverified = error
            .as_deref()
            .is_some_and(|reason| reason.starts_with("failed_unverified:"));
        run.status = if success {
            RunStatus::Done
        } else if unverified {
            RunStatus::FailedUnverified
        } else {
            RunStatus::Failed
        };
        run.error = error.clone();

        let duration = ended_at.signed_duration_since(run.started_at);
        let total_seconds = duration.num_seconds().max(0);
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        let text = if success {
            format!(
                "done in {minutes}m{seconds:02}s · {} ({})",
                run.model, run.vendor
            )
        } else if unverified {
            format!(
                "attempt {} unverified: {}",
                run.attempt,
                error.unwrap_or_else(|| "unknown error".to_string())
            )
        } else {
            format!(
                "attempt {} failed: {}",
                run.attempt,
                error.unwrap_or_else(|| "unknown error".to_string())
            )
        };
        let message = Message {
            ts: ended_at,
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

    pub(super) fn retry_exhausted_summary(&self, failed_run: &crate::state::RunRecord) -> String {
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

    pub(super) fn maybe_auto_retry(&mut self, failed_run: &crate::state::RunRecord) -> bool {
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
                self.state.agent_error = Some(summary.clone());
                let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
                self.append_system_message(failed_run.id, MessageKind::End, summary);
                return true;
            }

            self.state.agent_error = Some(summary.clone());
            let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
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
            self.state.agent_error = Some(summary.clone());
            let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
            self.append_system_message(failed_run.id, MessageKind::End, summary);
            return true;
        }

        self.state.agent_error = Some(summary.clone());
        let _ = self.transition_to_phase(Phase::BlockedNeedsUser);
        self.append_system_message(failed_run.id, MessageKind::End, summary);
        true
    }

    pub(super) fn finalize_current_run(&mut self, run: &crate::state::RunRecord) -> Result<()> {
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

    pub(super) fn complete_run_finalization(
        &mut self,
        run: &crate::state::RunRecord,
        failure_reason: Option<String>,
    ) -> Result<()> {
        use anyhow::Context;

        let session_dir = session_state::session_dir(&self.state.session_id);
        if let Some(error) = failure_reason {
            self.finalize_run_record(run.id, false, Some(error.clone()));
            let failed_run = self
                .state
                .agent_runs
                .iter()
                .find(|candidate| candidate.id == run.id)
                .cloned()
                .unwrap_or_else(|| run.clone());
            if !self.maybe_auto_retry(&failed_run) {
                self.state.agent_error = Some(error);
            }
            return Ok(());
        }
        match self.state.current_phase {
            Phase::BrainstormRunning => {
                let skip_artifact_path = session_dir
                    .join("artifacts")
                    .join(ArtifactKind::SkipToImpl.filename());
                let proposal = match SkipToImplProposal::read_from_path(&skip_artifact_path) {
                    Ok((p, warnings)) => {
                        for w in warnings {
                            let _ = self
                                .state
                                .log_event(format!("warning: skip_proposal.toml: {w}"));
                        }
                        p
                    }
                    Err(err) => {
                        let _ = self.state.log_event(format!(
                            "warning: skip_proposal.toml malformed or invalid, falling through to spec review: {err:#}"
                        ));
                        None
                    }
                };

                let summary_path = session_dir
                    .join("artifacts")
                    .join(ArtifactKind::SessionSummary.filename());
                match crate::artifacts::SessionSummaryArtifact::read_from_path(&summary_path) {
                    Ok(Some(summary)) => {
                        self.state.title = Some(summary.title.trim().to_string());
                    }
                    Ok(None) => {}
                    Err(err) => {
                        let _ = self.state.log_event(format!(
                            "warning: session_summary.toml malformed or invalid, leaving title unset: {err:#}"
                        ));
                    }
                }

                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;

                match proposal {
                    Some(p) if p.proposed => {
                        self.state.skip_to_impl_rationale = Some(p.rationale);
                        self.state.skip_to_impl_kind = Some(p.status);
                        self.transition_to_phase(Phase::SkipToImplPending)?;
                    }
                    _ => {
                        self.transition_to_phase(Phase::SpecReviewRunning)?;
                    }
                }
            }
            Phase::SpecReviewRunning => {
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                self.transition_to_phase(Phase::SpecReviewPaused)?;
                self.append_system_message(
                    run.id,
                    MessageKind::Summary,
                    "Spec review complete.".to_string(),
                );
                if run.modes.yolo {
                    self.auto_approve_spec_review_pause("spec_approval");
                }
            }
            Phase::PlanningRunning => {
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                // Spec line 46 conjoins yolo plan-review skip with `artifacts/plan.md` existing.
                // The successful-finalization context already implies the artifact, but the
                // explicit guard protects against a planning agent that reports success
                // without writing the file.
                let plan_path = session_state::session_dir(&self.state.session_id)
                    .join("artifacts")
                    .join("plan.md");
                if run.modes.yolo && Self::artifact_present(&plan_path) {
                    self.log_yolo_auto_approved("plan_review_skipped");
                    self.transition_to_phase(Phase::ShardingRunning)?;
                } else {
                    self.transition_to_phase(Phase::PlanReviewRunning)?;
                }
            }
            Phase::PlanReviewRunning => {
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                self.transition_to_phase(Phase::PlanReviewPaused)?;
                self.append_system_message(
                    run.id,
                    MessageKind::Summary,
                    "Plan review complete.".to_string(),
                );
                if run.modes.yolo {
                    self.auto_approve_plan_review_pause("plan_approval");
                }
            }
            Phase::ShardingRunning => {
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                let parsed = tasks::validate(&tasks_path)
                    .with_context(|| format!("invalid {}", tasks_path.display()));
                match parsed {
                    Ok(parsed) => {
                        self.state.builder.task_titles = parsed
                            .tasks
                            .iter()
                            .map(|t| (t.id, t.title.clone()))
                            .collect();
                        self.state.builder.reset_task_pipeline(
                            parsed
                                .tasks
                                .iter()
                                .map(|task| (task.id, Some(task.title.clone()))),
                        );
                        self.finalize_run_record(run.id, true, None);
                        self.state.agent_error = None;
                        self.transition_to_phase(Phase::ImplementationRound(1))?;
                    }
                    Err(err) => return Err(err),
                }
            }
            Phase::ImplementationRound(round) => {
                let round_dir = session_dir.join("rounds").join(format!("{round:03}"));
                let scope = read_review_scope(&round_dir.join("review_scope.toml"))?;
                let _ = write_review_scope_artifact(&round_dir, &scope.base_sha);
                self.finalize_run_record(run.id, true, None);
                self.state.agent_error = None;
                self.transition_to_phase(Phase::ReviewRound(round))?;
            }
            Phase::ReviewRound(round) => {
                let review_path = session_dir
                    .join("rounds")
                    .join(format!("{round:03}"))
                    .join("review.toml");
                match review::validate(&review_path) {
                    Ok(verdict) => {
                        let summary_text = verdict.summary.trim();
                        if !summary_text.is_empty() {
                            let kind = match verdict.status {
                                review::ReviewStatus::Approved | review::ReviewStatus::Refine => {
                                    MessageKind::Summary
                                }
                                review::ReviewStatus::Revise
                                | review::ReviewStatus::HumanBlocked
                                | review::ReviewStatus::AgentPivot => MessageKind::SummaryWarn,
                            };
                            let msg = Message {
                                ts: chrono::Utc::now(),
                                run_id: run.id,
                                kind,
                                sender: MessageSender::Agent {
                                    model: run.model.clone(),
                                    vendor: run.vendor.clone(),
                                },
                                text: summary_text.to_string(),
                            };
                            if let Err(err) = self.state.append_message(&msg) {
                                let _ = self.state.log_event(format!(
                                    "failed to append review summary message for run {}: {err}",
                                    run.id
                                ));
                            } else {
                                self.messages.push(msg);
                            }
                        }
                        self.finalize_run_record(run.id, true, None);
                        self.state.agent_error = None;
                        self.state.builder.last_verdict =
                            Some(format!("{:?}", verdict.status).to_lowercase());
                        match verdict.status {
                            review::ReviewStatus::Approved => {
                                // Advisory feedback on an approved verdict is non-blocking;
                                // surface it to the UI but continue the pipeline.
                                if !verdict.feedback.is_empty() {
                                    let advisory = format!(
                                        "advisory ({}): {}",
                                        verdict.feedback.len(),
                                        verdict.feedback[0].trim()
                                    );
                                    let advisory_msg = Message {
                                        ts: chrono::Utc::now(),
                                        run_id: run.id,
                                        kind: MessageKind::SummaryWarn,
                                        sender: MessageSender::Agent {
                                            model: run.model.clone(),
                                            vendor: run.vendor.clone(),
                                        },
                                        text: advisory,
                                    };
                                    if let Err(err) = self.state.append_message(&advisory_msg) {
                                        let _ = self.state.log_event(format!(
                                            "failed to append advisory feedback message: {err}"
                                        ));
                                    } else {
                                        self.messages.push(advisory_msg);
                                    }
                                }
                                if let Some(task_id) = self.state.builder.current_task_id() {
                                    let _ = self.state.builder.set_task_status(
                                        task_id,
                                        PipelineItemStatus::Approved,
                                        Some(round),
                                    );
                                }
                                if !self.state.builder.has_unfinished_tasks() {
                                    self.transition_to_phase(Phase::Done)?;
                                } else {
                                    self.transition_to_phase(Phase::ImplementationRound(
                                        round + 1,
                                    ))?;
                                }
                            }
                            review::ReviewStatus::Refine => {
                                // Approve the current task and stash feedback for
                                // the next coder. No re-review of this round.
                                self.state
                                    .builder
                                    .pending_refine_feedback
                                    .extend(verdict.feedback.iter().cloned());
                                if let Some(task_id) = self.state.builder.current_task_id() {
                                    let _ = self.state.builder.set_task_status(
                                        task_id,
                                        PipelineItemStatus::Approved,
                                        Some(round),
                                    );
                                }
                                if !self.state.builder.has_unfinished_tasks() {
                                    self.transition_to_phase(Phase::Done)?;
                                } else {
                                    self.transition_to_phase(Phase::ImplementationRound(
                                        round + 1,
                                    ))?;
                                }
                            }
                            review::ReviewStatus::Revise => {
                                if let Some(task_id) = self.state.builder.current_task_id() {
                                    if verdict.new_tasks.is_empty() {
                                        let _ = self.state.builder.set_task_status(
                                            task_id,
                                            PipelineItemStatus::Revise,
                                            Some(round),
                                        );
                                    } else {
                                        let new_tasks = verdict
                                            .new_tasks
                                            .iter()
                                            .map(|task| {
                                                (
                                                    task.title.clone(),
                                                    task.description.clone(),
                                                    task.test.clone(),
                                                    task.estimated_tokens,
                                                )
                                            })
                                            .collect::<Vec<_>>();
                                        let assigned_ids = assigned_revise_task_ids(
                                            &self.state.builder,
                                            new_tasks.len(),
                                        );
                                        rewrite_tasks_for_revise(
                                            &session_dir,
                                            task_id,
                                            &verdict.new_tasks,
                                            &assigned_ids,
                                        )?;
                                        self.state
                                            .builder
                                            .apply_revise_with_new_tasks(task_id, new_tasks);
                                        if let Some(first_inserted) = assigned_ids.first().copied()
                                        {
                                            self.state.builder.current_task = Some(first_inserted);
                                        }
                                    }
                                }
                                self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
                            }
                            review::ReviewStatus::HumanBlocked
                            | review::ReviewStatus::AgentPivot => {
                                let (verdict_status, trigger_str) = match verdict.status {
                                    review::ReviewStatus::HumanBlocked => {
                                        (PipelineItemStatus::HumanBlocked, "human_blocked")
                                    }
                                    review::ReviewStatus::AgentPivot => {
                                        (PipelineItemStatus::AgentPivot, "agent_pivot")
                                    }
                                    review::ReviewStatus::Approved
                                    | review::ReviewStatus::Refine
                                    | review::ReviewStatus::Revise => {
                                        // SAFETY: the enclosing outer match arm at :3196 only matches
                                        // `HumanBlocked | AgentPivot`, so the other ReviewStatus
                                        // variants cannot reach this inner match.
                                        unreachable!("already handled")
                                    }
                                };
                                if let Some(task_id) = self.state.builder.current_task_id() {
                                    let _ = self.state.builder.set_task_status(
                                        task_id,
                                        verdict_status,
                                        Some(round),
                                    );
                                }
                                let summary = verdict.feedback.join("\n");
                                let trigger_summary =
                                    (!summary.trim().is_empty()).then_some(summary);
                                self.enter_builder_recovery(
                                    round,
                                    self.state.builder.current_task_id(),
                                    trigger_summary,
                                    trigger_str,
                                );
                            }
                        }
                    }
                    Err(err) => return Err(err),
                }
            }
            Phase::BuilderRecovery(round) => match self.reconcile_builder_recovery(run.id) {
                Ok(()) => {
                    self.finalize_run_record(run.id, true, None);
                    self.state.agent_error = None;
                    if run.modes.yolo {
                        // Recovery has already validated `recovery.toml`/`tasks.toml`; yolo
                        // delegates the review gate, not the artifact validation step.
                        self.log_yolo_auto_approved("recovery_plan_review_skipped");
                        self.queue_recovery_sharding_pipeline_item(round);
                        self.transition_to_phase(Phase::BuilderRecoverySharding(round))?;
                    } else {
                        // Insert the recovery-mode plan review pipeline item before
                        // transitioning so the UI shows it as the next pending stage.
                        self.state.builder.push_pipeline_item(PipelineItem {
                            id: 0,
                            stage: "plan-review".to_string(),
                            task_id: None,
                            round: Some(round),
                            status: PipelineItemStatus::Pending,
                            title: Some("Recovery plan review".to_string()),
                            mode: Some("recovery".to_string()),
                            trigger: None,
                            interactive: Some(false),
                        });
                        self.transition_to_phase(Phase::BuilderRecoveryPlanReview(round))?;
                    }
                }
                Err(err) => {
                    let reason = format!("recovery_reconcile_failed: {err:#}");
                    self.finalize_run_record(run.id, false, Some(reason.clone()));
                    let failed_run = self
                        .state
                        .agent_runs
                        .iter()
                        .find(|candidate| candidate.id == run.id)
                        .cloned()
                        .unwrap_or_else(|| run.clone());
                    if !self.maybe_auto_retry(&failed_run) {
                        self.state.agent_error = Some(reason);
                    }
                }
            },
            Phase::BuilderRecoveryPlanReview(round) => {
                self.handle_recovery_plan_review_completed(run, round)?;
            }
            Phase::BuilderRecoverySharding(round) => {
                self.handle_recovery_sharding_completed(run, round)?;
            }
            Phase::IdeaInput
            | Phase::SpecReviewPaused
            | Phase::PlanReviewPaused
            | Phase::BlockedNeedsUser
            | Phase::SkipToImplPending
            | Phase::GitGuardPending
            | Phase::Done => {}
        }
        Ok(())
    }
}
