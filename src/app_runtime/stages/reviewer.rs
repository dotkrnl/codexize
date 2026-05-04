use crate::adapters::{AgentRun, run_label_with_model};
use crate::app::{App, guard};
use crate::app::prompts::{ReviewerPromptInputs, read_review_scope, reviewer_prompt, task_effort_for};
use crate::runner::launch_noninteractive;
use crate::selection::CachedModel;
use crate::state::{self as session_state, Phase};

impl App {
    pub(crate) fn launch_reviewer(&mut self) {
        let _ = self.launch_reviewer_with_model(None);
    }

    pub(crate) fn launch_reviewer_with_model(
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
        let Phase::ReviewRound(r) = self.state.current_phase else {
            return false;
        };
        let Some(task_id) = self.state.builder.current_task_id() else {
            self.record_agent_error("no current task".to_string());
            let _ = self.state.save();
            return false;
        };

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let round_dir = session_dir.join("rounds").join(format!("{r:03}"));
        let review_path = round_dir.join("review.toml");
        let review_scope_file = round_dir.join("review_scope.toml");
        let task_file = round_dir.join("task.toml");

        let _ = std::fs::remove_file(&review_path);

        let excluded = self
            .state
            .agent_runs
            .iter()
            .filter(|run| {
                (run.stage == "reviewer" || run.stage == "coder")
                    && run.task_id == Some(task_id)
                    && run.round == r
            })
            .cloned()
            .collect::<Vec<_>>();
        let modes = self.state.launch_modes();
        let requested_effort = task_effort_for(&session_dir, task_id);
        let effort = modes.effort_for(requested_effort, Self::phase_for_stage("reviewer"));
        // Override-model bypass: explicit operator pick beats the effort filter.
        let (used_vendors, used_models) = Self::used_review_pairs(&excluded);
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

        let attempt = self.attempt_for("reviewer", Some(task_id), r);
        let live_summary_path =
            self.live_summary_path_for_run("reviewer", Some(task_id), r, attempt);
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("reviewer-r{r}.md"));
        if let Err(err) = read_review_scope(&review_scope_file) {
            self.record_agent_error(format!("invalid review scope: {err:#}"));
            let _ = self.state.save();
            return false;
        }
        let coder_summary_file = round_dir.join("coder_summary.toml");
        let coder_summary_path = coder_summary_file
            .exists()
            .then_some(coder_summary_file.as_path());
        let prompt = reviewer_prompt(ReviewerPromptInputs {
            session_dir: &session_dir,
            task_id,
            round: r,
            task_file: &task_file,
            review_scope_file: &review_scope_file,
            coder_summary_file: coder_summary_path,
            review_file: &review_path,
            live_summary_path: &live_summary_path,
        });
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.surface_boundary_error(format!("error writing prompt: {e}"), true);
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
            effort,
            modes,
        };

        let window_name = run_label_with_model(
            &format!("[Round {r} Reviewer]"),
            &model,
            vendor_kind,
            effort,
        );
        let dirty = self.capture_run_guard(
            "reviewer",
            Some(task_id),
            r,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let run_key = Self::run_key_for("reviewer", Some(task_id), r, attempt);
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
                    "reviewer",
                    Some(task_id),
                    r,
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
                self.surface_boundary_error(format!("failed to launch reviewer: {e}"), true);
                false
            }
        }
    }
}
