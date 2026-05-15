use crate::app::models::subscription_tag;
use crate::app::prompts::final_validation_prompt;
use crate::app::{App, guard};
use crate::data::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::data::validation as final_validation;
use crate::data::validation::{DreamRecommendation, ValidationStatus};
use crate::selection::CachedModel;
use crate::selection::config::SelectionStage;
use crate::state::{self as session_state, DreamingDecision, DreamingDecisionKind, Stage};
use anyhow::{Context, Result};
impl App {
    pub(crate) fn launch_final_validation_with_model(
        &mut self,
        override_model: Option<CachedModel>,
    ) -> bool {
        self.clear_agent_error();
        if !self.guard_models_loaded() {
            return false;
        }
        let Stage::FinalValidation(round) = self.state.current_stage else {
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
        // Validator effort dial reuses the existing review-stage setting; no
        // new knob (per spec §5.3).
        let effort = modes.effort_for(EffortLevel::Normal, SelectionStage::Review);
        // Spec §5.3: model = session.selected_model, vendor inherited from
        // that model. Fall back to the standard primary picker if the
        // selected model is unknown to the current model list (e.g. the
        // session was started before the model was retired or in tests
        // that haven't recorded a brainstorm launch).
        let chosen = override_model
            .as_ref()
            .map(|model| {
                let (cli, launch_name, effort_mapping, effort_eligible) =
                    super::pick_cli_and_launch_name(model);
                (
                    model.name.clone(),
                    subscription_tag(model.subscription).to_string(),
                    cli,
                    launch_name,
                    effort_mapping,
                    effort_eligible,
                )
            })
            .or_else(|| self.session_selected_model_for_validator())
            .or_else(|| {
                self.choose_primary_model(None, SelectionStage::Review, effort, modes.cheap)
            });
        let Some((model, subscription_tag, cli, launch_name, effort_mapping, effort_eligible)) =
            chosen
        else {
            self.record_agent_error("no model available for final validation".to_string());
            self.save_state();
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
            self.prompt_meta(),
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
            cli,
            launch_name,
            prompt_path: prompt_path.clone(),
            effort,
            effort_mapping: effort_mapping.clone(),
            effort_eligible,
            modes,
        };
        let dirty = self.capture_run_guard(
            "final-validation",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let window_name = run_label_with_model(
            "[FinalValidation]",
            &model,
            effort,
            effort_eligible,
            &effort_mapping,
        );
        let run_id = self.state.next_agent_run_id();
        let run_key = Self::run_key_for("final-validation", None, round, attempt);
        let artifacts_dir = artifacts;
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&verdict_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            self.runner_supervisor.launch_noninteractive_with_policy(
                run_id,
                &window_name,
                &run,
                &run_key,
                &artifacts_dir,
                Some(&verdict_path),
                crate::data::acp::AcpLaunchPolicy::final_validation(
                    &verdict_path,
                    &live_summary_path,
                ),
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
                self.record_agent_error(format!("failed to launch final validation: {err}"));
                false
            }
        }
    }
    /// Co-located success-finalization for `Stage::FinalValidation(round)`.
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
            ValidationStatus::GoalMet => match verdict.dream_recommendation {
                Some(DreamRecommendation::Skip) => {
                    self.state.dreaming_decision = Some(DreamingDecision {
                        kind: DreamingDecisionKind::ValidatorSkipped,
                        round,
                        reason: None,
                    });
                    self.state.save()?;
                    self.transition_to_stage(Stage::Done)?;
                }
                Some(DreamRecommendation::Suggest) => {
                    self.state.dreaming_decision = Some(DreamingDecision {
                        kind: DreamingDecisionKind::Pending,
                        round,
                        reason: verdict.dream_reason,
                    });
                    self.state.save()?;
                    self.transition_to_stage(Stage::DreamingPending)?;
                }
                None => {
                    anyhow::bail!("goal_met verdict missing dream_recommendation");
                }
            },
            ValidationStatus::GoalGap => {
                let verdict_artifact = format!("artifacts/final_validation_{round}.toml");
                let new_tasks = final_validation::normalize_gap_tasks(
                    verdict.new_tasks,
                    self.state.builder.max_task_id(),
                    &verdict_artifact,
                );
                self.append_goal_gap_tasks(&session_dir, &new_tasks)?;
                self.transition_to_stage(Stage::ImplementationRound(round + 1))?;
            }
            ValidationStatus::NeedsHuman => {
                self.transition_to_blocked(crate::state::BlockOrigin::FinalValidation)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::{key, mk_app, with_temp_root};
    use crate::data::adapters::EffortLevel;
    use crate::state::{
        DreamingDecision, DreamingDecisionKind, LaunchModes, RunRecord, RunStatus, SessionState,
    };
    use crossterm::event::KeyCode;

    fn run_record(id: u64, round: u32) -> RunRecord {
        RunRecord {
            id,
            stage: "final-validation".to_string(),
            task_id: None,
            round,
            attempt: 1,
            model: "test-model".to_string(),
            subscription_label: "test-vendor".to_string(),
            window_name: "[FinalValidation] test-model".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            effort_eligible: false,
            modes: LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        }
    }

    fn write_verdict(session_id: &str, round: u32, body: &str) {
        let dir = session_state::session_dir(session_id).join("artifacts");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("final_validation_{round}.toml")), body).unwrap();
    }

