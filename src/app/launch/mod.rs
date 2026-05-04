// Per-stage launch handlers.
//
// Shared bookkeeping (model picking, run tracking, retry routing) lives in
// this file; the actual per-stage launch bodies live in sibling submodules so
// each pipeline stage's wiring is reviewable in isolation. The orchestrator
// (App) reaches the per-stage helpers as plain methods because the impl
// blocks all extend `crate::app::App`.

mod brainstorm;
mod coder;
mod final_validation;
mod plan_review;
mod planning;
mod recovery;
mod recovery_plan_review;
mod recovery_sharding;
mod reviewer;
mod sharding;
mod simplifier;
mod spec_review;

use super::*;
use crate::{
    adapters::EffortLevel,
    selection::{
        CachedModel, VendorKind,
        config::SelectionPhase,
        selection::{pick_for_phase_with_effort, select_for_review_with_effort},
    },
    state::{
        self as session_state, LaunchModes, Message, MessageKind, MessageSender, Phase,
        SessionState,
    },
};

use super::models::vendor_tag;

impl App {
    pub(super) fn try_test_launch(
        &mut self,
        artifact_path: Option<&std::path::Path>,
        run_key: &str,
        artifacts_dir: &std::path::Path,
    ) -> Option<anyhow::Result<()>> {
        #[cfg(not(test))]
        {
            let _ = (artifact_path, run_key, artifacts_dir);
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
            Some((|| -> anyhow::Result<()> {
                if let Some(error) = outcome.launch_error {
                    anyhow::bail!(error);
                }
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
        mut modes: LaunchModes,
        prompt_path: std::path::PathBuf,
    ) {
        let attempt = self.attempt_for(stage, task_id, round);
        modes.interactive = self.run_is_interactive(stage, round, modes);
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
        if run.modes.interactive {
            self.open_split_target(crate::app::split::SplitTarget::Run(run_id));
        } else {
            // Watchdog only arms for non-interactive runs (spec §3.8 / AC5).
            self.watchdog.register(
                run.id,
                run.effort,
                run.window_name.clone(),
                prompt_path,
                std::time::Instant::now(),
            );
        }
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
        self.input_mode = false;
        self.run_launched = true;
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

    fn run_is_interactive(&self, stage: &str, round: u32, modes: LaunchModes) -> bool {
        match stage {
            "brainstorm" | "planning" => !modes.yolo,
            "recovery" => self
                .state
                .builder
                .pipeline_items
                .iter()
                .rev()
                .find(|item| item.stage == "recovery" && item.round == Some(round))
                .and_then(|item| item.interactive)
                .unwrap_or(false),
            _ => false,
        }
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
            "simplifier" => self.launch_simplifier_with_model(Some(chosen)),
            _ => false,
        }
    }

    fn session_selected_model_for_validator(&mut self) -> Option<(String, VendorKind, String)> {
        let name = self.state.selected_model.as_ref()?.clone();
        let model = self.models.iter().find(|m| m.name == name)?;
        Some((
            model.name.clone(),
            model.vendor,
            vendor_tag(model.vendor).to_string(),
        ))
    }

    /// Look up the most-recent run on a stage for the given round and
    /// resolve its `(model, vendor_kind, vendor_tag)` triple. Returns
    /// `None` when no matching run exists or its persisted vendor string
    /// no longer parses (e.g. after a vendor rename).
    ///
    /// "Most recent" is by run id (monotonic via `next_agent_run_id`),
    /// not by `attempt`: when a round contains multiple tasks, a later
    /// task's attempt=1 is newer than an earlier task's attempt=2, and
    /// the simplifier should follow the model the round most recently
    /// settled on.
    fn round_stage_model(&self, stage: &str, round: u32) -> Option<(String, VendorKind, String)> {
        let last = self
            .state
            .agent_runs
            .iter()
            .filter(|run| run.stage == stage && run.round == round)
            .max_by_key(|run| run.id)?;
        let vendor_kind = crate::selection::vendor::str_to_vendor(&last.vendor)?;
        Some((
            last.model.clone(),
            vendor_kind,
            vendor_tag(vendor_kind).to_string(),
        ))
    }
}
