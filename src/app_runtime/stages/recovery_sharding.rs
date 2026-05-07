use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::prompts::recovery_sharding_prompt;
use crate::app::{App, guard};
use crate::selection::CachedModel;
use crate::state::{self as session_state, Phase};
use anyhow::Context;
impl App {
    /// Launch the non-interactive recovery-mode sharding agent.
    pub(crate) fn launch_recovery_sharding(&mut self) {
        let _ = self.launch_recovery_sharding_with_model(None);
    }
    pub(crate) fn launch_recovery_sharding_with_model(
        &mut self,
        override_model: Option<CachedModel>,
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
        let Phase::BuilderRecoverySharding(round) = self.state.current_phase else {
            return false;
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let _ = std::fs::remove_file(&tasks_path);
        let attempt = self.attempt_for("sharding", None, round);
        let live_summary_path = self.live_summary_path_for_run("sharding", None, round, attempt);
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("recovery-sharding-r{round}.md"));
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
        let (model, vendor_kind, vendor, route_provider) = chosen;
        let completed = self.state.builder.done_task_ids();
        let id_floor = self.state.builder.max_task_id();
        let prompt = recovery_sharding_prompt(
            &spec_path,
            &plan_path,
            &live_summary_path,
            &tasks_path,
            &completed,
            id_floor,
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
        session_state::transitions::mark_latest_pipeline_stage_running(&mut self.state, "sharding");
        let run = AgentRun {
            model: model.clone(),
            route_provider: route_provider.clone(),
            prompt_path: prompt_path.clone(),
            effort,
            modes,
        };
        let dirty = self.capture_run_guard(
            "sharding",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let window_name = run_label_with_model("[Recovery Sharding]", &model, vendor_kind, effort);
        let run_id = self.state.next_agent_run_id();
        let run_key = Self::run_key_for("sharding", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&tasks_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            self.runner_supervisor.launch_noninteractive(
                run_id,
                &window_name,
                &run,
                vendor_kind,
                &run_key,
                &artifacts_dir,
                Some(&tasks_path),
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    run_id,
                    "sharding",
                    None,
                    round,
                    model,
                    vendor,
                    route_provider,
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
            Err(err) => {
                self.record_agent_error(format!("failed to launch recovery sharding: {err}"));
                false
            }
        }
    }
}
