use super::{
    RetryTarget, Severity, parse_task_label_id, retry_stage_for_label, retry_target_for_run,
};
use crate::app::App;
use crate::app::prompts::restore_artifacts;
use crate::app::tree::node_at_path;
use crate::artifacts::ArtifactKind;
use crate::lifecycle::{LifecycleOps, slim_phase_for_stage_retry, slim_phase_for_task_retry};
use crate::logic::rules::retry_phase_for_stage;
use crate::scheduler::{is_implementation_lane_phase, manual_retry_allowed};
use crate::state::{self as session_state, NodeKind, Phase, RunStatus};
use std::time::Duration;
impl App {
    pub(crate) fn retry_allowed_by_project_lane(&mut self, target_phase: Phase) -> bool {
        if !is_implementation_lane_phase(target_phase) {
            return true;
        }
        let sessions_root = crate::picker::sessions_root_for(&self.config);
        let scan = match crate::data::picker_io::scan_sessions_for_scheduler(&sessions_root) {
            Ok(scan) => scan,
            Err(err) => {
                let message = format!("retry blocked: cannot scan sessions for lane gate: {err:#}");
                self.surface_boundary_error(message, false);
                return false;
            }
        };
        if manual_retry_allowed(target_phase, &self.state.session_id, &scan) {
            return true;
        }
        if !scan
            .iter()
            .any(|entry| entry.session_id() == self.state.session_id)
        {
            // Isolated App tests and path-based sessions can run outside the
            // project scan; only shell-owned focused sessions can be lane-gated.
            return true;
        }
        // Manual implementation retries share the scheduler's project-wide
        // lane gate; otherwise a focused session could start sharding or
        // later work while a background session is already mutating the repo.
        let message = "retry blocked: implementation lane is occupied by another session";
        let _ = self.state.log_event(message);
        self.push_status(message.to_string(), Severity::Warn, Duration::from_secs(5));
        false
    }

    pub(crate) fn retry_gate_phase_for_stage(stage: &str) -> Option<Phase> {
        match stage {
            "sharding" => Some(Phase::ShardingRunning),
            "repo-state-update" => Some(Phase::RepoStateUpdateRunning),
            _ => retry_phase_for_stage(stage),
        }
    }

