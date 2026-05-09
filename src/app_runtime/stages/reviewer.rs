use crate::adapters::{AgentRun, run_label_with_model};
use crate::app::prompts::{
    ReviewerPromptInputs, assigned_revise_task_ids, read_review_scope,
    reviewer_full_alignment_prompt, reviewer_prompt, rewrite_tasks_for_revise,
};
use crate::app::{App, guard};
use crate::review;
use crate::runner::select_full_alignment;
use crate::selection::CachedModel;
use crate::state::{
    self as session_state, Message, MessageKind, MessageSender, Phase, PipelineItemStatus,
};
use anyhow::Result;
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
        let requested_effort = self.task_effort_for_round(&session_dir, task_id, r);
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
        let (model, vendor_kind, vendor, route_provider, cli, launch_name) = chosen;
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
        let is_terminal_review = self.state.builder.is_terminal_review_task();
        let prompt_inputs = ReviewerPromptInputs {
            session_dir: &session_dir,
            task_id,
            round: r,
            task_file: &task_file,
            review_scope_file: &review_scope_file,
            coder_summary_file: coder_summary_path,
            review_file: &review_path,
            live_summary_path: &live_summary_path,
            is_terminal_review,
            meta: self.prompt_meta(),
        };
        // ReviewRound dispatch: cadence-driven full-alignment audit when the
        // round number is a non-zero multiple of `full_review_interval`.
        // `interval == 0` and `r == 0` keep the regular reviewer; recovery
        // rounds run on a separate phase so they cannot land here.
        let prompt = if select_full_alignment(r, self.runner_config.full_review_interval) {
            reviewer_full_alignment_prompt(prompt_inputs)
        } else {
            reviewer_prompt(prompt_inputs)
        };
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.surface_boundary_error(format!("error writing prompt: {e}"), true);
            return false;
        }
        let run = AgentRun {
            model: model.clone(),
            route_provider: route_provider.clone(),
            cli,
            launch_name,
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
                    "reviewer",
                    Some(task_id),
                    r,
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
        // Reject `refine` when this is the round's last reviewable task —
        // see `ReviewVerdict::enforce_terminal_review` and
        // `BuilderState::is_terminal_review_task` for the rationale.
        // Failure here propagates as the reviewer-run failure reason and
        // the orchestrator will surface a stage error to the operator.
        verdict.enforce_terminal_review(self.state.builder.is_terminal_review_task())?;
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
                self.approve_current_review_task_and_continue(round, run.modes.yolo)?;
            }
            review::ReviewStatus::Refine => {
                // Approve the current task and stash feedback for
                // the next coder. No re-review of this round.
                session_state::transitions::append_refine_feedback(
                    &mut self.state,
                    verdict.feedback.iter().cloned(),
                );
                self.approve_current_review_task_and_continue(round, run.modes.yolo)?;
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

    fn approve_current_review_task_and_continue(&mut self, round: u32, yolo: bool) -> Result<()> {
        if let Some(task_id) = self.state.builder.current_task_id() {
            let _ = session_state::transitions::mark_task_status(
                &mut self.state,
                task_id,
                PipelineItemStatus::Approved,
                Some(round),
            );
        }
        if self.state.builder.has_unfinished_tasks() {
            self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
        } else {
            self.enter_simplification_or_done(round, yolo)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::TestLaunchHarness;
    use crate::app::TestLaunchOutcome;
    use crate::app::test_support::{mk_app, with_temp_root};
    use crate::runner::{RunnerConfig, select_full_alignment};
    use crate::selection::{CachedModel, IpbrPhaseScores, ScoreSource, SubscriptionKind};
    use crate::state::{
        self as session_state, BuilderState, Phase, PipelineItem, PipelineItemStatus, SessionState,
    };
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    #[test]
    fn select_full_alignment_matrix() {
        // Mirrors the spec's selection rule:
        //   `interval > 0 && round > 0 && round % interval == 0`.
        // Recovery rounds run on a separate phase and therefore never land
        // here, so the table only models `ReviewRound(N)` cadence.
        for (round, interval, expected) in [
            (0u32, 5u32, false),
            (5, 5, true),
            (3, 5, false),
            (10, 5, true),
            (1, 0, false),
            (5, 0, false),
        ] {
            assert_eq!(
                select_full_alignment(round, interval),
                expected,
                "round={round} interval={interval}"
            );
        }
    }

    fn cached_review_model() -> CachedModel {
        CachedModel {
            vendor: SubscriptionKind::Codex,
            name: "review-model".to_string(),
            overall_score: 0.0,
            current_score: 0.0,
            standard_error: 0.0,
            axes: Vec::new(),
            axis_provenance: BTreeMap::new(),
            ipbr_phase_scores: IpbrPhaseScores {
                review: Some(1.0),
                ..IpbrPhaseScores::default()
            },
            score_source: ScoreSource::Ipbr,
            ipbr_row_matched: true,
            ipbr_match_key: Some("review-model".to_string()),
            route_underlying_vendor: None,
            route_provider: None,
            candidates: Vec::new(),
            selected_candidate: None,
            quota_percent: Some(100),
            quota_resets_at: None,
            display_order: 0,
            fallback_from: None,
        }
    }

    fn builder_with_running_task(task_id: u32) -> BuilderState {
        let mut builder = BuilderState::default();
        builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "coder".to_string(),
            task_id: Some(task_id),
            round: Some(1),
            status: PipelineItemStatus::Running,
            title: Some(format!("Task {task_id}")),
            mode: None,
            trigger: None,
            interactive: None,
            iteration: 1,
        });
        builder
            .task_titles
            .insert(task_id, format!("Task {task_id}"));
        builder
    }

    fn write_round_artifacts(session_dir: &std::path::Path, round: u32, task_id: u32) {
        std::fs::create_dir_all(session_dir.join("prompts")).unwrap();
        let round_dir = session_dir.join("rounds").join(format!("{round:03}"));
        std::fs::create_dir_all(&round_dir).unwrap();
        std::fs::write(
            round_dir.join("task.toml"),
            format!(
                "id = {task_id}\ntitle = \"x\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n"
            ),
        )
        .unwrap();
        std::fs::write(
            round_dir.join("review_scope.toml"),
            "base_sha = \"deadbeef\"\n",
        )
        .unwrap();
        let artifacts_dir = session_dir.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        std::fs::write(
            artifacts_dir.join("tasks.toml"),
            format!(
                "[[tasks]]\nid = {task_id}\ntitle = \"x\"\ndescription = \"d\"\ntest = \"t\"\nestimated_tokens = 1000\n"
            ),
        )
        .unwrap();
    }

    fn synthetic_review_with_audit() -> String {
        // Mirrors what a full-alignment reviewer would write: outer artifact
        // shape unchanged, the new section lives inside the `summary` block.
        r##"status = "approved"
summary = """Aggregate delta is acceptable.

## AC Coverage Audit
- AC-1: covered — landed in round 1
Path-Boundary drift: (none)
Forgotten items in Dependencies and Sequence: (none)
"""
feedback = []
"##
        .to_string()
    }

    fn synthetic_review_plain() -> String {
        r##"status = "approved"
summary = "Plain review."
feedback = []
"##
        .to_string()
    }

    #[test]
    fn review_round_5_with_default_config_uses_full_alignment_prompt() {
        with_temp_root(|| {
            // Default `RunnerConfig` carries `full_review_interval = 5`, so
            // `ReviewRound(5)` MUST select the full-alignment template. The
            // mocked launch writes a synthetic `review.toml` containing the
            // new `## AC Coverage Audit` section so we can also verify the
            // outer artifact stays parseable end-to-end.
            let session_id = "review-full-alignment-r5".to_string();
            let session_dir = session_state::session_dir(&session_id);
            std::fs::create_dir_all(&session_dir).unwrap();

            let mut state = SessionState::new(session_id);
            state.current_phase = Phase::ReviewRound(5);
            state.builder = builder_with_running_task(1);
            write_round_artifacts(&session_dir, 5, 1);

            let mut app = mk_app(state);
            assert_eq!(app.runner_config, RunnerConfig::default());
            app.models.push(cached_review_model());
            app.test_launch_harness = Some(Arc::new(Mutex::new(TestLaunchHarness {
                outcomes: VecDeque::from([TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: Some(synthetic_review_with_audit()),
                    launch_error: None,
                }]),
            })));

            assert!(app.launch_reviewer_with_model(None));

            let prompt = std::fs::read_to_string(session_dir.join("prompts/reviewer-r5.md"))
                .expect("reviewer prompt written");
            assert!(
                prompt.contains("## AC Coverage Audit"),
                "ReviewRound(5) prompt must instruct full-alignment audit",
            );
            assert!(
                prompt.contains("FULL-ALIGNMENT"),
                "prompt body must announce the full-alignment scope",
            );

            let review = std::fs::read_to_string(session_dir.join("rounds/005/review.toml"))
                .expect("synthetic review artifact written");
            assert!(
                review.contains("## AC Coverage Audit"),
                "artifact MUST carry the new audit section",
            );
        });
    }

    #[test]
    fn off_cadence_review_rounds_use_the_regular_prompt() {
        with_temp_root(|| {
            // `ReviewRound(3)` with the default cadence (5) must keep using
            // the regular reviewer template. Catches a future change that
            // accidentally inverts the modulo or drops the `round > 0` guard.
            let session_id = "review-regular-r3".to_string();
            let session_dir = session_state::session_dir(&session_id);
            std::fs::create_dir_all(&session_dir).unwrap();

            let mut state = SessionState::new(session_id);
            state.current_phase = Phase::ReviewRound(3);
            state.builder = builder_with_running_task(1);
            write_round_artifacts(&session_dir, 3, 1);

            let mut app = mk_app(state);
            app.models.push(cached_review_model());
            app.test_launch_harness = Some(Arc::new(Mutex::new(TestLaunchHarness {
                outcomes: VecDeque::from([TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: Some(synthetic_review_plain()),
                    launch_error: None,
                }]),
            })));

            assert!(app.launch_reviewer_with_model(None));

            let prompt = std::fs::read_to_string(session_dir.join("prompts/reviewer-r3.md"))
                .expect("reviewer prompt written");
            assert!(
                !prompt.contains("## AC Coverage Audit"),
                "off-cadence rounds must keep the regular reviewer template",
            );
            assert!(
                !prompt.contains("FULL-ALIGNMENT"),
                "regular reviewer prompt must not announce full-alignment scope",
            );
        });
    }

    #[test]
    fn full_review_interval_zero_disables_full_alignment_even_on_multiples() {
        with_temp_root(|| {
            // Operator contract: `full_review_interval = 0` disables the
            // feature entirely — every round, including would-be multiples,
            // falls through to the regular reviewer.
            let session_id = "review-disabled".to_string();
            let session_dir = session_state::session_dir(&session_id);
            std::fs::create_dir_all(&session_dir).unwrap();

            let mut state = SessionState::new(session_id);
            state.current_phase = Phase::ReviewRound(10);
            state.builder = builder_with_running_task(1);
            write_round_artifacts(&session_dir, 10, 1);

            let mut app = mk_app(state);
            app.runner_config.full_review_interval = 0;
            app.models.push(cached_review_model());
            app.test_launch_harness = Some(Arc::new(Mutex::new(TestLaunchHarness {
                outcomes: VecDeque::from([TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: Some(synthetic_review_plain()),
                    launch_error: None,
                }]),
            })));

            assert!(app.launch_reviewer_with_model(None));

            let prompt = std::fs::read_to_string(session_dir.join("prompts/reviewer-r10.md"))
                .expect("reviewer prompt written");
            assert!(
                !prompt.contains("## AC Coverage Audit"),
                "interval=0 must disable the full-alignment template",
            );
        });
    }
}
