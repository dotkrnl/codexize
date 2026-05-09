// Per-stage runtime modules.
//
// Each pipeline stage owns one file under `src/app_runtime/stages/<name>.rs`
// holding that stage's launch wiring (and, where the prior god files made
// the cross-stage cut natural, finalize/event hooks). The orchestrator
// (`crate::app::App`) reaches the per-stage helpers as plain methods because
// the impl blocks all extend `App` — physical location moved out of
// `src/app/launch/` so a future server-mode binary can drive `app_runtime`
// directly without pulling the legacy `app::launch` namespace.
//
// Shared bookkeeping (model picking, run tracking, retry routing) stays in
// this `mod.rs` so per-stage files remain reviewable in isolation.
mod brainstorm;
mod coder;
mod dispatch;
mod dreaming;
mod final_validation;
mod plan_review;
mod planning;
mod recovery;
mod recovery_plan_review;
mod recovery_sharding;
mod reviewer;
mod sharding;
mod simplification;
mod spec_review;
use crate::app::models::vendor_tag;
use crate::{
    adapters::EffortLevel,
    app::App,
    selection::{
        CachedModel, CliKind, SubscriptionKind,
        config::SelectionPhase,
        selection::{pick_for_phase_with_effort, select_for_review_with_effort},
    },
    state::{
        self as session_state, LaunchModes, Message, MessageKind, MessageSender, SessionState,
    },
};

/// Tuple returned by every model-pick helper so the launch boundary always
/// sees the selected `Candidate`'s CLI and launch_name (not just the row's
/// canonical identity). Free candidates require this so a row named
/// `deepseek-v4-flash` can launch via opencode with `dsk-4-flash`.
pub(crate) type StagePick = (
    String,           // model row name
    SubscriptionKind, // row subscription (compat mirror of selected candidate)
    String,           // vendor tag string
    CliKind,          // CLI to spawn
    String,           // verbatim launch_name passed to the CLI
);

/// Pull `(cli, launch_name)` from the row's selected candidate when arbitration
/// has chosen one; otherwise fall back to the direct-vendor CLI and the row's
/// canonical name. The fallback only fires for rows with no candidates (e.g.
/// override_model paths constructed before assembly seeded candidates), which
/// preserves pre-task-2 behavior.
pub(crate) fn pick_cli_and_launch_name(row: &CachedModel) -> (CliKind, String) {
    if let Some(candidate) = row.selected_candidate() {
        return (candidate.cli, candidate.launch_name.clone());
    }
    let cli = row.vendor.direct_cli().unwrap_or(CliKind::Opencode);
    (cli, row.name.clone())
}
impl App {
    pub(crate) fn try_test_launch(
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
    pub(crate) fn choose_primary_model(
        &mut self,
        override_model: Option<&CachedModel>,
        phase: SelectionPhase,
        effort: EffortLevel,
        cheap: bool,
    ) -> Option<StagePick> {
        if let Some(model) = override_model {
            let (cli, launch_name) = pick_cli_and_launch_name(model);
            return Some((
                model.name.clone(),
                model.vendor,
                vendor_tag(model.vendor).to_string(),
                cli,
                launch_name,
            ));
        }
        let outcome = pick_for_phase_with_effort(&self.models, phase, None, effort, cheap)?;
        let (cli, launch_name) = pick_cli_and_launch_name(outcome.model);
        let picked = (
            outcome.model.name.clone(),
            outcome.model.vendor,
            vendor_tag(outcome.model.vendor).to_string(),
            cli,
            launch_name,
        );
        self.emit_selection_warning(outcome.warning);
        Some(picked)
    }
    pub(crate) fn choose_review_model(
        &mut self,
        override_model: Option<&CachedModel>,
        used_vendors: &[SubscriptionKind],
        used_models: &[(SubscriptionKind, String)],
        effort: EffortLevel,
        cheap: bool,
    ) -> Option<StagePick> {
        if let Some(model) = override_model {
            let (cli, launch_name) = pick_cli_and_launch_name(model);
            return Some((
                model.name.clone(),
                model.vendor,
                vendor_tag(model.vendor).to_string(),
                cli,
                launch_name,
            ));
        }
        let outcome =
            select_for_review_with_effort(&self.models, used_vendors, used_models, effort, cheap)?;
        let (cli, launch_name) = pick_cli_and_launch_name(outcome.model);
        let picked = (
            outcome.model.name.clone(),
            outcome.model.vendor,
            vendor_tag(outcome.model.vendor).to_string(),
            cli,
            launch_name,
        );
        self.emit_selection_warning(outcome.warning);
        Some(picked)
    }
    // This launch bookkeeping intentionally keeps the selected model metadata
    // explicit at the call site so run records cannot silently omit a field.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_run_tracking(
        &mut self,
        run_id: u64,
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
        let stage_span = tracing::debug_span!(
            "stage",
            stage,
            task_id,
            round,
            attempt,
            interactive = modes.interactive
        );
        let _stage_enter = stage_span.enter();
        let run_id = session_state::transitions::start_agent_run_with_id(
            &mut self.state,
            run_id,
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
        tracing::debug!(
            run_id = run.id,
            model = %run.model,
            vendor = %run.vendor,
            window_name = %run.window_name,
            "agent run tracking started"
        );
        if run.modes.interactive {
            self.open_split_target(crate::app::split::SplitTarget::Run(run_id));
        } else {
            // Watchdog only arms for non-interactive runs (spec §3.8 / AC5).
            self.watchdog.register(
                run.id,
                run.effort,
                run.window_name.clone(),
                prompt_path,
                tokio::time::Instant::now(),
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
    fn session_selected_model_for_validator(&mut self) -> Option<StagePick> {
        let name = self.state.selected_model.as_ref()?.clone();
        let model = self.models.iter().find(|m| m.name == name)?;
        let (cli, launch_name) = pick_cli_and_launch_name(model);
        Some((
            model.name.clone(),
            model.vendor,
            vendor_tag(model.vendor).to_string(),
            cli,
            launch_name,
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
    fn round_stage_model(&self, stage: &str, round: u32) -> Option<StagePick> {
        let last = self
            .state
            .agent_runs
            .iter()
            .filter(|run| run.stage == stage && run.round == round)
            .max_by_key(|run| run.id)?;
        let vendor_kind =
            crate::logic::selection::assemble::parse_subscription_str(&last.vendor)?;
        // RunRecord doesn't persist the candidate's cli/launch_name, so when
        // resuming we look the row up in the current universe and reuse its
        // selected candidate (preserves Free-tier launch_name on resume); if
        // the row no longer exists, fall back to direct-CLI defaults.
        let (cli, launch_name) = match self.models.iter().find(|m| m.name == last.model) {
            Some(row) => pick_cli_and_launch_name(row),
            None => (
                vendor_kind.direct_cli().unwrap_or(CliKind::Opencode),
                last.model.clone(),
            ),
        };
        Some((
            last.model.clone(),
            vendor_kind,
            vendor_tag(vendor_kind).to_string(),
            cli,
            launch_name,
        ))
    }
}