    pub(crate) fn retry_gate_phase_for_stage_id(stage_id: crate::app::StageId) -> Phase {
        match stage_id {
            crate::app::StageId::Brainstorm => Phase::BrainstormRunning,
            crate::app::StageId::SpecReview => Phase::SpecReviewRunning,
            crate::app::StageId::Planning => Phase::PlanningRunning,
            crate::app::StageId::PlanReview => Phase::PlanReviewRunning,
            crate::app::StageId::Sharding => Phase::ShardingRunning,
            crate::app::StageId::Implementation => Phase::ImplementationRound(1),
            crate::app::StageId::Review => Phase::ReviewRound(1),
            crate::app::StageId::FinalValidation => Phase::FinalValidation(1),
            crate::app::StageId::Dreaming => Phase::Dreaming(1),
        }
    }

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
        let target_phase = match target {
            RetryTarget::Task(task_id) => slim_phase_for_task_retry(task_id, &self.state),
            RetryTarget::Stage(stage) => slim_phase_for_stage_retry(stage),
        };
        self.run_lifecycle_op("retry", |ctx| LifecycleOps::rewind(ctx, target_phase));
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
                let rounds: Vec<u32> = self
                    .state
                    .agent_runs
                    .iter()
                    .filter(|r| r.stage == "spec-review")
                    .map(|r| r.round)
                    .collect();
                for round in rounds {
                    let _ = fs::remove_file(artifacts.join(format!("spec-review-{round}.md")));
                    let _ = fs::remove_file(prompts.join(format!("spec-review-{round}.md")));
                }
                self.clear_agent_error();
                let _ = self.transition_to_phase(Phase::BrainstormRunning);
            }
            Phase::PlanningRunning => {
                self.cancel_run_label("[Planning]");
                let _ = fs::remove_file(artifacts.join("plan.md"));
                let _ = self.transition_to_phase(Phase::SpecReviewRunning);
            }
            Phase::PlanReviewRunning => {
                self.cancel_run_label("[Plan Review 1]");
                let rounds: Vec<u32> = self
                    .state
                    .agent_runs
                    .iter()
                    .filter(|r| r.stage == "plan-review")
                    .map(|r| r.round)
                    .collect();
                for round in rounds {
                    let _ = fs::remove_file(artifacts.join(format!("plan-review-{round}.md")));
                    let _ = fs::remove_file(prompts.join(format!("plan-review-{round}.md")));
                }
                let plan_backup = artifacts.join("plan.pre-review-1.md");
                let spec_backup = artifacts.join("spec.pre-review-1.md");
                restore_artifacts(&[
                    (plan_backup.as_path(), artifacts.join("plan.md").as_path()),
                    (spec_backup.as_path(), artifacts.join("spec.md").as_path()),
                ]);
                self.clear_agent_error();
                let _ = self.transition_to_phase(Phase::PlanningRunning);
            }
            Phase::PlanReviewPaused => {
                let plan_backup = artifacts.join("plan.pre-review-1.md");
                let spec_backup = artifacts.join("spec.pre-review-1.md");
                restore_artifacts(&[
                    (plan_backup.as_path(), artifacts.join("plan.md").as_path()),
                    (spec_backup.as_path(), artifacts.join("spec.md").as_path()),
                ]);
                let rounds: Vec<u32> = self
                    .state
                    .agent_runs
                    .iter()
                    .filter(|r| r.stage == "plan-review")
                    .map(|r| r.round)
                    .collect();
                for round in rounds {
                    let _ = fs::remove_file(artifacts.join(format!("plan-review-{round}.md")));
                    let _ = fs::remove_file(prompts.join(format!("plan-review-{round}.md")));
                }
                let _ = fs::remove_file(artifacts.join("plan.pre-review-1.md"));
                let _ = fs::remove_file(artifacts.join("spec.pre-review-1.md"));
                let _ = self.transition_to_phase(Phase::PlanningRunning);
            }
            Phase::ShardingRunning => {
                self.cancel_run_label("[Sharding]");
                let _ = fs::remove_file(artifacts.join("tasks.toml"));
                let _ = fs::remove_file(prompts.join("sharding.md"));
                let _ = self.transition_to_phase(Phase::PlanReviewRunning);
            }
            Phase::ImplementationRound(r) => {
                self.cancel_run_label(&format!("[Builder r{r}]"));
                let _ = fs::remove_dir_all(session_dir.join("rounds").join(format!("{r:03}")));
                let prev = if r <= 1 {
                    if self.state.skip_to_impl_rationale.is_some() {
                        Phase::BrainstormRunning
                    } else {
                        session_state::reset_builder_after_rewind(&mut self.state);
                        // Spec §Data model line 96: rewind from round 1 must
                        // pause in WaitingToImplement so the scheduler re-verifies
                        // the repository baseline before any sharding launch.
                        Phase::WaitingToImplement
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
                session_state::clear_skip_to_impl_proposal(&mut self.state);
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
            Phase::FinalValidation(r) => {
                // Validator is non-mutating and can be rewound. Route back to
                // ReviewRound(r) for any round; round-1 sessions on the
                // skip-to-impl path can additionally rewind to ImplementationRound(1)
                // via existing transitions, but the default rewind target is the
                // matching review round to preserve per-task review history.
                self.cancel_run_label("[FinalValidation]");
                let target = if r >= 1 {
                    Phase::ReviewRound(r)
                } else {
                    Phase::ImplementationRound(1)
                };
                let _ = self.transition_to_phase(target);
            }
            Phase::Simplification(r) => {
                // Simplification is a code-producing stage; rewind to the
                // matching ReviewRound to drop back into the loop on round >= 1
                // or to ImplementationRound(1) on the skip-to-impl path.
                self.cancel_run_label("[Simplifier]");
                let target = if r >= 1 {
                    Phase::ReviewRound(r)
                } else {
                    Phase::ImplementationRound(1)
                };
                let _ = self.transition_to_phase(target);
            }
            Phase::IdeaInput
            | Phase::BlockedNeedsUser
            | Phase::WaitingToImplement
            | Phase::RepoStateUpdateRunning
            | Phase::Done
            | Phase::Cancelled => {}
        }
        self.clear_agent_error();
        self.run_launched = false;
        self.current_run_id = None;
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        self.save_state();
    }
}
