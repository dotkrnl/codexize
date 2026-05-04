use anyhow::Context;

use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::{App, guard};
use crate::app::prompts::recovery_prompt;
use crate::runner::{launch_interactive, launch_noninteractive};
use crate::selection::CachedModel;
use crate::state::{self as session_state, Phase, PipelineItemStatus};

impl App {
    pub(in crate::app) fn launch_recovery(&mut self) {
        let _ = self.launch_recovery_with_model(None);
    }

    pub(in crate::app) fn launch_recovery_with_model(
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
        let (model, vendor_kind, vendor) = chosen;

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
            prompt_path: prompt_path.clone(),
            effort,
            modes,
        };
        let recovery_guard_mode = if is_human_blocked {
            guard::GuardMode::AskOperator
        } else {
            guard::GuardMode::AutoReset
        };
        let dirty = self.capture_run_guard("recovery", None, round, attempt, recovery_guard_mode);
        let window_name = run_label_with_model("[Recovery]", &model, vendor_kind, effort);
        let run_key = Self::run_key_for("recovery", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&recovery_path), &run_key, &artifacts_dir)
        {
            result
        } else if is_human_blocked {
            launch_interactive(
                &window_name,
                &run,
                vendor_kind,
                &run_key,
                &artifacts_dir,
                Some(&recovery_path),
            )
        } else {
            launch_noninteractive(
                &window_name,
                &run,
                vendor_kind,
                &run_key,
                &artifacts_dir,
                Some(&recovery_path),
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    "recovery",
                    None,
                    round,
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
            Err(err) => {
                self.record_agent_error(format!("failed to launch recovery: {err}"));
                false
            }
        }
    }
}
