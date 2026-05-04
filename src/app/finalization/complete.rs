use anyhow::{Context, Result};

use crate::app::{App, TerminationIntent};
use crate::app::prompts::{
    assigned_revise_task_ids, read_review_scope, rewrite_tasks_for_revise, write_review_scope_artifact,
};
use crate::artifacts::{ArtifactKind, SkipToImplProposal};
use crate::final_validation::{self, ValidationStatus};
use crate::review;
use crate::state::{
    self as session_state, Message, MessageKind, MessageSender, Phase, PipelineItemStatus,
};
use crate::tasks;

impl App {
    pub(crate) fn complete_run_finalization(
        &mut self,
        run: &crate::state::RunRecord,
        failure_reason: Option<String>,
    ) -> Result<()> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        if let Some(error) = failure_reason {
            self.finalize_run_record(run.id, false, Some(error.clone()));
            let pending_termination = self
                .pending_termination
                .as_ref()
                .filter(|pending| pending.run_id == run.id)
                .cloned();
            if let Some(pending) = pending_termination {
                self.pending_termination = None;
                self.clear_agent_error();
                match pending.intent {
                    TerminationIntent::StopOnly => {}
                    TerminationIntent::StopAndRetry(retry) => {
                        self.launch_retry_from_descriptor(retry);
                    }
                    TerminationIntent::StopAndQuit => {
                        self.pending_app_exit = true;
                    }
                }
                return Ok(());
            }
            if matches!(error.as_str(), "Operator Killed" | "user_forced_retry") {
                self.clear_agent_error();
                return Ok(());
            }
            if run.stage == "final-validation" {
                self.record_agent_error(error);
                self.transition_to_blocked(crate::state::BlockOrigin::FinalValidation)?;
                return Ok(());
            }
            let failed_run = self
                .state
                .agent_runs
                .iter()
                .find(|candidate| candidate.id == run.id)
                .cloned()
                .unwrap_or_else(|| run.clone());
            if !self.maybe_auto_retry(&failed_run) {
                self.record_agent_error(error);
            }
            return Ok(());
        }
        match self.state.current_phase {
            Phase::BrainstormRunning => {
                let skip_artifact_path = session_dir
                    .join("artifacts")
                    .join(ArtifactKind::SkipToImpl.filename());
                let proposal = match SkipToImplProposal::read_from_path(&skip_artifact_path) {
                    Ok((p, warnings)) => {
                        for w in warnings {
                            let _ = self
                                .state
                                .log_event(format!("warning: skip_proposal.toml: {w}"));
                        }
                        p
                    }
                    Err(err) => {
                        let _ = self.state.log_event(format!(
                            "warning: skip_proposal.toml malformed or invalid, falling through to spec review: {err:#}"
                        ));
                        None
                    }
                };

                let summary_path = session_dir
                    .join("artifacts")
                    .join(ArtifactKind::SessionSummary.filename());
                match crate::artifacts::SessionSummaryArtifact::read_from_path(&summary_path) {
                    Ok(Some(summary)) => {
                        session_state::transitions::record_session_title(
                            &mut self.state,
                            summary.title.trim().to_string(),
                        );
                    }
                    Ok(None) => {}
                    Err(err) => {
                        let _ = self.state.log_event(format!(
                            "warning: session_summary.toml malformed or invalid, leaving title unset: {err:#}"
                        ));
                    }
                }

                self.finalize_run_record(run.id, true, None);
                self.clear_agent_error();

                match proposal {
                    Some(p) if p.proposed => {
                        session_state::transitions::record_skip_to_impl_proposal(
                            &mut self.state,
                            p.rationale,
                            p.status,
                        );
                        self.transition_to_phase(Phase::SkipToImplPending)?;
                    }
                    _ => {
                        self.transition_to_phase(Phase::SpecReviewRunning)?;
                    }
                }
            }
            Phase::SpecReviewRunning => {
                self.finalize_run_record(run.id, true, None);
                self.clear_agent_error();
                self.transition_to_phase(Phase::SpecReviewPaused)?;
                self.append_system_message(
                    run.id,
                    MessageKind::Summary,
                    "Spec review complete.".to_string(),
                );
                if run.modes.yolo {
                    self.auto_approve_spec_review_pause("spec_approval");
                }
            }
            Phase::PlanningRunning => {
                self.finalize_run_record(run.id, true, None);
                self.clear_agent_error();
                // Spec line 46 conjoins yolo plan-review skip with `artifacts/plan.md` existing.
                // The successful-finalization context already implies the artifact, but the
                // explicit guard protects against a planning agent that reports success
                // without writing the file.
                let plan_path = session_state::session_dir(&self.state.session_id)
                    .join("artifacts")
                    .join("plan.md");
                if run.modes.yolo && Self::artifact_present(&plan_path) {
                    self.log_yolo_auto_approved("plan_review_skipped");
                    self.transition_to_phase(Phase::ShardingRunning)?;
                } else {
                    self.transition_to_phase(Phase::PlanReviewRunning)?;
                }
            }
            Phase::PlanReviewRunning => {
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
            }
            Phase::ShardingRunning => {
                let tasks_path = session_dir.join("artifacts").join("tasks.toml");
                let parsed = tasks::validate(&tasks_path)
                    .with_context(|| format!("invalid {}", tasks_path.display()));
                match parsed {
                    Ok(parsed) => {
                        session_state::transitions::initialize_task_pipeline(
                            &mut self.state,
                            parsed
                                .tasks
                                .iter()
                                .map(|task| (task.id, task.title.clone())),
                        );
                        self.finalize_run_record(run.id, true, None);
                        self.clear_agent_error();
                        self.transition_to_phase(Phase::ImplementationRound(1))?;
                    }
                    Err(err) => return Err(err),
                }
            }
            Phase::ImplementationRound(round) => {
                let round_dir = session_dir.join("rounds").join(format!("{round:03}"));
                let scope = read_review_scope(&round_dir.join("review_scope.toml"))?;
                let _ = write_review_scope_artifact(&round_dir, &scope.base_sha);
                self.finalize_run_record(run.id, true, None);
                self.clear_agent_error();
                if round == 1 && self.state.skip_to_impl_rationale.is_some() {
                    self.enter_simplification_or_done(1, run.modes.yolo)?;
                } else {
                    self.transition_to_phase(Phase::ReviewRound(round))?;
                }
            }
            Phase::ReviewRound(round) => {
                let review_path = session_dir
                    .join("rounds")
                    .join(format!("{round:03}"))
                    .join("review.toml");
                match review::validate(&review_path) {
                    Ok(verdict) => {
                        let summary_text = verdict.summary.trim();
                        if !summary_text.is_empty() {
                            let kind = match verdict.status {
                                review::ReviewStatus::Approved | review::ReviewStatus::Refine => {
                                    MessageKind::Summary
                                }
                                review::ReviewStatus::Revise
                                | review::ReviewStatus::HumanBlocked
                                | review::ReviewStatus::AgentPivot => MessageKind::SummaryWarn,
                            };
                            let msg = Message {
                                ts: chrono::Utc::now(),
                                run_id: run.id,
                                kind,
                                sender: MessageSender::Agent {
                                    model: run.model.clone(),
                                    vendor: run.vendor.clone(),
                                },
                                text: summary_text.to_string(),
                            };
                            if let Err(err) = self.state.append_message(&msg) {
                                let _ = self.state.log_event(format!(
                                    "failed to append review summary message for run {}: {err}",
                                    run.id
                                ));
                            } else {
                                self.messages.push(msg);
                            }
                        }
                        self.finalize_run_record(run.id, true, None);
                        self.clear_agent_error();
                        session_state::transitions::record_builder_verdict(
                            &mut self.state,
                            format!("{:?}", verdict.status).to_lowercase(),
                        );
                        match verdict.status {
                            review::ReviewStatus::Approved => {
                                // Advisory feedback on an approved verdict is non-blocking;
                                // surface it to the UI but continue the pipeline.
                                if !verdict.feedback.is_empty() {
                                    let advisory = format!(
                                        "advisory ({}): {}",
                                        verdict.feedback.len(),
                                        verdict.feedback[0].trim()
                                    );
                                    let advisory_msg = Message {
                                        ts: chrono::Utc::now(),
                                        run_id: run.id,
                                        kind: MessageKind::SummaryWarn,
                                        sender: MessageSender::Agent {
                                            model: run.model.clone(),
                                            vendor: run.vendor.clone(),
                                        },
                                        text: advisory,
                                    };
                                    if let Err(err) = self.state.append_message(&advisory_msg) {
                                        let _ = self.state.log_event(format!(
                                            "failed to append advisory feedback message: {err}"
                                        ));
                                    } else {
                                        self.messages.push(advisory_msg);
                                    }
                                }
                                if let Some(task_id) = self.state.builder.current_task_id() {
                                    let _ = session_state::transitions::mark_task_status(
                                        &mut self.state,
                                        task_id,
                                        PipelineItemStatus::Approved,
                                        Some(round),
                                    );
                                }
                                if !self.state.builder.has_unfinished_tasks() {
                                    self.enter_simplification_or_done(round, run.modes.yolo)?;
                                } else {
                                    self.transition_to_phase(Phase::ImplementationRound(
                                        round + 1,
                                    ))?;
                                }
                            }
                            review::ReviewStatus::Refine => {
                                // Approve the current task and stash feedback for
                                // the next coder. No re-review of this round.
                                session_state::transitions::append_refine_feedback(
                                    &mut self.state,
                                    verdict.feedback.iter().cloned(),
                                );
                                if let Some(task_id) = self.state.builder.current_task_id() {
                                    let _ = session_state::transitions::mark_task_status(
                                        &mut self.state,
                                        task_id,
                                        PipelineItemStatus::Approved,
                                        Some(round),
                                    );
                                }
                                if !self.state.builder.has_unfinished_tasks() {
                                    self.enter_simplification_or_done(round, run.modes.yolo)?;
                                } else {
                                    self.transition_to_phase(Phase::ImplementationRound(
                                        round + 1,
                                    ))?;
                                }
                            }
                            review::ReviewStatus::Revise => {
                                if let Some(task_id) = self.state.builder.current_task_id() {
                                    if verdict.new_tasks.is_empty() {
                                        let _ = session_state::transitions::mark_task_status(
                                            &mut self.state,
                                            task_id,
                                            PipelineItemStatus::Revise,
                                            Some(round),
                                        );
                                    } else {
                                        let new_tasks = verdict
                                            .new_tasks
                                            .iter()
                                            .map(|task| {
                                                (
                                                    task.title.clone(),
                                                    task.description.clone(),
                                                    task.test.clone(),
                                                    task.estimated_tokens,
                                                )
                                            })
                                            .collect::<Vec<_>>();
                                        let assigned_ids = assigned_revise_task_ids(
                                            &self.state.builder,
                                            new_tasks.len(),
                                        );
                                        rewrite_tasks_for_revise(
                                            &session_dir,
                                            task_id,
                                            &verdict.new_tasks,
                                            &assigned_ids,
                                        )?;
                                        session_state::transitions::apply_revise_with_new_tasks(
                                            &mut self.state,
                                            task_id,
                                            new_tasks,
                                        );
                                    }
                                }
                                self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
                            }
                            review::ReviewStatus::HumanBlocked
                            | review::ReviewStatus::AgentPivot => {
                                let (verdict_status, trigger_str) = match verdict.status {
                                    review::ReviewStatus::HumanBlocked => {
                                        (PipelineItemStatus::HumanBlocked, "human_blocked")
                                    }
                                    review::ReviewStatus::AgentPivot => {
                                        (PipelineItemStatus::AgentPivot, "agent_pivot")
                                    }
                                    review::ReviewStatus::Approved
                                    | review::ReviewStatus::Refine
                                    | review::ReviewStatus::Revise => {
                                        // SAFETY: the enclosing outer match arm at :3196 only matches
                                        // `HumanBlocked | AgentPivot`, so the other ReviewStatus
                                        // variants cannot reach this inner match.
                                        unreachable!("already handled")
                                    }
                                };
                                if let Some(task_id) = self.state.builder.current_task_id() {
                                    let _ = session_state::transitions::mark_task_status(
                                        &mut self.state,
                                        task_id,
                                        verdict_status,
                                        Some(round),
                                    );
                                }
                                let summary = verdict.feedback.join("\n");
                                let trigger_summary =
                                    (!summary.trim().is_empty()).then_some(summary);
                                self.enter_builder_recovery(
                                    round,
                                    self.state.builder.current_task_id(),
                                    trigger_summary,
                                    trigger_str,
                                );
                            }
                        }
                    }
                    Err(err) => return Err(err),
                }
            }
            Phase::BuilderRecovery(round) => match self.reconcile_builder_recovery(run.id) {
                Ok(()) => {
                    self.finalize_run_record(run.id, true, None);
                    self.clear_agent_error();
                    if run.modes.yolo {
                        // Recovery has already validated `recovery.toml`/`tasks.toml`; yolo
                        // delegates the review gate, not the artifact validation step.
                        self.log_yolo_auto_approved("recovery_plan_review_skipped");
                        self.queue_recovery_sharding_pipeline_item(round);
                        self.transition_to_phase(Phase::BuilderRecoverySharding(round))?;
                    } else {
                        // Insert the recovery-mode plan review pipeline item before
                        // transitioning so the UI shows it as the next pending stage.
                        session_state::transitions::queue_recovery_plan_review(
                            &mut self.state,
                            round,
                        );
                        self.transition_to_phase(Phase::BuilderRecoveryPlanReview(round))?;
                    }
                }
                Err(err) => {
                    let reason = format!("recovery_reconcile_failed: {err:#}");
                    self.finalize_run_record(run.id, false, Some(reason.clone()));
                    let failed_run = self
                        .state
                        .agent_runs
                        .iter()
                        .find(|candidate| candidate.id == run.id)
                        .cloned()
                        .unwrap_or_else(|| run.clone());
                    if !self.maybe_auto_retry(&failed_run) {
                        self.record_agent_error(reason);
                    }
                }
            },
            Phase::BuilderRecoveryPlanReview(round) => {
                self.handle_recovery_plan_review_completed(run, round)?;
            }
            Phase::BuilderRecoverySharding(round) => {
                self.handle_recovery_sharding_completed(run, round)?;
            }
            Phase::FinalValidation(round) => {
                let verdict_path = session_dir
                    .join("artifacts")
                    .join(format!("final_validation_{round}.toml"));
                let verdict = final_validation::validate(&verdict_path)
                    .with_context(|| format!("invalid {}", verdict_path.display()))?;
                self.finalize_run_record(run.id, true, None);
                self.clear_agent_error();
                match verdict.status {
                    ValidationStatus::GoalMet => {
                        self.transition_to_phase(Phase::Done)?;
                    }
                    ValidationStatus::GoalGap => {
                        let verdict_artifact = format!("artifacts/final_validation_{round}.toml");
                        let new_tasks = final_validation::normalize_gap_tasks(
                            verdict.new_tasks,
                            self.state.builder.max_task_id(),
                            &verdict_artifact,
                        );
                        self.append_goal_gap_tasks(&session_dir, &new_tasks)?;
                        self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
                    }
                    ValidationStatus::NeedsHuman => {
                        self.transition_to_blocked(crate::state::BlockOrigin::FinalValidation)?;
                    }
                }
            }
            Phase::Simplification(round) => {
                // The artifact-validation gate above has already accepted
                // the simplification TOML; on success we hand control to
                // FinalValidation. The simplifier's verdict is advisory
                // only — final validation makes its own call against
                // idea + spec, so we don't branch on the parsed status here.
                self.finalize_run_record(run.id, true, None);
                self.clear_agent_error();
                let _ = session_state::transitions::enter_final_validation(&mut self.state, round)?;
            }
            Phase::IdeaInput
            | Phase::SpecReviewPaused
            | Phase::PlanReviewPaused
            | Phase::BlockedNeedsUser
            | Phase::SkipToImplPending
            | Phase::GitGuardPending
            | Phase::Done => {}
        }
        Ok(())
    }
}
