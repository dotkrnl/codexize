use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::prompts::recovery_plan_review_prompt;
use crate::app::{App, guard};
use crate::selection::CachedModel;
use crate::state::{self as session_state, Phase};
use anyhow::Context;
impl App {
    pub(crate) fn launch_recovery_plan_review(&mut self) {
        let _ = self.launch_recovery_plan_review_with_model(None);
    }
    pub(crate) fn launch_recovery_plan_review_with_model(
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
        let Phase::BuilderRecoveryPlanReview(round) = self.state.current_phase else {
            return false;
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let plan_review_path = artifacts.join("plan_review.toml");
        let _ = std::fs::remove_file(&plan_review_path);
        let recovery_path = session_dir
            .join("rounds")
            .join(format!("{round:03}"))
            .join("recovery.toml");
        let triggering_review_path = session_dir
            .join("rounds")
            .join(format!("{round:03}"))
            .join("review.toml");
        let attempt = self.attempt_for("plan-review", None, round);
        let live_summary_path = self.live_summary_path_for_run("plan-review", None, round, attempt);
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("recovery-plan-review-r{round}.md"));
        let modes = self.state.launch_modes();
        let phase = Self::phase_for_stage("plan-review");
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
        let prompt = recovery_plan_review_prompt(
            &spec_path,
            &plan_path,
            &triggering_review_path,
            &recovery_path,
            &live_summary_path,
            &plan_review_path,
            self.memory_view.max_topics_per_read,
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
        session_state::transitions::mark_latest_pipeline_stage_running(
            &mut self.state,
            "plan-review",
        );
        let run = AgentRun {
            model: model.clone(),
            route_provider: route_provider.clone(),
            prompt_path: prompt_path.clone(),
            effort,
            modes,
        };
        let dirty = self.capture_run_guard(
            "plan-review",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let window_name =
            run_label_with_model("[Recovery Plan Review]", &model, vendor_kind, effort);
        let run_id = self.state.next_agent_run_id();
        let run_key = Self::run_key_for("plan-review", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&plan_review_path), &run_key, &artifacts_dir)
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
                Some(&plan_review_path),
                self.default_acp_policy(),
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    run_id,
                    "plan-review",
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
                self.record_agent_error(format!("failed to launch recovery plan review: {err}"));
                false
            }
        }
    }
}
