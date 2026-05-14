use crate::app::prompts::planning_prompt;
use crate::app::{App, guard};
use crate::data::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::selection::CachedModel;
use crate::state::{self as session_state, Stage};
use anyhow::Result;
impl App {
    pub(crate) fn launch_planning_with_model(
        &mut self,
        override_model: Option<CachedModel>,
        interactive: bool,
    ) -> bool {
        self.clear_agent_error();
        if !self.guard_models_loaded() {
            return false;
        }
        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");
        let modes = self.state.launch_modes();
        let stage = Self::selection_stage_for_stage("planning");
        let effort = modes.effort_for(EffortLevel::Normal, stage);
        let Some(chosen) =
            self.choose_primary_model(override_model.as_ref(), stage, effort, modes.cheap)
        else {
            self.record_agent_error("no model available with quota".to_string());
            self.save_state();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, subscription_tag, cli, launch_name, effort_mapping, effort_eligible) = chosen;
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
        let earlier_specs = self.earlier_waiting_specs();
        let prompt = planning_prompt(
            &spec_path,
            &plan_path,
            &live_summary_path,
            modes.yolo,
            prior_attempts_path.as_deref(),
            &earlier_specs,
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
        // YOLO planning must take the existing non-interactive runner path.
        let interactive = interactive && !modes.yolo;
        let guard_mode = if interactive {
            guard::GuardMode::AskOperator
        } else {
            guard::GuardMode::AutoReset
        };
        let dirty = self.capture_run_guard("planning", None, 1, attempt, guard_mode);
        let window_name = run_label_with_model(
            "[Planning]",
            &model,
            effort,
            effort_eligible,
            &effort_mapping,
        );
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
                self.surface_boundary_error(format!("failed to launch planning: {e}"), true);
                false
            }
        }
    }
    /// Co-located success-finalization for `Stage::PlanningRunning`.
    ///
    /// Spec line 46 conjoins yolo plan-review skip with `artifacts/plan.md`
    /// existing. The successful-finalization context already implies the
    /// artifact, but the explicit guard protects against a planning agent
    /// that reports success without writing the file. The yolo skip lands
    /// on `WaitingToImplement` — spec §Data model line 96 puts the queue
    /// pause before sharding, so the scheduler/repo-state-update dispatch
    /// (the only normal route into `ShardingRunning`) remains the sole gate.
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
            self.transition_to_stage(Stage::WaitingToImplement)?;
        } else {
            self.transition_to_stage(Stage::PlanReviewRunning)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::test_support::{mk_app, with_temp_root};
    use crate::state::{LaunchModes, RunRecord, RunStatus, SessionState, Stage, session_dir};

    fn planning_run(yolo: bool) -> RunRecord {
        RunRecord {
            id: 1,
            stage: "planning".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "test-model".to_string(),
            subscription_label: "test-vendor".to_string(),
            window_name: "[Planning] test-model".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: crate::data::adapters::EffortLevel::Normal,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            effort_eligible: false,
            modes: LaunchModes {
                yolo,
                ..LaunchModes::default()
            },
            hostname: None,
            mount_device_id: None,
            section_path: None,
        }
    }

    #[test]
    fn yolo_planning_success_pauses_in_waiting_to_implement() {
        // Spec §Data model line 96: approved plans pause in
        // WaitingToImplement before any sharding launch.
        with_temp_root(|| {
            let session_id = "20260511-091000-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::PlanningRunning;
            let run = planning_run(true);
            state.agent_runs.push(run.clone());
            state.save().unwrap();
            let artifacts = session_dir(session_id).join("artifacts");
            std::fs::create_dir_all(&artifacts).unwrap();
            std::fs::write(artifacts.join("plan.md"), "# plan\n").unwrap();
            let mut app = mk_app(state);
            app.finalize_planning_success(&run).unwrap();
            assert_eq!(app.state.current_stage, Stage::WaitingToImplement);
        });
    }

    #[test]
    fn non_yolo_planning_success_routes_to_plan_review() {
        // Sanity guard: the WaitingToImplement pause must not swallow the
        // human-review path. Non-yolo runs still flow into PlanReviewRunning.
        with_temp_root(|| {
            let session_id = "20260511-091000-000000002";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::PlanningRunning;
            let run = planning_run(false);
            state.agent_runs.push(run.clone());
            state.save().unwrap();
            let artifacts = session_dir(session_id).join("artifacts");
            std::fs::create_dir_all(&artifacts).unwrap();
            std::fs::write(artifacts.join("plan.md"), "# plan\n").unwrap();
            let mut app = mk_app(state);
            app.finalize_planning_success(&run).unwrap();
            assert_eq!(app.state.current_stage, Stage::PlanReviewRunning);
        });
    }
}