    #[test]
    fn goal_met_skip_finishes_and_persists_validator_skip() {
        with_temp_root(|| {
            let mut state = SessionState::new("fv-skip".to_string());
            state.current_stage = Stage::FinalValidation(1);
            let run = run_record(10, 1);
            state.agent_runs.push(run.clone());
            write_verdict(
                &state.session_id,
                1,
                r#"status = "goal_met"
summary = "Ready to ship"
findings = []
dream_recommendation = "skip"
"#,
            );
            let mut app = mk_app(state);

            app.finalize_final_validation_success(&run, 1).unwrap();

            assert_eq!(app.state.current_stage, Stage::Done);
            assert_eq!(
                app.state.dreaming_decision,
                Some(DreamingDecision {
                    kind: DreamingDecisionKind::ValidatorSkipped,
                    round: 1,
                    reason: None,
                })
            );
            assert!(app.active_modal().is_none());
        });
    }

    #[test]
    fn goal_met_suggest_enters_persisted_decision_prompt() {
        with_temp_root(|| {
            let mut state = SessionState::new("fv-suggest".to_string());
            state.current_stage = Stage::FinalValidation(2);
            let run = run_record(20, 2);
            state.agent_runs.push(run.clone());
            write_verdict(
                &state.session_id,
                2,
                r#"status = "goal_met"
summary = "Ready after memory updates"
findings = []
dream_recommendation = "suggest"
dream_reason = "Several memory lessons should be consolidated."
"#,
            );
            let mut app = mk_app(state);

            app.finalize_final_validation_success(&run, 2).unwrap();

            assert_eq!(app.state.current_stage, Stage::DreamingPending);
            assert_eq!(
                app.state.dreaming_decision,
                Some(DreamingDecision {
                    kind: DreamingDecisionKind::Pending,
                    round: 2,
                    reason: Some("Several memory lessons should be consolidated.".to_string()),
                })
            );
            assert_eq!(
                app.active_modal(),
                Some(crate::app::ModalKind::DreamingDecision)
            );
        });
    }

    #[test]
    fn skip_decision_persists_and_finishes_without_reoffering() {
        with_temp_root(|| {
            let mut state = SessionState::new("fv-operator-skip".to_string());
            state.current_stage = Stage::DreamingPending;
            state.dreaming_decision = Some(DreamingDecision {
                kind: DreamingDecisionKind::Pending,
                round: 3,
                reason: Some("Memory changed enough to consolidate.".to_string()),
            });
            let mut app = mk_app(state);

            app.handle_modal_key(
                crate::app::ModalKind::DreamingDecision,
                key(KeyCode::Char('s')),
            );

            assert_eq!(app.state.current_stage, Stage::Done);
            assert_eq!(
                app.state.dreaming_decision,
                Some(DreamingDecision {
                    kind: DreamingDecisionKind::OperatorSkipped,
                    round: 3,
                    reason: Some("Memory changed enough to consolidate.".to_string()),
                })
            );
            assert!(app.active_modal().is_none());
        });
    }
}
