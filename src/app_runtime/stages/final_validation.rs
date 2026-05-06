use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::models::vendor_tag;
use crate::app::prompts::final_validation_prompt;
use crate::app::{App, guard};
use crate::final_validation::{self, ValidationStatus};
use crate::selection::CachedModel;
use crate::selection::config::SelectionPhase;
use crate::state::{self as session_state, Phase};
use anyhow::{Context, Result};
impl App {
    pub(crate) fn launch_final_validation(&mut self) {
        let _ = self.launch_final_validation_with_model(None);
    }
    pub(crate) fn launch_final_validation_with_model(
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
        let Phase::FinalValidation(round) = self.state.current_phase else {
            return false;
        };
        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let verdict_path = artifacts.join(format!("final_validation_{round}.toml"));
        // Force a fresh verdict each entry so a stale TOML can't be mistaken
        // for this run's output during finalization.
        let _ = std::fs::remove_file(&verdict_path);
        let idea_text = self.state.idea_text.clone().unwrap_or_default();
        let spec_text = std::fs::read_to_string(&spec_path).unwrap_or_default();
        let modes = self.state.launch_modes();
        // Validator effort dial reuses the existing review-phase setting; no
        // new knob (per spec §5.3).
        let effort = modes.effort_for(EffortLevel::Normal, SelectionPhase::Review);
        // Spec §5.3: model = session.selected_model, vendor inherited from
        // that model. Fall back to the standard primary picker if the
        // selected model is unknown to the current model list (e.g. the
        // session was started before the model was retired or in tests
        // that haven't recorded a brainstorm launch).
        let chosen = override_model
            .as_ref()
            .map(|model| {
                (
                    model.name.clone(),
                    model.vendor,
                    vendor_tag(model.vendor).to_string(),
                )
            })
            .or_else(|| self.session_selected_model_for_validator())
            .or_else(|| {
                self.choose_primary_model(None, SelectionPhase::Review, effort, modes.cheap)
            });
        let Some((model, vendor_kind, vendor)) = chosen else {
            self.record_agent_error("no model available for final validation".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let attempt = self.attempt_for("final-validation", None, round);
        let live_summary_path =
            self.live_summary_path_for_run("final-validation", None, round, attempt);
        let simplification_path = session_dir
            .join("rounds")
            .join(format!("{round:03}"))
            .join("simplification.toml");
        let prompt = final_validation_prompt(
            &idea_text,
            &spec_text,
            &verdict_path,
            &live_summary_path,
            Some(&simplification_path),
        );
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("final-validation-r{round}.md"));
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt) {
            self.record_agent_error(format!("error writing prompt: {err}"));
            return false;
        }
        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
            effort,
            modes,
        };
        let dirty = self.capture_run_guard(
            "final-validation",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let window_name = run_label_with_model("[FinalValidation]", &model, vendor_kind, effort);
        let run_id = self.state.next_agent_run_id();
        let run_key = Self::run_key_for("final-validation", None, round, attempt);
        let artifacts_dir = artifacts.clone();
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&verdict_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            self.runner_supervisor.launch_noninteractive_with_policy(
                run_id,
                &window_name,
                &run,
                vendor_kind,
                &run_key,
                &artifacts_dir,
                Some(&verdict_path),
                crate::acp::AcpLaunchPolicy::final_validation(&verdict_path, &live_summary_path),
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    run_id,
                    "final-validation",
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
                self.record_agent_error(format!("failed to launch final validation: {err}"));
                false
            }
        }
    }
    /// Co-located success-finalization for `Phase::FinalValidation(round)`.
    pub(crate) fn finalize_final_validation_success(
        &mut self,
        run: &crate::state::RunRecord,
        round: u32,
    ) -> Result<()> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let verdict_path = session_dir
            .join("artifacts")
            .join(format!("final_validation_{round}.toml"));
        let verdict = final_validation::validate(&verdict_path)
            .with_context(|| format!("invalid {}", verdict_path.display()))?;
        self.finalize_run_record(run.id, true, None);
        self.clear_agent_error();
        match verdict.status {
            ValidationStatus::GoalMet => {
                self.transition_to_phase(Phase::Done)?;
            }
            ValidationStatus::GoalGap => {
                let verdict_artifact = format!("artifacts/final_validation_{round}.toml");
                let new_tasks = final_validation::normalize_gap_tasks(
                    verdict.new_tasks,
                    self.state.builder.max_task_id(),
                    &verdict_artifact,
                );
                self.append_goal_gap_tasks(&session_dir, &new_tasks)?;
                self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
            }
            ValidationStatus::NeedsHuman => {
                self.transition_to_blocked(crate::state::BlockOrigin::FinalValidation)?;
            }
        }
        Ok(())
    }
}
