use anyhow::Result;

use crate::adapters::{AgentRun, run_label_with_model};
use crate::app::prompts::{
    ReviewerPromptInputs, assigned_revise_task_ids, read_review_scope, reviewer_prompt,
    rewrite_tasks_for_revise, task_effort_for,
};
use crate::app::{App, guard};
use crate::review;
use crate::selection::CachedModel;
use crate::state::{
    self as session_state, Message, MessageKind, MessageSender, Phase, PipelineItemStatus,
};

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
        let run_id = self.state.next_agent_run_id();
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
            self.runner_supervisor.launch_noninteractive(
                run_id,
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

    /// Co-located success-finalization for `Phase::ReviewRound(round)`.
    pub(crate) fn finalize_reviewer_success(
        &mut self,
        run: &crate::state::RunRecord,
        round: u32,
    ) -> Result<()> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let review_path = session_dir
            .join("rounds")
            .join(format!("{round:03}"))
            .join("review.toml");
        let verdict = review::validate(&review_path)?;
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
                    self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
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
                    self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
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
                        let assigned_ids =
                            assigned_revise_task_ids(&self.state.builder, new_tasks.len());
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
            review::ReviewStatus::HumanBlocked | review::ReviewStatus::AgentPivot => {
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
                        // SAFETY: the enclosing outer match arm only matches
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
                let trigger_summary = (!summary.trim().is_empty()).then_some(summary);
                self.enter_builder_recovery(
                    round,
                    self.state.builder.current_task_id(),
                    trigger_summary,
                    trigger_str,
                );
            }
        }
        Ok(())
    }
}
