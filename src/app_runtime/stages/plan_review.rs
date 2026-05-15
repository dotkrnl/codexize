use crate::app::prompts::plan_review_prompt;
use crate::app::{App, guard};
use crate::data::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::selection::CachedModel;
use crate::state::{self as session_state, MessageKind, RunStatus, Stage};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
use std::path::Path;
impl App {
    /// Extend the operator's default ACP policy so the interactive plan
    /// reviewer can direct-apply approved edits to both `plan.md` and
    /// `spec.md` during the session. Idempotent — silently skips
    /// appending an entry already in the operator's configured allowlist.
    pub(crate) fn plan_review_acp_policy(
        mut policy: crate::data::acp::AcpLaunchPolicy,
        plan_path: &Path,
        spec_path: &Path,
    ) -> crate::data::acp::AcpLaunchPolicy {
        for path in [plan_path.to_path_buf(), spec_path.to_path_buf()] {
            if !policy.allowed_write_paths.contains(&path) {
                policy.allowed_write_paths.push(path);
            }
        }
        policy
    }
    pub(crate) fn launch_plan_review(&mut self) {
        let _ = self.launch_plan_review_with_model(None);
    }
    pub(crate) fn launch_plan_review_with_model(
        &mut self,
        override_model: Option<CachedModel>,
    ) -> bool {
        self.clear_agent_error();
        if !self.guard_models_loaded() {
            return false;
        }
        let round = match self.state.current_stage {
            Stage::PlanReviewPaused => self.completed_rounds("plan-review") + 1,
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
        let stage = Self::selection_stage_for_stage("plan-review");
        let effort = modes.effort_for(EffortLevel::Normal, stage);
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
            self.save_state();
            return false;
        };
        let (model, subscription_tag, cli, launch_name, effort_mapping, effort_eligible) = chosen;
        let attempt = self.attempt_for("plan-review", None, round);
        let live_summary_path = self.live_summary_path_for_run("plan-review", None, round, attempt);
        let prompt = plan_review_prompt(
            &spec_path.display().to_string(),
            &plan_path.display().to_string(),
            &review_path.display().to_string(),
            round,
            &live_summary_path.display().to_string(),
            self.prompt_meta(),
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
            cli,
            launch_name,
            prompt_path: prompt_path.clone(),
            effort,
            effort_mapping: effort_mapping.clone(),
            effort_eligible,
            modes,
        };
        let window_name = run_label_with_model(
            &format!("[Plan Review {round}]"),
            &model,
            effort,
            effort_eligible,
            &effort_mapping,
        );
        let run_id = self.state.next_agent_run_id();
        let dirty = self.capture_run_guard(
            "plan-review",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let run_key = Self::run_key_for("plan-review", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let policy =
            Self::plan_review_acp_policy(self.default_acp_policy(), &plan_path, &spec_path);
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&review_path), &run_key, &artifacts_dir)
        {
            result
        } else if modes.yolo {
            // YOLO plan review runs without a human operator on the loop —
            // the finalize handler auto-approves the pause modal, and the
            // interactive launcher would block on operator input that never
            // arrives. Mirrors the brainstorm/planning split.
            self.runner_supervisor.launch_noninteractive_with_policy(
                run_id,
                &window_name,
                &run,
                &run_key,
                &artifacts_dir,
                Some(&review_path),
                policy,
            )
        } else {
            self.runner_supervisor.launch_interactive_with_policy(
                run_id,
                &window_name,
                &run,
                &run_key,
                &artifacts_dir,
                Some(&review_path),
                policy,
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
            Err(err) => {
                self.record_agent_error(format!("failed to launch plan review: {err}"));
                false
            }
        }
    }
}
impl App {
    /// Modal handler for the "plan review paused — accept verdict?" prompt.
    /// Co-located with the plan-review launch so the stage's launch and
    /// pause-modal behavior live in one file.
    pub(crate) fn handle_plan_review_paused_modal_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q' | 'Q') | KeyCode::Esc => true,
            KeyCode::Char('y') | KeyCode::Enter => {
                self.clear_agent_error();
                self.queue_view_of_current_artifact("plan.md");
                // Spec §Data model line 96: approved plans pause in
                // `WaitingToImplement` before sharding. The scheduler /
                // repo-state-update dispatch out of `WaitingToImplement`
                // owns the actual launch into `ShardingRunning`.
                self.transition_to_stage_logged(Stage::WaitingToImplement);
                false
            }
            KeyCode::Char('n') => {
                self.transition_to_stage_logged(Stage::PlanReviewRunning);
                self.launch_plan_review();
                false
            }
            // Consume all other keys so the UI is genuinely modal.
            _ => false,
        }
    }
    /// Co-located success-finalization for `Stage::PlanReviewRunning`.
    pub(crate) fn finalize_plan_review_success(
        &mut self,
        run: &crate::state::RunRecord,
    ) -> Result<()> {
        self.finalize_run_record(run.id, true, None);
        self.clear_agent_error();
        if !matches!(self.state.current_stage, Stage::PlanReviewRunning) {
            self.append_system_message(
                run.id,
                MessageKind::Summary,
                "Plan review complete.".to_string(),
            );
            return Ok(());
        }
        self.transition_to_stage(Stage::PlanReviewPaused)?;
        self.append_system_message(
            run.id,
            MessageKind::Summary,
            "Plan review complete.".to_string(),
        );
        if run.modes.yolo {
            self.auto_approve_plan_review_pause("plan_approval");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::App;
    use crate::app::test_support::{mk_app, with_temp_root};
    use crate::data::acp::AcpLaunchPolicy;
    use crate::state::{LaunchModes, RunRecord, RunStatus, SessionState, Stage};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;

    fn plan_review_run() -> RunRecord {
        RunRecord {
            id: 5,
            stage: "plan-review".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "test-model".to_string(),
            subscription_label: "test-vendor".to_string(),
            window_name: "[Plan Review 1] test-model".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: crate::data::adapters::EffortLevel::Normal,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            effort_eligible: false,
            modes: LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        }
    }

    #[test]
    fn plan_review_paused_enter_pauses_in_waiting_to_implement() {
        // Spec §Data model line 96: confirmed plan review approval pauses
        // in WaitingToImplement; the dispatch out of WaitingToImplement is
        // the sole production route into ShardingRunning.
        with_temp_root(|| {
            let session_id = "20260511-091500-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::PlanReviewPaused;
            state.agent_runs.push(plan_review_run());
            state.save().unwrap();
            let mut app = mk_app(state);
            let consumed = app.handle_plan_review_paused_modal_key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ));
            assert!(!consumed, "Enter must dismiss the modal (returns false)");
            assert_eq!(app.state.current_stage, Stage::WaitingToImplement);
        });
    }

    #[test]
    fn plan_review_paused_y_pauses_in_waiting_to_implement() {
        with_temp_root(|| {
            let session_id = "20260511-091500-000000002";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::PlanReviewPaused;
            state.agent_runs.push(plan_review_run());
            state.save().unwrap();
            let mut app = mk_app(state);
            app.handle_plan_review_paused_modal_key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
            ));
            assert_eq!(app.state.current_stage, Stage::WaitingToImplement);
        });
    }

    #[test]
    fn yolo_auto_approve_plan_review_pause_lands_in_waiting_to_implement() {
        // The yolo auto-approve helper (called from `finalize_plan_review_success`
        // and `maybe_yolo_auto_resolve`) must use the same pause pathway as
        // the interactive modal — otherwise a yolo run would bypass the
        // implementation-lane queue.
        with_temp_root(|| {
            let session_id = "20260511-091500-000000003";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::PlanReviewPaused;
            state.modes.yolo = true;
            state.agent_runs.push(plan_review_run());
            state.save().unwrap();
            let mut app = mk_app(state);
            app.auto_approve_plan_review_pause("plan_approval");
            assert_eq!(app.state.current_stage, Stage::WaitingToImplement);
        });
    }

    #[test]
    fn plan_review_policy_appends_plan_and_spec_md_to_allowed_write_paths() {
        let plan_path = PathBuf::from("/sess/artifacts/plan.md");
        let spec_path = PathBuf::from("/sess/artifacts/spec.md");
        let policy =
            App::plan_review_acp_policy(AcpLaunchPolicy::default(), &plan_path, &spec_path);
        assert!(
            policy.allowed_write_paths.contains(&plan_path),
            "plan.md must be writable so the interactive reviewer can direct-apply plan edits"
        );
        assert!(
            policy.allowed_write_paths.contains(&spec_path),
            "spec.md must also be writable — the operator may approve spec edits surfaced during plan review"
        );
    }

    #[test]
    fn plan_review_policy_preserves_operator_configured_entries() {
        let plan_path = PathBuf::from("/sess/artifacts/plan.md");
        let spec_path = PathBuf::from("/sess/artifacts/spec.md");
        let existing = PathBuf::from("/sess/artifacts/live_summary.txt");
        let mut base = AcpLaunchPolicy::default();
        base.allowed_write_paths.push(existing.clone());
        let policy = App::plan_review_acp_policy(base, &plan_path, &spec_path);
        assert!(policy.allowed_write_paths.contains(&existing));
        assert!(policy.allowed_write_paths.contains(&plan_path));
        assert!(policy.allowed_write_paths.contains(&spec_path));
    }

    #[test]
    fn plan_review_policy_is_idempotent_when_paths_already_allowed() {
        let plan_path = PathBuf::from("/sess/artifacts/plan.md");
        let spec_path = PathBuf::from("/sess/artifacts/spec.md");
        let mut base = AcpLaunchPolicy::default();
        base.allowed_write_paths.push(plan_path.clone());
        base.allowed_write_paths.push(spec_path.clone());
        let policy = App::plan_review_acp_policy(base, &plan_path, &spec_path);
        for target in [&plan_path, &spec_path] {
            let occurrences = policy
                .allowed_write_paths
                .iter()
                .filter(|p| *p == target)
                .count();
            assert_eq!(occurrences, 1, "{target:?} must not be duplicated");
        }
    }
}
