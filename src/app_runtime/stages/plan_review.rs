use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::{App, guard};
use crate::app::prompts::plan_review_prompt;
use crate::runner::launch_noninteractive;
use crate::selection::CachedModel;
use crate::state::{self as session_state, Phase, RunStatus};

impl App {
    pub(crate) fn launch_plan_review(&mut self) {
        let _ = self.launch_plan_review_with_model(None);
    }

    pub(crate) fn launch_plan_review_with_model(
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

        let round = match self.state.current_phase {
            Phase::PlanReviewPaused => self.completed_rounds("plan-review") + 1,
            _ => self.completed_rounds("plan-review").max(1),
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");
        let review_path = session_dir
            .join("artifacts")
            .join(format!("plan-review-{round}.md"));
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("plan-review-{round}.md"));

        let modes = self.state.launch_modes();
        let phase = Self::phase_for_stage("plan-review");
        let effort = modes.effort_for(EffortLevel::Normal, phase);
        let runs: Vec<_> = self
            .state
            .agent_runs
            .iter()
            .filter(|run| {
                (run.stage == "planning" || (run.stage == "plan-review" && run.round == round))
                    && run.status == RunStatus::Done
            })
            .cloned()
            .collect();
        let (used_vendors, used_models) = Self::used_review_pairs(&runs);
        let Some(chosen) = self.choose_review_model(
            override_model.as_ref(),
            &used_vendors,
            &used_models,
            effort,
            modes.cheap,
        ) else {
            self.record_agent_error("no model available for review".to_string());
            let _ = self.state.save();
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let attempt = self.attempt_for("plan-review", None, round);
        let live_summary_path = self.live_summary_path_for_run("plan-review", None, round, attempt);
        let prompt = plan_review_prompt(
            &spec_path.display().to_string(),
            &plan_path.display().to_string(),
            &review_path.display().to_string(),
            round,
            &live_summary_path.display().to_string(),
        );
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
        let window_name = run_label_with_model(
            &format!("[Plan Review {round}]"),
            &model,
            vendor_kind,
            effort,
        );
        let dirty = self.capture_run_guard(
            "plan-review",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let run_key = Self::run_key_for("plan-review", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&review_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            launch_noninteractive(
                &window_name,
                &run,
                vendor_kind,
                &run_key,
                &artifacts_dir,
                Some(&review_path),
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    "plan-review",
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
                self.record_agent_error(format!("failed to launch plan review: {err}"));
                false
            }
        }
    }
}
