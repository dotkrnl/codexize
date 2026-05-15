use crate::app::prompts::spec_review_prompt;
use crate::app::{App, guard};
use crate::app_runtime::{UiKey, UiKeyCode};
use crate::data::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::selection::CachedModel;
use crate::state::{self as session_state, MessageKind, RunStatus, Stage};
use anyhow::Result;
use std::path::Path;
impl App {
    /// Extend the operator's default ACP policy so the interactive spec
    /// reviewer can direct-apply approved edits to `spec.md` during the
    /// session. Idempotent — silently skips appending if `spec.md` is
    /// already in the operator's configured allowlist.
    pub(crate) fn spec_review_acp_policy(
        mut policy: crate::data::acp::AcpLaunchPolicy,
        spec_path: &Path,
    ) -> crate::data::acp::AcpLaunchPolicy {
        let spec_path = spec_path.to_path_buf();
        if !policy.allowed_write_paths.contains(&spec_path) {
            policy.allowed_write_paths.push(spec_path);
        }
        policy
    }
    pub(crate) fn launch_spec_review(&mut self) {
        let _ = self.launch_spec_review_with_model(None);
    }
    pub(crate) fn launch_spec_review_with_model(
        &mut self,
        override_model: Option<CachedModel>,
    ) -> bool {
        self.clear_agent_error();
        if !self.guard_models_loaded() {
            return false;
        }
        let round = match self.state.current_stage {
            Stage::SpecReviewPaused => self.completed_rounds("spec-review") + 1,
            _ => self.completed_rounds("spec-review").max(1),
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let review_path = session_dir
            .join("artifacts")
            .join(format!("spec-review-{round}.md"));
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("spec-review-{round}.md"));
        let modes = self.state.launch_modes();
        let stage = Self::selection_stage_for_stage("spec-review");
        let effort = modes.effort_for(EffortLevel::Normal, stage);
        let runs: Vec<_> = self
            .state
            .agent_runs
            .iter()
            .filter(|run| {
                (run.stage == "brainstorm" || (run.stage == "spec-review" && run.round == round))
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
        let attempt = self.attempt_for("spec-review", None, round);
        let live_summary_path = self.live_summary_path_for_run("spec-review", None, round, attempt);
        let earlier_specs = self.earlier_waiting_specs();
        let prompt = spec_review_prompt(
            &spec_path.display().to_string(),
            &review_path.display().to_string(),
            &live_summary_path.display().to_string(),
            &earlier_specs,
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
            &format!("[Spec Review {round}]"),
            &model,
            effort,
            effort_eligible,
            &effort_mapping,
        );
        let run_id = self.state.next_agent_run_id();
        let dirty = self.capture_run_guard(
            "spec-review",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let run_key = Self::run_key_for("spec-review", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let policy = Self::spec_review_acp_policy(self.default_acp_policy(), &spec_path);
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&review_path), &run_key, &artifacts_dir)
        {
            result
        } else if modes.yolo {
            // YOLO spec review runs without a human operator on the loop —
            // the finalize handler auto-approves the pause modal, and the
            // interactive launcher would block waiting on operator input
            // that never arrives. Mirrors the brainstorm/planning split.
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
                    "spec-review",
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
                self.record_agent_error(format!("failed to launch spec review: {err}"));
                false
            }
        }
    }
}
impl App {
    /// Modal handler for the "spec review paused — accept verdict?" prompt.
    /// Co-located with the spec-review launch so the stage's launch and
    /// pause-modal behavior live in one file.
    pub(crate) fn handle_spec_review_paused_modal_key(&mut self, key: impl Into<UiKey>) -> bool {
        let key = key.into();
        match key.code {
            UiKeyCode::Char('q' | 'Q') | UiKeyCode::Esc => true,
            UiKeyCode::Char('y') | UiKeyCode::Enter => {
                self.clear_agent_error();
                self.transition_to_stage_logged(Stage::PlanningRunning);
                false
            }
            UiKeyCode::Char('n') => {
                self.transition_to_stage_logged(Stage::SpecReviewRunning);
                self.launch_spec_review();
                false
            }
            // Consume all other keys so the UI is genuinely modal.
            _ => false,
        }
    }
    /// Co-located success-finalization for `Stage::SpecReviewRunning`.
    pub(crate) fn finalize_spec_review_success(
        &mut self,
        run: &crate::state::RunRecord,
    ) -> Result<()> {
        self.finalize_run_record(run.id, true, None);
        self.clear_agent_error();
        if !matches!(self.state.current_stage, Stage::SpecReviewRunning) {
            self.append_system_message(
                run.id,
                MessageKind::Summary,
                "Spec review complete.".to_string(),
            );
            return Ok(());
        }
        self.transition_to_stage(Stage::SpecReviewPaused)?;
        self.append_system_message(
            run.id,
            MessageKind::Summary,
            "Spec review complete.".to_string(),
        );
        if run.modes.yolo {
            self.auto_approve_spec_review_pause("spec_approval");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::App;
    use crate::data::acp::AcpLaunchPolicy;
    use std::path::PathBuf;

    #[test]
    fn spec_review_policy_appends_spec_md_to_allowed_write_paths() {
        let spec_path = PathBuf::from("/sess/artifacts/spec.md");
        let policy = App::spec_review_acp_policy(AcpLaunchPolicy::default(), &spec_path);
        assert!(
            policy.allowed_write_paths.contains(&spec_path),
            "spec.md must be writable so the interactive reviewer can direct-apply edits"
        );
    }

    #[test]
    fn spec_review_policy_preserves_operator_configured_entries() {
        let spec_path = PathBuf::from("/sess/artifacts/spec.md");
        let existing = PathBuf::from("/sess/artifacts/live_summary.txt");
        let mut base = AcpLaunchPolicy::default();
        base.allowed_write_paths.push(existing.clone());
        let policy = App::spec_review_acp_policy(base, &spec_path);
        assert!(policy.allowed_write_paths.contains(&existing));
        assert!(policy.allowed_write_paths.contains(&spec_path));
    }

    #[test]
    fn spec_review_policy_is_idempotent_when_spec_already_allowed() {
        let spec_path = PathBuf::from("/sess/artifacts/spec.md");
        let mut base = AcpLaunchPolicy::default();
        base.allowed_write_paths.push(spec_path.clone());
        let policy = App::spec_review_acp_policy(base, &spec_path);
        let occurrences = policy
            .allowed_write_paths
            .iter()
            .filter(|p| *p == &spec_path)
            .count();
        assert_eq!(occurrences, 1, "spec.md must not be duplicated");
    }
}
