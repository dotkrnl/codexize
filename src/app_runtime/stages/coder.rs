use crate::adapters::{AgentRun, run_label_with_model};
use crate::app::prompts::{
    CoderPromptInputs, coder_prompt, read_review_scope, task_toml_for, write_review_scope_artifact,
};
use crate::app::{App, guard};
use crate::selection::CachedModel;
use crate::state::{self as session_state, Stage};
use anyhow::Result;
impl App {
    pub(crate) fn launch_coder_with_model(&mut self, override_model: Option<CachedModel>) -> bool {
        self.clear_agent_error();
        if !self.guard_models_loaded() {
            return false;
        }
        let Stage::ImplementationRound(r) = self.state.current_stage else {
            return false;
        };
        let Some(task_id) = self.ensure_builder_task_for_round(r) else {
            self.record_agent_error("no pending tasks".to_string());
            self.save_state();
            return false;
        };
        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let round_dir = session_dir.join("rounds").join(format!("{r:03}"));
        let task_file = round_dir.join("task.toml");
        let dirty_before_coder = guard::git_status_dirty();
        if !task_file.exists() {
            let body = task_toml_for(&session_dir, task_id).unwrap_or_else(|e| {
                format!("# task body could not be loaded: {e}\nid = {task_id}\n")
            });
            let _ = std::fs::write(&task_file, body);
        }
        // Pin the base HEAD before the coder runs; preserves original base on resume.
        self.capture_round_base(&round_dir);
        let modes = self.state.launch_modes();
        self.record_dirty_worktree_yolo_gate(dirty_before_coder, modes);
        let requested_effort = self.task_effort_for_round(&session_dir, task_id, r);
        let stage = Self::selection_stage_for_stage("coder");
        let effort = modes.effort_for(requested_effort, stage);
        // Override-model bypass: an explicit operator pick wins over the
        // tough-eligibility filter (spec §3.7). The adapter still emits the
        // launch-snapshot effort flag derived from `task.tough`.
        let Some(chosen) =
            self.choose_primary_model(override_model.as_ref(), stage, effort, modes.cheap)
        else {
            self.record_agent_error("no model available with quota".to_string());
            self.save_state();
            return false;
        };
        let (model, subscription_tag, cli, launch_name, effort_mapping, effort_eligible) = chosen;
        let prompt_path = session_dir.join("prompts").join(format!("coder-r{r}.md"));
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let attempt = self.attempt_for("coder", Some(task_id), r);
        let live_summary_path = self.live_summary_path_for_run("coder", Some(task_id), r, attempt);
        let resume = self
            .state
            .agent_runs
            .iter()
            .any(|run| run.stage == "coder" && run.task_id == Some(task_id) && run.round == r);
        // Drain refine carryover only when starting a fresh coder run; on
        // resume we'd have already included it in the original prompt.
        let refine_carryover: Vec<String> = if resume {
            Vec::new()
        } else {
            session_state::take_pending_refine_feedback(&mut self.state)
        };
        let prompt = coder_prompt(CoderPromptInputs {
            session_dir: &session_dir,
            task_id,
            round: r,
            task_file: &task_file,
            live_summary_path: &live_summary_path,
            resume,
            refine_carryover: &refine_carryover,
            meta: self.prompt_meta(),
        });
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
        let window_name = run_label_with_model(
            &format!("[Round {r} Coder]"),
            &model,
            effort,
            effort_eligible,
            &effort_mapping,
        );
        let run_id = self.state.next_agent_run_id();
        self.capture_run_guard(
            "coder",
            Some(task_id),
            r,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let run_key = Self::run_key_for("coder", Some(task_id), r, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result =
            if let Some(result) = self.try_test_launch(None, &run_key, &artifacts_dir) {
                result
            } else {
                self.runner_supervisor.launch_noninteractive_with_policy(
                    run_id,
                    &window_name,
                    &run,
                    &run_key,
                    &artifacts_dir,
                    None,
                    self.default_acp_policy(),
                )
            };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    run_id,
                    "coder",
                    Some(task_id),
                    r,
                    model,
                    subscription_tag,
                    window_name,
                    effort,
                    effort_mapping,
                    effort_eligible,
                    modes,
                    prompt_path,
                );
                true
            }
            Err(e) => {
                self.surface_boundary_error(format!("failed to launch coder: {e}"), true);
                false
            }
        }
    }
    /// Co-located success-finalization for `Stage::ImplementationRound(round)`.
    pub(crate) fn finalize_coder_success(
        &mut self,
        run: &crate::state::RunRecord,
        round: u32,
    ) -> Result<()> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let round_dir = session_dir.join("rounds").join(format!("{round:03}"));
        let scope = read_review_scope(&round_dir.join("review_scope.toml"))?;
        write_review_scope_artifact(&round_dir, &scope.base_sha)?;
        self.finalize_run_record(run.id, true, None);
        self.clear_agent_error();
        if round == 1 && self.state.skip_to_impl_rationale.is_some() {
            self.enter_simplification_or_done(1, run.modes.yolo)?;
        } else {
            self.transition_to_stage(Stage::ReviewRound(round))?;
        }
        Ok(())
    }
}
