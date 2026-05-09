use crate::app::{App, Reason};
use crate::state::{
    self as session_state, Message, MessageKind, MessageSender, Phase, PipelineItem,
    PipelineItemStatus,
};
use crate::{review, tasks};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
fn recovery_error_detail(err: &anyhow::Error) -> String {
    format!("{err:#}")
}
impl App {
    pub(crate) fn enter_builder_recovery(
        &mut self,
        triggering_round: u32,
        trigger_task_id: Option<u32>,
        trigger_summary: Option<String>,
        trigger: &str,
    ) -> bool {
        if self.current_run_id.is_some() || self.run_launched {
            let _ = self.state.log_event(
                "enter_builder_recovery called while a run label is still marked active"
                    .to_string(),
            );
        }
        let session_dir = self.session_dir();
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");
        let (prev_task_ids, prev_max) = tasks::validate(&tasks_path)
            .ok()
            .map(|f| {
                let ids = f.tasks.iter().map(|t| t.id).collect::<Vec<_>>();
                let max = ids.iter().copied().max();
                (ids, max)
            })
            .unwrap_or_default();
        let recovery_cycle_count =
            session_state::transitions::increment_recovery_cycle_count(&mut self.state);
        let effective_trigger = if recovery_cycle_count >= 3 && trigger != "human_blocked" {
            let loop_msg = format!(
                "recovery loop: {} consecutive recovery cycles without approval — escalating to human_blocked",
                recovery_cycle_count
            );
            let _ = self.state.log_event(loop_msg.clone());
            let msg = Message {
                ts: chrono::Utc::now(),
                run_id: self.current_run_id.unwrap_or(0),
                kind: MessageKind::SummaryWarn,
                sender: MessageSender::System,
                text: loop_msg,
            };
            if let Err(err) = self.state.append_message(&msg) {
                let _ = self.state.log_event(format!(
                    "failed to append circuit-breaker escalation message: {err}"
                ));
            } else {
                self.messages.push(msg);
            }
            "human_blocked"
        } else {
            trigger
        };
        session_state::transitions::record_builder_recovery_context(
            &mut self.state,
            trigger_task_id,
            prev_max,
            prev_task_ids,
            trigger_summary,
        );
        session_state::transitions::mark_current_task_for_recovery(
            &mut self.state,
            triggering_round,
        );
        let interactive = effective_trigger == "human_blocked";
        session_state::transitions::queue_recovery_stage(
            &mut self.state,
            triggering_round,
            effective_trigger.to_string(),
            interactive,
        );
        self.clear_agent_error();
        if let Err(err) = self.transition_to_phase(Phase::BuilderRecovery(triggering_round)) {
            self.record_agent_error(format!("failed to enter builder recovery: {err}"));
            self.clear_builder_recovery_context();
            let _ = self.transition_to_blocked(crate::state::BlockOrigin::BuilderRecovery);
        }
        true
    }
    pub(crate) fn recovery_outer_iteration(&mut self) -> u32 {
        if let Some(override_iter) = self.state.builder.next_iteration_for_recovery.take() {
            return override_iter;
        }
        if let Some(task_id) = self.state.builder.recovery_trigger_task_id
            && let Some(item) = self
                .state
                .builder
                .pipeline_items
                .iter()
                .find(|item| item.stage == "coder" && item.task_id == Some(task_id))
        {
            return item.iteration;
        }
        self.state
            .builder
            .pipeline_items
            .iter()
            .map(|item| item.iteration)
            .max()
            .unwrap_or(1)
    }
    pub(crate) fn enter_builder_recovery_from_block(&mut self) {
        let trigger_round = self
            .state
            .builder
            .pipeline_items
            .iter()
            .filter_map(|item| item.round)
            .max()
            .or(match self.state.current_phase {
                Phase::ImplementationRound(r)
                | Phase::ReviewRound(r)
                | Phase::Simplification(r)
                | Phase::FinalValidation(r) => Some(r),
                _ => None,
            })
            .unwrap_or(1);
        let trigger_task_id = self.state.builder.current_task_id().or_else(|| {
            self.state
                .builder
                .pipeline_items
                .iter()
                .filter(|i| i.stage == "coder")
                .filter_map(|i| i.task_id)
                .max()
        });
        let next_iter = self
            .state
            .builder
            .pipeline_items
            .iter()
            .map(|i| i.iteration)
            .max()
            .unwrap_or(0)
            + 1;
        self.state.builder.next_iteration_for_recovery = Some(next_iter);
        let summary =
            Some("final validation cap exhausted; operator-initiated recovery".to_string());
        self.enter_builder_recovery(trigger_round, trigger_task_id, summary, "human_blocked");
    }
    pub(crate) fn started_builder_task_ids(&self) -> BTreeSet<u32> {
        self.state
            .agent_runs
            .iter()
            .filter(|run| matches!(run.stage.as_str(), "coder" | "reviewer"))
            .filter_map(|run| run.task_id)
            .collect()
    }
    pub(crate) fn recovery_notes_document_started_supersession(
        text: &str,
        superseded_ids: &BTreeSet<u32>,
    ) -> Result<()> {
        if !text.contains("Recovery Notes") {
            anyhow::bail!("missing required `Recovery Notes` section");
        }
        for id in superseded_ids {
            let needle = id.to_string();
            let found = text.match_indices(&needle).any(|(idx, _)| {
                let prev = idx
                    .checked_sub(1)
                    .and_then(|p| text.as_bytes().get(p).copied())
                    .map(char::from);
                let next = text
                    .as_bytes()
                    .get(idx + needle.len())
                    .copied()
                    .map(char::from);
                !prev.is_some_and(|ch| ch.is_ascii_digit())
                    && !next.is_some_and(|ch| ch.is_ascii_digit())
            });
            if !found {
                anyhow::bail!("`Recovery Notes` missing superseded started task id {id}");
            }
        }
        Ok(())
    }
    pub(crate) fn reconcile_builder_recovery(&mut self, recovery_run_id: u64) -> Result<()> {
        let session_dir = self.session_dir();
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let parsed = tasks::validate(&tasks_path)
            .with_context(|| format!("invalid {}", tasks_path.display()))?;
        let done_ids = self
            .state
            .builder
            .done_task_ids()
            .into_iter()
            .collect::<BTreeSet<_>>();
        let started_ids = self.started_builder_task_ids();
        let prev_task_ids = self
            .state
            .builder
            .recovery_prev_task_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let historical_max = self
            .state
            .builder
            .recovery_prev_max_task_id
            .into_iter()
            .chain(done_ids.iter().copied())
            .chain(started_ids.iter().copied())
            .max()
            .unwrap_or(0);
        let recovered_ids = parsed.tasks.iter().map(|t| t.id).collect::<Vec<_>>();
        let recovered_set = recovered_ids.iter().copied().collect::<BTreeSet<_>>();
        if let Some(collision) = recovered_ids.iter().find(|id| done_ids.contains(id)) {
            anyhow::bail!("recovered unfinished tasks include completed task id {collision}");
        }
        let historical_ids = prev_task_ids
            .iter()
            .copied()
            .chain(done_ids.iter().copied())
            .chain(started_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        if let Some(id) = recovered_ids
            .iter()
            .find(|id| !historical_ids.contains(id) && **id <= historical_max)
        {
            anyhow::bail!(
                "new recovery task id {id} must be greater than prior max id {historical_max}"
            );
        }
        let superseded_started: BTreeSet<_> = started_ids
            .difference(&done_ids)
            .filter(|id| !recovered_set.contains(id))
            .copied()
            .collect();
        if !superseded_started.is_empty() {
            for path in [&spec_path, &plan_path] {
                let text = std::fs::read_to_string(path)
                    .with_context(|| format!("cannot read {}", path.display()))?;
                Self::recovery_notes_document_started_supersession(&text, &superseded_started)
                    .with_context(|| format!("invalid {}", path.display()))?;
            }
        }
        let recovery_iteration = self.recovery_outer_iteration();
        self.replace_pipeline_from_recovery(&parsed, recovery_iteration);
        session_state::transitions::mark_latest_pipeline_stage_done(&mut self.state, "recovery");
        session_state::transitions::set_retry_reset_run_id_cutoff(&mut self.state, recovery_run_id);
        self.clear_builder_recovery_context();
        Ok(())
    }
    pub(crate) fn handle_recovery_plan_review_completed(
        &mut self,
        run: &crate::state::RunRecord,
        round: u32,
    ) -> Result<()> {
        let session_dir = self.session_dir();
        let plan_review_path = session_dir.join("artifacts").join("plan_review.toml");
        session_state::transitions::mark_latest_pipeline_stage_done(&mut self.state, "plan-review");
        let verdict = match review::validate(&plan_review_path) {
            Ok(v) => v,
            Err(err) => {
                let reason =
                    Reason::RecoveryPlanReviewFailed(recovery_error_detail(&err)).to_string();
                self.finalize_run_record(run.id, false, Some(reason.clone()));
                let failed_run = self
                    .state
                    .agent_runs
                    .iter()
                    .find(|r| r.id == run.id)
                    .cloned();
                let run_ref = failed_run.as_ref().unwrap_or(run);
                if !self.maybe_auto_retry(run_ref) {
                    self.record_agent_error(reason);
                }
                return Ok(());
            }
        };
        let summary_text = verdict.summary.trim().to_string();
        if !summary_text.is_empty() {
            let kind = match verdict.status {
                review::ReviewStatus::Approved => MessageKind::Summary,
                _ => MessageKind::SummaryWarn,
            };
            let msg = Message {
                ts: chrono::Utc::now(),
                run_id: run.id,
                kind,
                sender: MessageSender::Agent {
                    model: run.model.clone(),
                    subscription_label: run.subscription_label.clone(),
                },
                text: summary_text,
            };
            if let Err(err) = self.state.append_message(&msg) {
                let _ = self.state.log_event(format!(
                    "failed to append recovery plan review message: {err}"
                ));
            } else {
                self.messages.push(msg);
            }
        }
        self.finalize_run_record(run.id, true, None);
        self.clear_agent_error();
        match verdict.status {
            review::ReviewStatus::Approved | review::ReviewStatus::Refine => {
                session_state::transitions::reset_recovery_cycle_count(&mut self.state);
                self.queue_recovery_sharding_pipeline_item(round);
                self.transition_to_phase(Phase::BuilderRecoverySharding(round))?;
            }
            review::ReviewStatus::Revise
            | review::ReviewStatus::HumanBlocked
            | review::ReviewStatus::AgentPivot => {
                let trigger_str = if verdict.status == review::ReviewStatus::HumanBlocked {
                    "human_blocked"
                } else {
                    "agent_pivot"
                };
                let summary = verdict.feedback.join("\n");
                let trigger_summary = (!summary.trim().is_empty()).then_some(summary);
                self.enter_builder_recovery(round, None, trigger_summary, trigger_str);
            }
        }
        Ok(())
    }
    fn replace_pipeline_from_recovery(
        &mut self,
        parsed: &tasks::TasksFile,
        recovery_iteration: u32,
    ) {
        let done_ids: BTreeSet<_> = self.state.builder.done_task_ids().into_iter().collect();
        let mut next_items: Vec<PipelineItem> = self
            .state
            .builder
            .pipeline_items
            .iter()
            .filter(|item| {
                item.stage == "coder" && item.task_id.is_some_and(|id| done_ids.contains(&id))
            })
            .cloned()
            .collect();
        if next_items.is_empty() {
            for &tid in &done_ids {
                next_items.push(PipelineItem {
                    id: 0,
                    stage: "coder".to_string(),
                    task_id: Some(tid),
                    round: None,
                    status: PipelineItemStatus::Approved,
                    title: self.state.builder.task_titles.get(&tid).cloned(),
                    mode: None,
                    trigger: None,
                    interactive: None,
                    iteration: recovery_iteration,
                });
            }
        }
        let recovered_titles = parsed
            .tasks
            .iter()
            .map(|task| (task.id, task.title.clone()))
            .collect::<Vec<_>>();
        for task in &parsed.tasks {
            if !done_ids.contains(&task.id) {
                next_items.push(PipelineItem {
                    id: 0,
                    stage: "coder".to_string(),
                    task_id: Some(task.id),
                    round: None,
                    status: PipelineItemStatus::Pending,
                    title: Some(task.title.clone()),
                    mode: None,
                    trigger: None,
                    interactive: None,
                    iteration: recovery_iteration,
                });
            }
        }
        session_state::transitions::replace_recovery_pipeline(
            &mut self.state,
            next_items,
            recovered_titles,
        );
    }
    pub(crate) fn handle_recovery_sharding_completed(
        &mut self,
        run: &crate::state::RunRecord,
        round: u32,
    ) -> Result<()> {
        let session_dir = self.session_dir();
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");
        session_state::transitions::mark_latest_pipeline_stage_done(&mut self.state, "sharding");
        let parsed = match tasks::validate(&tasks_path) {
            Ok(p) => p,
            Err(err) => {
                let reason =
                    Reason::RecoveryShardingFailed(recovery_error_detail(&err)).to_string();
                self.finalize_run_record(run.id, false, Some(reason.clone()));
                let failed_run = self
                    .state
                    .agent_runs
                    .iter()
                    .find(|r| r.id == run.id)
                    .cloned();
                let run_ref = failed_run.as_ref().unwrap_or(run);
                if !self.maybe_auto_retry(run_ref) {
                    self.record_agent_error(reason);
                }
                return Ok(());
            }
        };
        let max_seen = self.state.builder.max_task_id();
        if let Some(task) = parsed.tasks.iter().find(|t| t.id <= max_seen) {
            let reason = format!(
                "recovery sharding produced task id {} but new ids must be > {} (max id ever seen)",
                task.id, max_seen
            );
            self.finalize_run_record(run.id, false, Some(reason.clone()));
            self.record_agent_error(reason);
            self.clear_builder_recovery_context();
            let _ = self.transition_to_blocked(crate::state::BlockOrigin::BuilderRecovery);
            return Ok(());
        }
        self.finalize_run_record(run.id, true, None);
        self.clear_agent_error();
        let recovery_iteration = self.recovery_outer_iteration();
        self.replace_pipeline_from_recovery(&parsed, recovery_iteration);
        let pipeline_msg = format!(
            "recovery sharding complete: {} pending tasks",
            self.state.builder.pending_task_ids().len()
        );
        self.append_system_message(run.id, MessageKind::Summary, pipeline_msg);
        self.transition_to_phase(Phase::ImplementationRound(round + 1))?;
        Ok(())
    }
}
#[cfg(test)]
#[path = "builder_recovery_tests.rs"]
mod tests;
