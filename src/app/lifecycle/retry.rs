use super::{
    RetryTarget, Severity, parse_task_label_id, retry_stage_for_label, retry_target_for_run,
};
use crate::app::App;
use crate::app::prompts::restore_artifacts;
use crate::app::tree::node_at_path;
use crate::artifacts::ArtifactKind;
use crate::logic::rules::retry_phase_for_stage;
use crate::state::{self as session_state, NodeKind, Phase, PipelineItemStatus, RunStatus};
use std::collections::BTreeSet;
use std::time::Duration;
impl App {
    fn cancel_run_label(&self, base: &str) {
        let prefix = format!("{base} ");
        for run in self.state.agent_runs.iter().filter(|run| {
            run.status == RunStatus::Running
                && (run.window_name == base || run.window_name.starts_with(&prefix))
        }) {
            self.runner_supervisor.cancel_run(run.id);
        }
    }
    pub(crate) fn selected_retry_target(&self) -> Option<RetryTarget> {
        let row = self.visible_rows.get(self.selected)?;
        for depth in (1..=row.path.len()).rev() {
            let node = node_at_path(&self.nodes, &row.path[..depth])?;
            if node.kind == NodeKind::Task {
                return parse_task_label_id(&node.label).map(RetryTarget::Task);
            }
            if node.kind == NodeKind::Stage
                && let Some(stage) = retry_stage_for_label(&node.label)
            {
                return Some(RetryTarget::Stage(stage));
            }
        }
        row.backing_leaf_run_id
            .and_then(|run_id| {
                self.state
                    .agent_runs
                    .iter()
                    .find(|run| run.id == run_id)
                    .and_then(retry_target_for_run)
            })
            .or_else(|| {
                self.current_run_id.and_then(|run_id| {
                    self.state
                        .agent_runs
                        .iter()
                        .find(|run| run.id == run_id)
                        .and_then(retry_target_for_run)
                })
            })
            .or_else(|| self.state.builder.current_task_id().map(RetryTarget::Task))
    }
    pub(crate) fn retry_selected_target(&mut self) {
        let Some(target) = self.selected_retry_target() else {
            self.push_status(
                "retry: select a stage or task first".to_string(),
                Severity::Warn,
                Duration::from_secs(3),
            );
            return;
        };
        match target {
            RetryTarget::Task(task_id) => self.retry_task(task_id),
            RetryTarget::Stage(stage) => self.retry_stage(stage),
        }
    }
    fn retry_task(&mut self, task_id: u32) {
        let task_rounds = self
            .state
            .agent_runs
            .iter()
            .filter(|run| run.task_id == Some(task_id))
            .map(|run| run.round)
            .collect::<BTreeSet<_>>();
        let retry_round = task_rounds
            .iter()
            .next_back()
            .copied()
            .or(match self.state.current_phase {
                Phase::ImplementationRound(round) | Phase::ReviewRound(round) => Some(round),
                Phase::BuilderRecovery(round)
                | Phase::BuilderRecoveryPlanReview(round)
                | Phase::BuilderRecoverySharding(round) => Some(round),
                _ => None,
            })
            .unwrap_or(1);
        let recovery_context_matches = self.state.builder.recovery_trigger_task_id == Some(task_id);
        let removed_runs = self
            .state
            .agent_runs
            .iter()
            .filter(|run| {
                run.task_id == Some(task_id)
                    || (recovery_context_matches
                        && task_rounds.contains(&run.round)
                        && run.task_id.is_none()
                        && (run.stage == "recovery"
                            || run.window_name.contains("[Recovery Plan Review]")
                            || run.window_name.contains("[Recovery Sharding]")))
            })
            .cloned()
            .collect::<Vec<_>>();
        if removed_runs.is_empty() {
            self.push_status(
                format!("retry: no attempt logs for task {task_id}"),
                Severity::Warn,
                Duration::from_secs(3),
            );
            return;
        }
        // Preserve failed RunRecord rows and their messages so the next
        // attempt's prompt can include the prior-attempt transcript and
        // the operator does not have to re-type previously-answered
        // recovery decisions. Only running runs are preempted.
        let prior_ids = self.preempt_prior_runs(&removed_runs);
        self.failed_models.retain(|(stage, key_task_id, _), _| {
            *key_task_id != Some(task_id) && stage != "recovery"
        });
        if let Some(item) = self
            .state
            .builder
            .pipeline_items
            .iter_mut()
            .find(|item| item.stage == "coder" && item.task_id == Some(task_id))
        {
            item.status = PipelineItemStatus::Pending;
            item.round = None;
        } else {
            self.state
                .builder
                .push_pipeline_item(crate::state::PipelineItem {
                    id: 0,
                    stage: "coder".to_string(),
                    task_id: Some(task_id),
                    round: None,
                    status: PipelineItemStatus::Pending,
                    title: self.state.builder.task_titles.get(&task_id).cloned(),
                    mode: None,
                    trigger: None,
                    interactive: None,
                    iteration: self.state.builder.iteration.max(1),
                });
        }
        self.clear_agent_error();
        self.current_run_id = None;
        self.run_launched = false;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        session_state::transitions::set_phase_for_operator_retry(
            &mut self.state,
            Phase::ImplementationRound(retry_round),
        );
        let _ = self.state.log_event(format!(
            "palette_retry: task={task_id} prior_runs={}",
            prior_ids.len()
        ));
        let _ = self.state.save();
        self.rebuild_tree_view(None);
        self.launch_coder();
    }
    fn retry_stage(&mut self, stage: &'static str) {
        let removed_runs = self
            .state
            .agent_runs
            .iter()
            .filter(|run| run.stage == stage && run.task_id.is_none())
            .cloned()
            .collect::<Vec<_>>();
        if removed_runs.is_empty() {
            self.push_status(
                format!(
                    "retry: no attempt logs for {}",
                    RetryTarget::Stage(stage).label()
                ),
                Severity::Warn,
                Duration::from_secs(3),
            );
            return;
        }
        let prior_ids = self.preempt_prior_runs(&removed_runs);
        self.failed_models
            .retain(|(key_stage, key_task_id, _), _| key_stage != stage || key_task_id.is_some());
        self.clear_agent_error();
        self.current_run_id = None;
        self.run_launched = false;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        if let Some(phase) = retry_phase_for_stage(stage) {
            session_state::transitions::set_phase_for_operator_retry(&mut self.state, phase);
        }
        let _ = self.state.log_event(format!(
            "palette_retry: stage={stage} prior_runs={}",
            prior_ids.len()
        ));
        let _ = self.state.save();
        self.rebuild_tree_view(None);
        match stage {
            "brainstorm" => {
                let idea = self.state.idea_text.clone().unwrap_or_default();
                self.launch_brainstorm(idea);
            }
            "spec-review" => self.launch_spec_review(),
            "planning" => self.launch_planning(),
            "plan-review" => self.launch_plan_review(),
            "sharding" => self.launch_sharding(),
            _ => {}
        }
    }
    /// Preempt the prior runs of a stage so the operator can launch a
    /// fresh attempt — but keep the failed `RunRecord`s and their
    /// `messages.toml` rows. The new attempt's prompt builder reads them
    /// to render the prior-attempts transcript so the operator does not
    /// have to re-answer questions they already answered upstream. Files
    /// under `artifacts/` are kept too: every per-attempt path is
    /// suffixed with the attempt number, so they coexist with the
    /// new attempt's outputs and serve as audit trail.
    fn preempt_prior_runs(&mut self, prior_runs: &[crate::state::RunRecord]) -> BTreeSet<u64> {
        let prior_ids = prior_runs.iter().map(|run| run.id).collect::<BTreeSet<_>>();
        for run in prior_runs {
            if run.status == RunStatus::Running {
                self.cancel_run_label(&run.window_name);
                self.finalize_run_record(
                    run.id,
                    false,
                    Some("preempted by operator retry".to_string()),
                );
            }
        }
        prior_ids
    }
    pub(crate) fn go_back(&mut self) {
        use std::fs;
        let session_dir = self.session_dir();
        let artifacts = session_dir.join("artifacts");
        let prompts = session_dir.join("prompts");
        // Interrupt the running agent (if any) so rewinding takes effect even
        // when the phase-specific cancel_run_label base doesn't match the launch
        // run label (e.g. "[Spec Review 1]" vs "[Spec Review]").
        if let Some(run_id) = self.current_run_id {
            let running = self
                .state
                .agent_runs
                .iter()
                .find(|r| r.id == run_id)
                .cloned();
            if let Some(run) = running {
                self.cancel_run_label(&run.window_name);
                if run.status == crate::state::RunStatus::Running {
                    self.finalize_run_record(run_id, false, Some("aborted by user".to_string()));
                }
            }
        }
        match self.state.current_phase {
            Phase::BrainstormRunning => {
                self.cancel_run_label("[Brainstorm]");
                let _ = fs::remove_file(artifacts.join("spec.md"));
                let _ = fs::remove_file(prompts.join("brainstorm.md"));
                self.clear_agent_error();
                let _ = self.transition_to_phase(Phase::IdeaInput);
            }
            Phase::SpecReviewRunning | Phase::SpecReviewPaused => {
                self.cancel_run_label("[Spec Review]");
                let _ = fs::remove_file(artifacts.join("spec-review-1.md"));
                let _ = fs::remove_file(prompts.join("spec-review-1.md"));
                // TODO(Task 2): clean up all review artifacts by RunRecord instead of the
                // removed spec_reviewers/phase_models state.
                let _ = self.transition_to_phase(Phase::BrainstormRunning);
            }
            Phase::PlanningRunning => {
                self.cancel_run_label("[Planning]");
                let _ = fs::remove_file(artifacts.join("plan.md"));
                let _ = self.transition_to_phase(Phase::SpecReviewRunning);
            }
            Phase::PlanReviewRunning => {
                self.cancel_run_label("[Plan Review 1]");
                let _ = fs::remove_file(artifacts.join("plan-review-1.md"));
                let _ = fs::remove_file(prompts.join("plan-review-1.md"));
                let plan_backup = artifacts.join("plan.pre-review-1.md");
                let spec_backup = artifacts.join("spec.pre-review-1.md");
                restore_artifacts(&[
                    (plan_backup.as_path(), artifacts.join("plan.md").as_path()),
                    (spec_backup.as_path(), artifacts.join("spec.md").as_path()),
                ]);
                self.clear_agent_error();
                // TODO(Task 2): restore the paused/running distinction from RunRecord state.
                let _ = self.transition_to_phase(Phase::PlanningRunning);
            }
            Phase::PlanReviewPaused => {
                let plan_backup = artifacts.join("plan.pre-review-1.md");
                let spec_backup = artifacts.join("spec.pre-review-1.md");
                restore_artifacts(&[
                    (plan_backup.as_path(), artifacts.join("plan.md").as_path()),
                    (spec_backup.as_path(), artifacts.join("spec.md").as_path()),
                ]);
                let _ = fs::remove_file(artifacts.join("plan-review-1.md"));
                let _ = fs::remove_file(prompts.join("plan-review-1.md"));
                let _ = fs::remove_file(artifacts.join("plan.pre-review-1.md"));
                let _ = fs::remove_file(artifacts.join("spec.pre-review-1.md"));
                // TODO(Task 2): clean up all plan review artifacts by RunRecord history.
                let _ = self.transition_to_phase(Phase::PlanningRunning);
            }
            Phase::ShardingRunning => {
                self.cancel_run_label("[Sharding]");
                let _ = fs::remove_file(artifacts.join("tasks.toml"));
                let _ = fs::remove_file(prompts.join("sharding.md"));
                // TODO(Task 2): remove sharding launch metadata from RunRecord instead of the
                // removed phase_models state.
                let _ = self.transition_to_phase(Phase::PlanReviewRunning);
            }
            Phase::ImplementationRound(r) => {
                self.cancel_run_label(&format!("[Builder r{r}]"));
                let _ = fs::remove_dir_all(session_dir.join("rounds").join(format!("{r:03}")));
                let prev = if r <= 1 {
                    if self.state.skip_to_impl_rationale.is_some() {
                        Phase::BrainstormRunning
                    } else {
                        session_state::transitions::reset_builder_after_rewind(&mut self.state);
                        Phase::ShardingRunning
                    }
                } else {
                    Phase::ReviewRound(r - 1)
                };
                let _ = self.transition_to_phase(prev);
            }
            Phase::ReviewRound(r) => {
                self.cancel_run_label(&format!("[Review r{r}]"));
                let _ = fs::remove_dir_all(session_dir.join("rounds").join(format!("{r:03}")));
                let _ = self.transition_to_phase(Phase::ImplementationRound(r));
            }
            Phase::BuilderRecovery(r) => {
                self.cancel_run_label("[Recovery]");
                let _ = fs::remove_file(prompts.join(format!("recovery-r{r}.md")));
                // Recovery is builder-only and should not be rewound into coder/reviewer; go back to
                // the triggering review round so the operator can intervene.
                let _ = self.transition_to_phase(Phase::ReviewRound(r));
            }
            Phase::BuilderRecoveryPlanReview(r) => {
                self.cancel_run_label("[Recovery Plan Review]");
                let _ = fs::remove_file(prompts.join(format!("recovery-plan-review-r{r}.md")));
                let _ = self.transition_to_phase(Phase::BuilderRecovery(r));
            }
            Phase::BuilderRecoverySharding(r) => {
                self.cancel_run_label("[Recovery Sharding]");
                let _ = fs::remove_file(prompts.join(format!("recovery-sharding-r{r}.md")));
                let _ = self.transition_to_phase(Phase::BuilderRecoveryPlanReview(r));
            }
            Phase::SkipToImplPending => {
                self.cancel_run_label("[Skip Confirm]");
                let _ = fs::remove_file(artifacts.join(ArtifactKind::SkipToImpl.filename()));
                session_state::transitions::clear_skip_to_impl_proposal(&mut self.state);
                self.clear_agent_error();
                let _ = self.transition_to_phase(Phase::BrainstormRunning);
            }
            Phase::GitGuardPending => {
                // No agent process owned by this phase; the modal is purely TUI.
                // Operator handlers are the legitimate exit path; go_back is
                // a no-op while the decision is pending.
            }
            Phase::DreamingPending => {
                // The decision is persisted specifically so resume cannot
                // re-run final validation and re-offer Dreaming; leave the
                // modal as the only exit from this phase.
            }
            Phase::Dreaming(_) => {
                self.cancel_run_label("[Dreaming]");
            }
            Phase::FinalValidation(_) => {
                // Validator is non-mutating and can be rewound. Route back to
                // ReviewRound(r) for any round; round-1 sessions on the
                // skip-to-impl path can additionally rewind to ImplementationRound(1)
                // via existing transitions, but the default rewind target is the
                // matching review round to preserve per-task review history.
                if let Phase::FinalValidation(r) = self.state.current_phase {
                    self.cancel_run_label("[FinalValidation]");
                    let target = if r >= 1 {
                        Phase::ReviewRound(r)
                    } else {
                        Phase::ImplementationRound(1)
                    };
                    let _ = self.transition_to_phase(target);
                }
            }
            Phase::Simplification(_) => {
                // Simplification is a code-producing stage; rewind to the
                // matching ReviewRound to drop back into the loop on round >= 1
                // or to ImplementationRound(1) on the skip-to-impl path.
                if let Phase::Simplification(r) = self.state.current_phase {
                    self.cancel_run_label("[Simplifier]");
                    let target = if r >= 1 {
                        Phase::ReviewRound(r)
                    } else {
                        Phase::ImplementationRound(1)
                    };
                    let _ = self.transition_to_phase(target);
                }
            }
            Phase::IdeaInput | Phase::BlockedNeedsUser | Phase::Done => {}
        }
        self.clear_agent_error();
        self.run_launched = false;
        self.current_run_id = None;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        let _ = self.state.save();
    }
}
