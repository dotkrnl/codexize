use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::prompts::sharding_prompt;
use crate::app::{App, guard};
use crate::selection::CachedModel;
use crate::state::{self as session_state, Phase};
use crate::tasks;
use anyhow::{Context, Result};
impl App {
    pub(crate) fn launch_sharding(&mut self) {
        let _ = self.launch_sharding_with_model(None);
    }
    pub(crate) fn launch_sharding_with_model(
        &mut self,
        override_model: Option<CachedModel>,
    ) -> bool {
        self.clear_agent_error();
        if !self.guard_models_loaded() {
            return false;
        }
        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");
        let modes = self.state.launch_modes();
        let phase = Self::phase_for_stage("sharding");
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
        let _ = std::fs::remove_file(&tasks_path);
        let prompt_path = session_dir.join("prompts").join("sharding.md");
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let attempt = self.attempt_for("sharding", None, 1);
        let live_summary_path = self.live_summary_path_for_run("sharding", None, 1, attempt);
        let prompt = sharding_prompt(
            &spec_path,
            &plan_path,
            &tasks_path,
            &live_summary_path,
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
            effort_mapping: effort_mapping.clone(),
            effort_eligible,
            modes,
        };
        let dirty =
            self.capture_run_guard("sharding", None, 1, attempt, guard::GuardMode::AutoReset);
        let window_name = run_label_with_model(
            "[Sharding]",
            &model,
            effort,
            effort_eligible,
            &effort_mapping,
        );
        let run_id = self.state.next_agent_run_id();
        let run_key = Self::run_key_for("sharding", None, 1, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&tasks_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            self.runner_supervisor.launch_noninteractive_with_policy(
                run_id,
                &window_name,
                &run,
                &run_key,
                &artifacts_dir,
                Some(&tasks_path),
                self.default_acp_policy(),
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    run_id,
                    "sharding",
                    None,
                    1,
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
            Err(e) => {
                self.surface_boundary_error(format!("failed to launch sharding: {e}"), true);
                false
            }
        }
    }
    /// Co-located success-finalization for `Phase::ShardingRunning`.
    pub(crate) fn finalize_sharding_success(
        &mut self,
        run: &crate::state::RunRecord,
    ) -> Result<()> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");
        let parsed = tasks::validate(&tasks_path)
            .with_context(|| format!("invalid {}", tasks_path.display()))?;
        session_state::initialize_task_pipeline(
            &mut self.state,
            parsed
                .tasks
                .iter()
                .map(|task| (task.id, task.title.clone())),
        );
        self.finalize_run_record(run.id, true, None);
        self.clear_agent_error();
        self.transition_to_phase(Phase::ImplementationRound(1))?;
        Ok(())
    }
}
