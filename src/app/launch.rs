// launch.rs
use super::*;
use crate::{
    adapters::{AgentRun, EffortLevel, adapter_for_vendor, window_name_with_model},
    artifacts::ArtifactKind,
    runner::{launch_interactive, launch_noninteractive},
    selection::{
        CachedModel, VendorKind,
        config::SelectionPhase,
        selection::{pick_for_phase_with_effort, select_for_review_with_effort},
    },
    state::{
        self as session_state, LaunchModes, Message, MessageKind, MessageSender, Phase,
        PipelineItemStatus, RunStatus, SessionState,
    },
};
use anyhow::Result;

use super::{models::vendor_tag, prompts::*};

impl App {
    pub(super) fn try_test_launch(
        &mut self,
        status_path: &std::path::Path,
        artifact_path: Option<&std::path::Path>,
        run_key: &str,
        artifacts_dir: &std::path::Path,
    ) -> Option<Result<()>> {
        #[cfg(not(test))]
        {
            let _ = (status_path, artifact_path, run_key, artifacts_dir);
            None
        }
        #[cfg(test)]
        {
            let harness = self.test_launch_harness.as_ref()?.clone();
            let outcome = harness
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .outcomes
                .pop_front()
                .expect("expected queued test launch outcome");
            Some((|| -> Result<()> {
                if let Some(error) = outcome.launch_error {
                    anyhow::bail!(error);
                }
                if let Some(parent) = status_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(status_path, outcome.exit_code.to_string())?;
                if let (Some(path), Some(contents)) = (artifact_path, outcome.artifact_contents) {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(path, contents)?;
                }
                // Write a synthetic finish stamp so test-path behavior mirrors
                // the real runner-owned wrapper.
                let stamp_path = artifacts_dir
                    .join("run-finish")
                    .join(format!("{run_key}.toml"));
                let stamp = crate::runner::FinishStamp {
                    finished_at: chrono::Utc::now().to_rfc3339(),
                    exit_code: outcome.exit_code,
                    head_before: "test-base".to_string(),
                    head_after: "test-after".to_string(),
                    head_state: "stable".to_string(),
                    signal_received: String::new(),
                    working_tree_clean: true,
                };
                let _ = crate::runner::write_finish_stamp(&stamp_path, &stamp);
                Ok(())
            })())
        }
    }

    pub(super) fn choose_primary_model(
        &mut self,
        override_model: Option<&CachedModel>,
        phase: SelectionPhase,
        effort: EffortLevel,
        cheap: bool,
    ) -> Option<(String, VendorKind, String)> {
        if let Some(model) = override_model {
            return Some((
                model.name.clone(),
                model.vendor,
                vendor_tag(model.vendor).to_string(),
            ));
        }

        let outcome =
            pick_for_phase_with_effort(&self.models, phase, None, &self.versions, effort, cheap)?;
        let picked = (
            outcome.model.name.clone(),
            outcome.model.vendor,
            vendor_tag(outcome.model.vendor).to_string(),
        );
        self.emit_selection_warning(outcome.warning);
        Some(picked)
    }

    pub(super) fn choose_review_model(
        &mut self,
        override_model: Option<&CachedModel>,
        used_vendors: &[VendorKind],
        used_models: &[(VendorKind, String)],
        effort: EffortLevel,
        cheap: bool,
    ) -> Option<(String, VendorKind, String)> {
        if let Some(model) = override_model {
            return Some((
                model.name.clone(),
                model.vendor,
                vendor_tag(model.vendor).to_string(),
            ));
        }

        let outcome = select_for_review_with_effort(
            &self.models,
            used_vendors,
            used_models,
            &self.versions,
            effort,
            cheap,
        )?;
        let picked = (
            outcome.model.name.clone(),
            outcome.model.vendor,
            vendor_tag(outcome.model.vendor).to_string(),
        );
        self.emit_selection_warning(outcome.warning);
        Some(picked)
    }

