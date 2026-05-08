use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::prompts::plan_review_prompt;
use crate::app::{App, guard};
use crate::selection::CachedModel;
use crate::state::{self as session_state, MessageKind, Phase, RunStatus};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
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
        let (model, vendor_kind, vendor, route_provider) = chosen;
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
            route_provider: route_provider.clone(),
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
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&review_path), &run_key, &artifacts_dir)
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
                Some(&review_path),
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
            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => true,
            KeyCode::Char('y') | KeyCode::Enter => {
                self.clear_agent_error();
                self.queue_view_of_current_artifact("plan.md");
                // Defer the ShardingRunning transition until *after* the
                // editor closes. Otherwise plan-schema validation runs against
                // the pre-edit copy, fails silently (the result was discarded
                // by `let _ =`), and the modal stays up while vim still opens.
                self.pending_post_view_phase = Some(Phase::ShardingRunning);
                false
            }
            KeyCode::Char('n') => {
                let _ = self.transition_to_phase(Phase::PlanReviewRunning);
                self.launch_plan_review();
                false
            }
            // Consume all other keys so the UI is genuinely modal.
            _ => false,
        }
    }
    /// Co-located success-finalization for `Phase::PlanReviewRunning`.
    pub(crate) fn finalize_plan_review_success(
        &mut self,
        run: &crate::state::RunRecord,
    ) -> Result<()> {
        self.finalize_run_record(run.id, true, None);
        self.clear_agent_error();
        self.transition_to_phase(Phase::PlanReviewPaused)?;
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
    use crate::app::test_support::{key, mk_app, with_temp_root};
    use crate::state::{self as session_state, Phase, SessionState};
    use crossterm::event::KeyCode;

    const VALID_PLAN: &str = r#"# Example Plan

## Goal Description
Ship the feature.

## Acceptance Criteria
- AC-1: cover the main path
  - Positive Tests (expected to PASS):
    - accepts a valid input
  - Negative Tests (expected to FAIL):
    - rejects an invalid input

## Path Boundaries

### Upper Bound (Maximum Scope)
Ceiling.

### Lower Bound (Minimum Scope)
Floor.

### Allowed Choices
- Can use: existing helpers
- Cannot use: new third-party crates

## Dependencies and Sequence
1. Milestone 1: implement it.
"#;

    const INVALID_PLAN: &str = r#"# Unstructured Plan

## Not The Schema
Missing every required section.
"#;

    fn write_plan(session_id: &str, body: &str) {
        let path = session_state::session_dir(session_id)
            .join("artifacts")
            .join("plan.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn paused_app(session_label: &str, plan_body: &str) -> crate::app::App {
        let mut state = SessionState::new(session_label.to_string());
        state.current_phase = Phase::PlanReviewPaused;
        write_plan(&state.session_id, plan_body);
        mk_app(state)
    }

    #[test]
    fn enter_in_paused_modal_defers_transition_until_after_editor() {
        with_temp_root(|| {
            let mut app = paused_app("plan-review-defer", INVALID_PLAN);
            app.handle_plan_review_paused_modal_key(key(KeyCode::Enter));

            // Phase must NOT advance synchronously: validation runs *after*
            // the editor closes so the operator can fix the plan first.
            assert_eq!(app.state.current_phase, Phase::PlanReviewPaused);
            assert!(
                app.pending_view_path.is_some(),
                "editor must be queued for plan.md"
            );
            assert_eq!(
                app.pending_post_view_phase,
                Some(Phase::ShardingRunning),
                "post-editor transition target must be recorded"
            );
            assert!(app.state.agent_error.is_none());
        });
    }

    #[test]
    fn post_editor_tick_advances_to_sharding_when_plan_validates() {
        with_temp_root(|| {
            let mut app = paused_app("plan-review-success", VALID_PLAN);
            app.handle_plan_review_paused_modal_key(key(KeyCode::Enter));

            // Simulate the runtime taking the queued path to launch the editor.
            let _ = app.take_pending_view_path();

            app.runtime_tick_before_data_drain()
                .expect("tick must succeed");

            assert_eq!(app.state.current_phase, Phase::ShardingRunning);
            assert!(app.pending_post_view_phase.is_none());
            assert!(app.state.agent_error.is_none());
        });
    }

    #[test]
    fn post_editor_tick_surfaces_error_when_plan_still_invalid() {
        with_temp_root(|| {
            let mut app = paused_app("plan-review-still-invalid", INVALID_PLAN);
            app.handle_plan_review_paused_modal_key(key(KeyCode::Enter));

            // Editor "ran" but the operator didn't fix the schema breakage.
            let _ = app.take_pending_view_path();

            app.runtime_tick_before_data_drain()
                .expect("tick must succeed");

            // Modal stays up so the operator can retry; the validation error
            // surfaces through agent_error instead of being silently swallowed.
            assert_eq!(app.state.current_phase, Phase::PlanReviewPaused);
            assert!(app.pending_post_view_phase.is_none());
            let err = app
                .state
                .agent_error
                .as_deref()
                .expect("validation error must surface");
            assert!(
                err.contains("plan schema validation failed"),
                "unexpected error text: {err}"
            );
        });
    }

    #[test]
    fn tick_before_editor_runs_does_not_apply_pending_transition() {
        with_temp_root(|| {
            let mut app = paused_app("plan-review-pre-editor", VALID_PLAN);
            app.handle_plan_review_paused_modal_key(key(KeyCode::Enter));

            // The runtime calls runtime_tick_before_data_drain *after* draining
            // pending_view_path each iteration, but a stray tick before the
            // editor opens must not race ahead and apply the transition.
            app.runtime_tick_before_data_drain()
                .expect("tick must succeed");

            assert_eq!(app.state.current_phase, Phase::PlanReviewPaused);
            assert_eq!(
                app.pending_post_view_phase,
                Some(Phase::ShardingRunning),
                "transition stays deferred while editor is still queued"
            );
        });
    }
}
