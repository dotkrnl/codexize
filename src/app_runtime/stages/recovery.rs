use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::prompts::recovery_prompt;
use crate::app::{App, guard};
use crate::selection::CachedModel;
use crate::state::{self as session_state, Phase, PipelineItemStatus};
use anyhow::{Context, Result};
impl App {
    pub(crate) fn launch_recovery(&mut self) {
        let _ = self.launch_recovery_with_model(None);
    }
    pub(crate) fn launch_recovery_with_model(
        &mut self,
        override_model: Option<CachedModel>,
    ) -> bool {
        self.clear_agent_error();
        if !self.guard_models_loaded() {
            return false;
        }
        let Phase::BuilderRecovery(round) = self.state.current_phase else {
            return false;
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let recovery_path = session_dir
            .join("rounds")
            .join(format!("{round:03}"))
            .join("recovery.toml");
        let _ = std::fs::remove_file(&recovery_path);
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("recovery-r{round}.md"));
        let modes = self.state.launch_modes();
        let phase = Self::phase_for_stage("recovery");
        let effort = modes.effort_for(EffortLevel::Normal, phase);
        let Some(chosen) =
            self.choose_primary_model(override_model.as_ref(), phase, effort, modes.cheap)
        else {
            self.record_agent_error("no model available with quota".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, subscription_tag, cli, launch_name, effort_mapping, effort_eligible) = chosen;
        let is_human_blocked = self
            .state
            .builder
            .pipeline_items_by_stage("recovery")
            .iter()
            .find(|i| i.status == PipelineItemStatus::Running)
            .and_then(|i| i.trigger.as_deref())
            == Some("human_blocked");
        let completed = self.state.builder.done_task_ids();
        let mut started = self
            .started_builder_task_ids()
            .into_iter()
            .collect::<Vec<_>>();
        started.sort_unstable();
        let attempt = self.attempt_for("recovery", None, round);
        let live_summary_path = self.live_summary_path_for_run("recovery", None, round, attempt);
        let prior_attempts_path = crate::app::prior_attempts::write_prior_attempts_transcript(
            &session_dir,
            &self.messages,
            &self.state.agent_runs,
            "recovery",
            round,
        );
        let prompt = recovery_prompt(
            &spec_path,
            &plan_path,
            &tasks_path,
            self.state.builder.recovery_trigger_task_id,
            self.state.builder.recovery_trigger_summary.as_deref(),
            &completed,
            &started,
            &live_summary_path,
            &recovery_path,
            is_human_blocked,
            prior_attempts_path.as_deref(),
            self.prompt_meta(),
        );
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt)
            .with_context(|| format!("cannot write {}", prompt_path.display()))
        {
            self.record_agent_error(err.to_string());
            return false;
        }
        let run = AgentRun {
            model: model.clone(),
            cli,
            launch_name,
            prompt_path: prompt_path.clone(),
            effort,
            effort_mapping: effort_mapping.clone(),
            effort_eligible,
            modes,
        };
        let recovery_guard_mode = if is_human_blocked {
            guard::GuardMode::AskOperator
        } else {
            guard::GuardMode::AutoReset
        };
        let dirty = self.capture_run_guard("recovery", None, round, attempt, recovery_guard_mode);
        let window_name = run_label_with_model(
            "[Recovery]",
            &model,
            effort,
            effort_eligible,
            &effort_mapping,
        );
        let run_id = self.state.next_agent_run_id();
        let run_key = Self::run_key_for("recovery", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&recovery_path), &run_key, &artifacts_dir)
        {
            result
        } else if is_human_blocked {
            self.runner_supervisor.launch_interactive_with_policy(
                run_id,
                &window_name,
                &run,
                &run_key,
                &artifacts_dir,
                Some(&recovery_path),
                self.default_acp_policy(),
            )
        } else {
            self.runner_supervisor.launch_noninteractive_with_policy(
                run_id,
                &window_name,
                &run,
                &run_key,
                &artifacts_dir,
                Some(&recovery_path),
                self.default_acp_policy(),
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    run_id,
                    "recovery",
                    None,
                    round,
                    model,
                    subscription_tag,
                    window_name,
                    effort,
                    effort_mapping,
                    effort_eligible,
                    modes,
                    prompt_path,
                );
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(err) => {
                self.record_agent_error(format!("failed to launch recovery: {err}"));
                false
            }
        }
    }
    /// Co-located success-finalization for `Phase::BuilderRecovery(round)`.
    ///
    /// Runs `reconcile_builder_recovery` and then either advances to recovery
    /// plan-review (or sharding under yolo) or surfaces the failure and lets
    /// the auto-retry policy take over.
    pub(crate) fn finalize_recovery_success(
        &mut self,
        run: &crate::state::RunRecord,
        round: u32,
    ) -> Result<()> {
        match self.reconcile_builder_recovery(run.id) {
            Ok(()) => {
                self.finalize_run_record(run.id, true, None);
                self.clear_agent_error();
                if run.modes.yolo {
                    // Recovery has already validated `recovery.toml`/`tasks.toml`; yolo
                    // delegates the review gate, not the artifact validation step.
                    self.log_yolo_auto_approved("recovery_plan_review_skipped");
                    self.queue_recovery_sharding_pipeline_item(round);
                    self.transition_to_phase(Phase::BuilderRecoverySharding(round))?;
                } else {
                    // Insert the recovery-mode plan review pipeline item before
                    // transitioning so the UI shows it as the next pending stage.
                    session_state::queue_recovery_plan_review(&mut self.state, round);
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
                    self.record_agent_error(reason);
                }
            }
        }
        Ok(())
    }
}