    // This launch bookkeeping intentionally keeps the selected model metadata
    // explicit at the call site so run records cannot silently omit a field.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_run_tracking(
        &mut self,
        stage: &str,
        task_id: Option<u32>,
        round: u32,
        model: String,
        vendor: String,
        window_name: String,
        effort: EffortLevel,
        modes: LaunchModes,
    ) {
        let attempt = self.attempt_for(stage, task_id, round);
        let run_id = session_state::transitions::start_agent_run(
            &mut self.state,
            stage.to_string(),
            task_id,
            round,
            attempt,
            model,
            vendor,
            window_name,
            effort,
            modes,
        );
        let Some(run) = self
            .state
            .agent_runs
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
        else {
            return;
        };
        self.prime_yolo_exit_tracking(&run);
        let effort_suffix = crate::adapters::effort_suffix_from_str(&run.vendor, run.effort);
        let started = Message {
            ts: chrono::Utc::now(),
            run_id,
            kind: MessageKind::Started,
            sender: MessageSender::System,
            text: format!(
                "agent started · {}{} ({})",
                run.model, effort_suffix, run.vendor
            ),
        };
        if let Err(err) = self.state.append_message(&started) {
            let _ = self.state.log_event(format!(
                "failed to append started message for run {run_id}: {err}"
            ));
        } else {
            self.messages.push(started);
        }
        self.current_run_id = Some(run_id);
        self.window_launched = true;
        self.live_summary_path =
            Some(self.live_summary_path_for_run(stage, task_id, round, attempt));
        self.live_summary_cached_text.clear();
        self.live_summary_cached_mtime = None;
        let _ = self.setup_watcher();
        if let Err(err) = self.state.save() {
            let _ = self
                .state
                .log_event(format!("failed to save session after launch: {err}"));
        }
        self.read_live_summary_pipeline();
        self.messages = SessionState::load_messages(&self.state.session_id).unwrap_or_default();
        self.rebuild_tree_view(None);
        // A fresh run launch (including a retry creating a newer attempt) is
        // the other automatic re-enable point: turn progress follow back on
        // even if the operator had previously navigated manually, then refocus
        // onto the new run's deepest visible row.
        self.enable_progress_follow_and_refocus();
    }

    pub(super) fn launch_recovery_plan_review(&mut self) {
        let _ = self.launch_recovery_plan_review_with_model(None);
    }

    pub(super) fn launch_recovery_plan_review_with_model(
        &mut self,
        override_model: Option<CachedModel>,
    ) -> bool {
        use anyhow::Context;

        self.clear_agent_error();
        if self.models.is_empty() {
            self.record_agent_error(
                "model list not yet loaded — wait a moment and try again".to_string(),
            );
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }
        let Phase::BuilderRecoveryPlanReview(round) = self.state.current_phase else {
            return false;
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let plan_review_path = artifacts.join("plan_review.toml");
        let _ = std::fs::remove_file(&plan_review_path);

        let recovery_path = session_dir
            .join("rounds")
            .join(format!("{round:03}"))
            .join("recovery.toml");
        let triggering_review_path = session_dir
            .join("rounds")
            .join(format!("{round:03}"))
            .join("review.toml");
        let attempt = self.attempt_for("plan-review", None, round);
        let live_summary_path = self.live_summary_path_for_run("plan-review", None, round, attempt);
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("recovery-plan-review-r{round}.md"));

        let modes = self.state.launch_modes();
        let phase = Self::phase_for_stage("plan-review");
        let effort = modes.effort_for(EffortLevel::Normal, phase);
        let Some(chosen) =
            self.choose_primary_model(override_model.as_ref(), phase, effort, modes.cheap)
        else {
            self.record_agent_error("no model available with quota".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let prompt = recovery_plan_review_prompt(
            &spec_path,
            &plan_path,
            &triggering_review_path,
            &recovery_path,
            &live_summary_path,
            &plan_review_path,
        );
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt)
            .with_context(|| format!("cannot write {}", prompt_path.display()))
        {
            self.record_agent_error(err.to_string());
            return false;
        }

        session_state::transitions::mark_latest_pipeline_stage_running(
            &mut self.state,
            "plan-review",
        );

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
            effort,
            modes,
        };
        let status_path = self.run_status_path_for("plan-review", None, round, attempt);
        let dirty = self.capture_run_guard(
            "plan-review",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let window_name =
            window_name_with_model("[Recovery Plan Review]", &model, vendor_kind, effort);
        let run_key = Self::run_key_for("plan-review", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) = self.try_test_launch(
            &status_path,
            Some(&plan_review_path),
            &run_key,
            &artifacts_dir,
        ) {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    "plan-review",
                    None,
                    round,
                    model,
                    vendor,
                    window_name,
                    effort,
                    modes,
                );
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(err) => {
                self.record_agent_error(format!("failed to launch recovery plan review: {err}"));
                false
            }
        }
    }

    /// Launch the non-interactive recovery-mode sharding agent.
    pub(super) fn launch_recovery_sharding(&mut self) {
        let _ = self.launch_recovery_sharding_with_model(None);
    }

    pub(super) fn launch_recovery_sharding_with_model(
        &mut self,
        override_model: Option<CachedModel>,
    ) -> bool {
        use anyhow::Context;

        self.clear_agent_error();
        if self.models.is_empty() {
            self.record_agent_error(
                "model list not yet loaded — wait a moment and try again".to_string(),
            );
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }
        let Phase::BuilderRecoverySharding(round) = self.state.current_phase else {
            return false;
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let _ = std::fs::remove_file(&tasks_path);
        let attempt = self.attempt_for("sharding", None, round);
        let live_summary_path = self.live_summary_path_for_run("sharding", None, round, attempt);
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("recovery-sharding-r{round}.md"));

        let modes = self.state.launch_modes();
        let phase = Self::phase_for_stage("sharding");
        let effort = modes.effort_for(EffortLevel::Normal, phase);
        let Some(chosen) =
            self.choose_primary_model(override_model.as_ref(), phase, effort, modes.cheap)
        else {
            self.record_agent_error("no model available with quota".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let completed = self.state.builder.done_task_ids();
        let id_floor = self.state.builder.max_task_id();
        let prompt = recovery_sharding_prompt(
            &spec_path,
            &plan_path,
            &live_summary_path,
            &tasks_path,
            &completed,
            id_floor,
        );
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt)
            .with_context(|| format!("cannot write {}", prompt_path.display()))
        {
            self.record_agent_error(err.to_string());
            return false;
        }

        session_state::transitions::mark_latest_pipeline_stage_running(&mut self.state, "sharding");

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
            effort,
            modes,
        };
        let status_path = self.run_status_path_for("sharding", None, round, attempt);
        let dirty = self.capture_run_guard(
            "sharding",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let window_name =
            window_name_with_model("[Recovery Sharding]", &model, vendor_kind, effort);
        let run_key = Self::run_key_for("sharding", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&tasks_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    "sharding",
                    None,
                    round,
                    model,
                    vendor,
                    window_name,
                    effort,
                    modes,
                );
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(err) => {
                self.record_agent_error(format!("failed to launch recovery sharding: {err}"));
                false
            }
        }
    }

    pub(super) fn launch_brainstorm(&mut self, idea: String) {
        let _ = self.launch_brainstorm_with_model(idea, None);
    }

    pub(super) fn launch_brainstorm_with_model(
        &mut self,
        idea: String,
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

        let modes = self.state.launch_modes();
        let phase = Self::phase_for_stage("brainstorm");
        let effort = modes.effort_for(EffortLevel::Normal, phase);
        let Some(chosen) =
            self.choose_primary_model(override_model.as_ref(), phase, effort, modes.cheap)
        else {
            self.record_agent_error(
                "no model available with quota — check model strip".to_string(),
            );
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let session_id = &self.state.session_id;
        let prompt_path = session_state::session_dir(session_id)
            .join("prompts")
            .join("brainstorm.md");
        let spec_path = session_state::session_dir(session_id)
            .join("artifacts")
            .join("spec.md");

        let _ = std::fs::remove_file(&spec_path);
        let _ = std::fs::remove_file(
            session_state::session_dir(session_id)
                .join("artifacts")
                .join(ArtifactKind::SkipToImpl.filename()),
        );
        let _ = std::fs::remove_file(
            session_state::session_dir(session_id)
                .join("artifacts")
                .join(ArtifactKind::SessionSummary.filename()),
        );

        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let attempt = self.attempt_for("brainstorm", None, 1);
        let live_summary_path = self.live_summary_path_for_run("brainstorm", None, 1, attempt);
        let summary_path = session_state::session_dir(session_id)
            .join("artifacts")
            .join(ArtifactKind::SessionSummary.filename());
        let prompt = brainstorm_prompt(
            &idea,
            &spec_path.display().to_string(),
            &summary_path.display().to_string(),
            &live_summary_path.display().to_string(),
            modes.yolo,
        );
        if let Err(e) = std::fs::write(&prompt_path, &prompt) {
            self.record_agent_error(format!("error writing prompt: {e}"));
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path: prompt_path.clone(),
            effort,
            modes,
        };

        let status_path = self.run_status_path_for("brainstorm", None, 1, attempt);
        let guard_mode = if modes.yolo {
            guard::GuardMode::AutoReset
        } else {
            guard::GuardMode::AskOperator
        };
        let dirty = self.capture_run_guard("brainstorm", None, 1, attempt, guard_mode);
        let adapter = adapter_for_vendor(vendor_kind);
        let window_name = window_name_with_model("[Brainstorm]", &model, vendor_kind, effort);
        let run_key = Self::run_key_for("brainstorm", None, 1, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&spec_path), &run_key, &artifacts_dir)
        {
            result
        } else if modes.yolo {
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        } else {
            launch_interactive(
                &window_name,
                &run,
                adapter.as_ref(),
                true,
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                session_state::transitions::record_brainstorm_launch(
                    &mut self.state,
                    idea.clone(),
                    model.clone(),
                );
                let _ = self.transition_to_phase(Phase::BrainstormRunning);
                self.start_run_tracking(
                    "brainstorm",
                    None,
                    1,
                    model,
                    vendor,
                    window_name,
                    effort,
                    modes,
                );
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(e) => {
                self.record_agent_error(format!("failed to launch brainstorm: {e}"));
                false
            }
        }
    }

    #[cfg(test)]
    pub(super) fn select_brainstorm_model<'a>(
        models: &'a [CachedModel],
        versions: &VersionIndex,
    ) -> Option<&'a CachedModel> {
        crate::selection::selection::pick_for_phase(models, SelectionPhase::Idea, None, versions)
    }

    pub(super) fn launch_retry_for_stage(
        &mut self,
        failed_run: &crate::state::RunRecord,
        chosen: CachedModel,
    ) -> bool {
        match failed_run.stage.as_str() {
            "brainstorm" => {
                let Some(idea) = self.state.idea_text.clone() else {
                    return false;
                };
                self.launch_brainstorm_with_model(idea, Some(chosen))
            }
            "spec-review" => self.launch_spec_review_with_model(Some(chosen)),
            "planning" => self.launch_planning_with_model(Some(chosen), true),
            "plan-review" => match self.state.current_phase {
                Phase::BuilderRecoveryPlanReview(_) => {
                    self.launch_recovery_plan_review_with_model(Some(chosen))
                }
                _ => self.launch_plan_review_with_model(Some(chosen)),
            },
            "sharding" => match self.state.current_phase {
                Phase::BuilderRecoverySharding(_) => {
                    self.launch_recovery_sharding_with_model(Some(chosen))
                }
                _ => self.launch_sharding_with_model(Some(chosen)),
            },
            "recovery" => self.launch_recovery_with_model(Some(chosen)),
            "coder" => self.launch_coder_with_model(Some(chosen)),
            "reviewer" => self.launch_reviewer_with_model(Some(chosen)),
            _ => false,
        }
    }

    pub(super) fn launch_spec_review(&mut self) {
        let _ = self.launch_spec_review_with_model(None);
    }

    pub(super) fn launch_spec_review_with_model(
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
            Phase::SpecReviewPaused => self.completed_rounds("spec-review") + 1,
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
        let phase = Self::phase_for_stage("spec-review");
        let effort = modes.effort_for(EffortLevel::Normal, phase);
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
            let _ = self.state.save();
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let attempt = self.attempt_for("spec-review", None, round);
        let live_summary_path = self.live_summary_path_for_run("spec-review", None, round, attempt);
        let prompt = spec_review_prompt(
            &spec_path.display().to_string(),
            &review_path.display().to_string(),
            &live_summary_path.display().to_string(),
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
            prompt_path,
            effort,
            modes,
        };
        let window_name = window_name_with_model(
            &format!("[Spec Review {round}]"),
            &model,
            vendor_kind,
            effort,
        );
        let status_path = self.run_status_path_for("spec-review", None, round, attempt);
        let dirty = self.capture_run_guard(
            "spec-review",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let run_key = Self::run_key_for("spec-review", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&review_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    "spec-review",
                    None,
                    round,
                    model,
                    vendor,
                    window_name,
                    effort,
                    modes,
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

    pub(super) fn launch_planning(&mut self) {
        let _ = self.launch_planning_with_model(None, true);
    }

    pub(super) fn launch_planning_with_model(
        &mut self,
        override_model: Option<CachedModel>,
        interactive: bool,
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

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");

        let review_paths: Vec<std::path::PathBuf> = self
            .state
            .agent_runs
            .iter()
            .filter(|run| run.stage == "spec-review" && run.status == RunStatus::Done)
            .map(|run| {
                session_dir
                    .join("artifacts")
                    .join(format!("spec-review-{}.md", run.round))
            })
            .filter(|path| path.exists())
            .collect();

        let modes = self.state.launch_modes();
        let phase = Self::phase_for_stage("planning");
        let effort = modes.effort_for(EffortLevel::Normal, phase);
        let Some(chosen) =
            self.choose_primary_model(override_model.as_ref(), phase, effort, modes.cheap)
        else {
            self.record_agent_error("no model available with quota".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let _ = std::fs::remove_file(&plan_path);

        let prompt_path = session_dir.join("prompts").join("planning.md");
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let attempt = self.attempt_for("planning", None, 1);
        let live_summary_path = self.live_summary_path_for_run("planning", None, 1, attempt);
        let prompt = planning_prompt(
            &spec_path,
            &review_paths,
            &plan_path,
            &live_summary_path,
            modes.yolo,
        );
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

        // YOLO planning must take the existing non-interactive runner path.
        let interactive = interactive && !modes.yolo;
        let adapter = adapter_for_vendor(vendor_kind);
        let status_path = self.run_status_path_for("planning", None, 1, attempt);
        let guard_mode = if interactive {
            guard::GuardMode::AskOperator
        } else {
            guard::GuardMode::AutoReset
        };
        let dirty = self.capture_run_guard("planning", None, 1, attempt, guard_mode);
        let window_name = window_name_with_model("[Planning]", &model, vendor_kind, effort);
        let run_key = Self::run_key_for("planning", None, 1, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&plan_path), &run_key, &artifacts_dir)
        {
            result
        } else if interactive {
            launch_interactive(
                &window_name,
                &run,
                adapter.as_ref(),
                true,
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        } else {
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    "planning",
                    None,
                    1,
                    model,
                    vendor,
                    window_name,
                    effort,
                    modes,
                );
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(e) => {
                self.surface_boundary_error(format!("failed to launch planning: {e}"), true);
                false
            }
        }
    }

    pub(super) fn launch_plan_review(&mut self) {
        let _ = self.launch_plan_review_with_model(None);
    }

    pub(super) fn launch_plan_review_with_model(
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
        let (model, vendor_kind, vendor) = chosen;

        let attempt = self.attempt_for("plan-review", None, round);
        let live_summary_path = self.live_summary_path_for_run("plan-review", None, round, attempt);
        let prompt = plan_review_prompt(
            &spec_path.display().to_string(),
            &plan_path.display().to_string(),
            &review_path.display().to_string(),
            round,
            &live_summary_path.display().to_string(),
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
            prompt_path,
            effort,
            modes,
        };
        let window_name = window_name_with_model(
            &format!("[Plan Review {round}]"),
            &model,
            vendor_kind,
            effort,
        );
        let status_path = self.run_status_path_for("plan-review", None, round, attempt);
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
            self.try_test_launch(&status_path, Some(&review_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    "plan-review",
                    None,
                    round,
                    model,
                    vendor,
                    window_name,
                    effort,
                    modes,
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

    pub(super) fn launch_sharding(&mut self) {
        let _ = self.launch_sharding_with_model(None);
    }

    pub(super) fn launch_sharding_with_model(
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

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let spec_path = session_dir.join("artifacts").join("spec.md");
        let plan_path = session_dir.join("artifacts").join("plan.md");
        let tasks_path = session_dir.join("artifacts").join("tasks.toml");

        let modes = self.state.launch_modes();
        let phase = Self::phase_for_stage("sharding");
        let effort = modes.effort_for(EffortLevel::Normal, phase);
        let Some(chosen) =
            self.choose_primary_model(override_model.as_ref(), phase, effort, modes.cheap)
        else {
            self.record_agent_error("no model available with quota".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let _ = std::fs::remove_file(&tasks_path);

        let prompt_path = session_dir.join("prompts").join("sharding.md");
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let attempt = self.attempt_for("sharding", None, 1);
        let live_summary_path = self.live_summary_path_for_run("sharding", None, 1, attempt);
        let prompt = sharding_prompt(&spec_path, &plan_path, &tasks_path, &live_summary_path);
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

        let status_path = self.run_status_path_for("sharding", None, 1, attempt);
        let dirty =
            self.capture_run_guard("sharding", None, 1, attempt, guard::GuardMode::AutoReset);
        let window_name = window_name_with_model("[Sharding]", &model, vendor_kind, effort);
        let run_key = Self::run_key_for("sharding", None, 1, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&tasks_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    "sharding",
                    None,
                    1,
                    model,
                    vendor,
                    window_name,
                    effort,
                    modes,
                );
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(e) => {
                self.surface_boundary_error(format!("failed to launch sharding: {e}"), true);
                false
            }
        }
    }

    pub(super) fn launch_recovery(&mut self) {
        let _ = self.launch_recovery_with_model(None);
    }

    pub(super) fn launch_recovery_with_model(
        &mut self,
        override_model: Option<CachedModel>,
    ) -> bool {
        use anyhow::Context;

        self.clear_agent_error();
        if self.models.is_empty() {
            self.record_agent_error(
                "model list not yet loaded — wait a moment and try again".to_string(),
            );
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }
        let Phase::BuilderRecovery(round) = self.state.current_phase else {
            return false;
        };
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join("spec.md");
        let plan_path = artifacts.join("plan.md");
        let tasks_path = artifacts.join("tasks.toml");
        let recovery_path = session_dir
            .join("rounds")
            .join(format!("{round:03}"))
            .join("recovery.toml");
        let _ = std::fs::remove_file(&recovery_path);
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("recovery-r{round}.md"));

        let modes = self.state.launch_modes();
        let phase = Self::phase_for_stage("recovery");
        let effort = modes.effort_for(EffortLevel::Normal, phase);
        let Some(chosen) =
            self.choose_primary_model(override_model.as_ref(), phase, effort, modes.cheap)
        else {
            self.record_agent_error("no model available with quota".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let is_human_blocked = self
            .state
            .builder
            .pipeline_items_by_stage("recovery")
            .iter()
            .find(|i| i.status == PipelineItemStatus::Running)
            .and_then(|i| i.trigger.as_deref())
            == Some("human_blocked");

        let completed = self.state.builder.done_task_ids();
        let mut started = self
            .started_builder_task_ids()
            .into_iter()
            .collect::<Vec<_>>();
        started.sort_unstable();
        let attempt = self.attempt_for("recovery", None, round);
        let live_summary_path = self.live_summary_path_for_run("recovery", None, round, attempt);
        let prompt = recovery_prompt(
            &spec_path,
            &plan_path,
            &tasks_path,
            self.state.builder.recovery_trigger_task_id,
            self.state.builder.recovery_trigger_summary.as_deref(),
            &completed,
            &started,
            &live_summary_path,
            &recovery_path,
            is_human_blocked,
        );
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt)
            .with_context(|| format!("cannot write {}", prompt_path.display()))
        {
            self.record_agent_error(err.to_string());
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            prompt_path,
            effort,
            modes,
        };
        let status_path = self.run_status_path_for("recovery", None, round, attempt);
        let recovery_guard_mode = if is_human_blocked {
            guard::GuardMode::AskOperator
        } else {
            guard::GuardMode::AutoReset
        };
        let dirty = self.capture_run_guard("recovery", None, round, attempt, recovery_guard_mode);
        let window_name = window_name_with_model("[Recovery]", &model, vendor_kind, effort);
        let run_key = Self::run_key_for("recovery", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, Some(&tasks_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            if is_human_blocked {
                launch_interactive(
                    &window_name,
                    &run,
                    adapter.as_ref(),
                    true,
                    &status_path,
                    &run_key,
                    &artifacts_dir,
                )
            } else {
                launch_noninteractive(
                    &window_name,
                    &run,
                    adapter.as_ref(),
                    &status_path,
                    &run_key,
                    &artifacts_dir,
                )
            }
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    "recovery",
                    None,
                    round,
                    model,
                    vendor,
                    window_name,
                    effort,
                    modes,
                );
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(err) => {
                self.record_agent_error(format!("failed to launch recovery: {err}"));
                false
            }
        }
    }

    pub(super) fn launch_coder(&mut self) {
        let _ = self.launch_coder_with_model(None);
    }

    pub(super) fn launch_coder_with_model(&mut self, override_model: Option<CachedModel>) -> bool {
        self.clear_agent_error();
        if self.models.is_empty() {
            self.record_agent_error(
                "model list not yet loaded — wait a moment and try again".to_string(),
            );
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }
        let Phase::ImplementationRound(r) = self.state.current_phase else {
            return false;
        };

        let Some(task_id) = self.ensure_builder_task_for_round(r) else {
            self.record_agent_error("no pending tasks".to_string());
            let _ = self.state.save();
            return false;
        };

        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let round_dir = session_dir.join("rounds").join(format!("{r:03}"));
        let task_file = round_dir.join("task.toml");
        let dirty_before_coder = guard::git_status_dirty();

        if !task_file.exists() {
            let body = task_toml_for(&session_dir, task_id).unwrap_or_else(|e| {
                format!("# task body could not be loaded: {e}\nid = {task_id}\n")
            });
            let _ = std::fs::write(&task_file, body);
        }

        // Pin the base HEAD before the coder runs; preserves original base on resume.
        self.capture_round_base(&round_dir);

        let modes = self.state.launch_modes();
        self.record_dirty_worktree_yolo_gate(dirty_before_coder, modes);
        let requested_effort = task_effort_for(&session_dir, task_id);
        let phase = Self::phase_for_stage("coder");
        let effort = modes.effort_for(requested_effort, phase);
        // Override-model bypass: an explicit operator pick wins over the
        // tough-eligibility filter (spec §3.7). The adapter still emits the
        // launch-snapshot effort flag derived from `task.tough`.
        let Some(chosen) =
            self.choose_primary_model(override_model.as_ref(), phase, effort, modes.cheap)
        else {
            self.record_agent_error("no model available with quota".to_string());
            let _ = self.state.save();
            return false;
        };
        let (model, vendor_kind, vendor) = chosen;

        let prompt_path = session_dir.join("prompts").join(format!("coder-r{r}.md"));
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let attempt = self.attempt_for("coder", Some(task_id), r);
        let live_summary_path = self.live_summary_path_for_run("coder", Some(task_id), r, attempt);
        let resume = self
            .state
            .agent_runs
            .iter()
            .any(|run| run.stage == "coder" && run.task_id == Some(task_id) && run.round == r);
        // Drain refine carryover only when starting a fresh coder run; on
        // resume we'd have already included it in the original prompt.
        let refine_carryover: Vec<String> = if resume {
            Vec::new()
        } else {
            session_state::transitions::take_pending_refine_feedback(&mut self.state)
        };
        let prompt = coder_prompt(
            &session_dir,
            task_id,
            r,
            &task_file,
            &live_summary_path,
            resume,
            &refine_carryover,
        );
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

        let window_name =
            window_name_with_model(&format!("[Round {r} Coder]"), &model, vendor_kind, effort);
        let status_path = self.run_status_path_for("coder", Some(task_id), r, attempt);
        self.capture_run_guard(
            "coder",
            Some(task_id),
            r,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let run_key = Self::run_key_for("coder", Some(task_id), r, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(&status_path, None, &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    "coder",
                    Some(task_id),
                    r,
                    model,
                    vendor,
                    window_name,
                    effort,
                    modes,
                );
                true
            }
            Err(e) => {
                self.surface_boundary_error(format!("failed to launch coder: {e}"), true);
                false
            }
        }
    }

    pub(super) fn launch_reviewer(&mut self) {
        let _ = self.launch_reviewer_with_model(None);
    }

    pub(super) fn launch_reviewer_with_model(
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

        let window_name = window_name_with_model(
            &format!("[Round {r} Reviewer]"),
            &model,
            vendor_kind,
            effort,
        );
        let status_path = self.run_status_path_for("reviewer", Some(task_id), r, attempt);
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
            self.try_test_launch(&status_path, Some(&review_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            let adapter = adapter_for_vendor(vendor_kind);
            launch_noninteractive(
                &window_name,
                &run,
                adapter.as_ref(),
                &status_path,
                &run_key,
                &artifacts_dir,
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
}
