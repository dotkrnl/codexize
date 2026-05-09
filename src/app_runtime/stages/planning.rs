use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::prompts::planning_prompt;
use crate::app::{App, guard};
use crate::selection::CachedModel;
use crate::state::{self as session_state, Phase, RunStatus};
use anyhow::Result;
impl App {
    pub(crate) fn launch_planning(&mut self) {
        let _ = self.launch_planning_with_model(None, true);
    }
    pub(crate) fn launch_planning_with_model(
        &mut self,
        override_model: Option<CachedModel>,
        interactive: bool,
    ) -> bool {
        self.clear_agent_error();
        if self.models.is_empty() {
            self.record_agent_error(
                "model list not yet loaded — wait a moment and try again".to_string(),
            );
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }
        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");
        let review_paths: Vec<std::path::PathBuf> = self
            .state
            .agent_runs
            .iter()
            .filter(|run| run.stage == "spec-review" && run.status == RunStatus::Done)
            .map(|run| {
                session_dir
                    .join("artifacts")
                    .join(format!("spec-review-{}.md", run.round))
            })
            .filter(|path| path.exists())
            .collect();
        let modes = self.state.launch_modes();
        let phase = Self::phase_for_stage("planning");
        let effort = modes.effort_for(EffortLevel::Normal, phase);
        let Some(chosen) =
            self.choose_primary_model(override_model.as_ref(), phase, effort, modes.cheap)
        else {
            self.record_agent_error("no model available with quota".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor, cli, launch_name) = chosen;
        let _ = std::fs::remove_file(&plan_path);
        let prompt_path = session_dir.join("prompts").join("planning.md");
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let attempt = self.attempt_for("planning", None, 1);
        let live_summary_path = self.live_summary_path_for_run("planning", None, 1, attempt);
        let prior_attempts_path = crate::app::prior_attempts::write_prior_attempts_transcript(
            &session_dir,
            &self.messages,
            &self.state.agent_runs,
            "planning",
            1,
        );
        let prompt = planning_prompt(
            &spec_path,
            &review_paths,
            &plan_path,
            &live_summary_path,
            modes.yolo,
            prior_attempts_path.as_deref(),
            self.prompt_meta(),
        );
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.surface_boundary_error(format!("error writing prompt: {e}"), true);
            return false;
        }
        let run = AgentRun {
            model: model.clone(),
            cli,
            launch_name,
            prompt_path: prompt_path.clone(),
            effort,
            modes,
        };
        // YOLO planning must take the existing non-interactive runner path.
        let interactive = interactive && !modes.yolo;
        let guard_mode = if interactive {
            guard::GuardMode::AskOperator
        } else {
            guard::GuardMode::AutoReset
        };
        let dirty = self.capture_run_guard("planning", None, 1, attempt, guard_mode);
        let window_name = run_label_with_model("[Planning]", &model, vendor_kind, effort);
        let run_id = self.state.next_agent_run_id();
        let run_key = Self::run_key_for("planning", None, 1, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&plan_path), &run_key, &artifacts_dir)
        {
            result
        } else if interactive {
            self.runner_supervisor.launch_interactive_with_policy(
                run_id,
                &window_name,
                &run,
                vendor_kind,
                &run_key,
                &artifacts_dir,
                Some(&plan_path),
                self.default_acp_policy(),
            )
        } else {
            self.runner_supervisor.launch_noninteractive_with_policy(
                run_id,
                &window_name,
                &run,
                vendor_kind,
                &run_key,
                &artifacts_dir,
                Some(&plan_path),
                self.default_acp_policy(),
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    run_id,
                    "planning",
                    None,
                    1,
                    model,
                    vendor,
                    window_name,
                    effort,
                    modes,
                    prompt_path,
                );
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(e) => {
                self.surface_boundary_error(format!("failed to launch planning: {e}"), true);
                false
            }
        }
    }
    /// Co-located success-finalization for `Phase::PlanningRunning`.
    ///
    /// Spec line 46 conjoins yolo plan-review skip with `artifacts/plan.md`
    /// existing. The successful-finalization context already implies the
    /// artifact, but the explicit guard protects against a planning agent
    /// that reports success without writing the file.
    pub(crate) fn finalize_planning_success(
        &mut self,
        run: &crate::state::RunRecord,
    ) -> Result<()> {
        self.finalize_run_record(run.id, true, None);
        self.clear_agent_error();
        let plan_path = session_state::session_dir(&self.state.session_id)
            .join("artifacts")
            .join("plan.md");
        if run.modes.yolo && Self::artifact_present(&plan_path) {
            self.log_yolo_auto_approved("plan_review_skipped");
            self.transition_to_phase(Phase::ShardingRunning)?;
        } else {
            self.transition_to_phase(Phase::PlanReviewRunning)?;
        }
        Ok(())
    }
}
