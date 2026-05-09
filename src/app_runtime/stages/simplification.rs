use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::models::vendor_tag;
use crate::app::prompts::{read_review_scope, simplifier_prompt};
use crate::app::{App, guard};
use crate::selection::CachedModel;
use crate::selection::config::SelectionPhase;
use crate::state::{self as session_state, Phase};
use anyhow::Result;
impl App {
    pub(crate) fn launch_simplifier(&mut self) {
        let _ = self.launch_simplifier_with_model(None);
    }
    pub(crate) fn launch_simplifier_with_model(
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
        let Phase::Simplification(round) = self.state.current_phase else {
            return false;
        };
        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let round_dir = session_dir.join("rounds").join(format!("{round:03}"));
        // Diff scope is anchored at the *session* base (rounds/001's
        // review_scope.toml — captured at the first round entry and never
        // overwritten), not the current round's base. The simplifier needs
        // to see every commit produced this session so cross-task cleanups
        // remain in scope; using `rounds/{round}/review_scope.toml` would
        // restrict it to the last task's diff only and miss the
        // accumulating refactor opportunities the user expects it to find.
        let review_scope_file = session_dir
            .join("rounds")
            .join("001")
            .join("review_scope.toml");
        let simplification_path = round_dir.join("simplification.toml");
        // Force a fresh verdict each entry so a stale TOML can't be mistaken
        // for this run's output during finalization. Mirrors final-validation.
        let _ = std::fs::remove_file(&simplification_path);
        if let Err(err) = read_review_scope(&review_scope_file) {
            self.record_agent_error(format!("invalid review scope: {err:#}"));
            let _ = self.state.save();
            return false;
        }
        let modes = self.state.launch_modes();
        // SelectionPhase::Build matches the coder's effort dial so
        // the simplifier inherits the same "normal vs. tough" wiring.
        let effort = modes.effort_for(EffortLevel::Normal, SelectionPhase::Build);
        // Model selection precedence (spec §2.3, Q5/b):
        //   1. explicit operator override (retry from picker);
        //   2. an already-selected simplifier run for this round (retry
        //      reuses the same model so cap-bounded attempts stay coherent);
        //   3. the most recent coder run for the round (the cleanup honors
        //      the conventions just written);
        //   4. fallback to the standard primary picker so we never refuse
        //      to start when run metadata is missing on session resume.
        let chosen = override_model
            .as_ref()
            .map(|model| {
                (
                    model.name.clone(),
                    model.vendor,
                    vendor_tag(model.vendor).to_string(),
                    model.route_provider.clone(),
                )
            })
            .or_else(|| self.round_stage_model("simplifier", round))
            .or_else(|| self.round_stage_model("coder", round))
            .or_else(|| {
                self.choose_primary_model(None, SelectionPhase::Build, effort, modes.cheap)
            });
        let Some((model, vendor_kind, vendor, route_provider)) = chosen else {
            self.record_agent_error("no model available for simplifier".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };
        let attempt = self.attempt_for("simplifier", None, round);
        let live_summary_path = self.live_summary_path_for_run("simplifier", None, round, attempt);
        // Drain refine feedback the reviewer stashed when approving the final
        // task with a Refine verdict. Without this, that feedback is lost — no
        // later coder runs to consume it, and the validator stays unaware.
        let resume = self
            .state
            .agent_runs
            .iter()
            .any(|run| run.stage == "simplifier" && run.round == round);
        let refine_carryover: Vec<String> = if resume {
            Vec::new()
        } else {
            session_state::transitions::take_pending_refine_feedback(&mut self.state)
        };
        let prompt = simplifier_prompt(
            &session_dir,
            &review_scope_file,
            &simplification_path,
            &live_summary_path,
            &refine_carryover,
            self.prompt_meta(),
        );
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("simplifier-r{round}.md"));
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
            cli: vendor_kind.direct_cli().unwrap_or(crate::selection::CliKind::Opencode),
            launch_name: model.clone(),
            prompt_path: prompt_path.clone(),
            effort,
            modes,
        };
        // Code-producing stages share the coder/reviewer guard mode.
        let dirty = self.capture_run_guard(
            "simplifier",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let window_name = run_label_with_model("[Simplifier]", &model, vendor_kind, effort);
        let run_id = self.state.next_agent_run_id();
        let run_key = Self::run_key_for("simplifier", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&simplification_path), &run_key, &artifacts_dir)
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
                Some(&simplification_path),
                crate::acp::AcpLaunchPolicy::simplifier(&simplification_path, &live_summary_path),
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    run_id,
                    "simplifier",
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
                self.record_agent_error(format!("failed to launch simplifier: {err}"));
                false
            }
        }
    }
    /// Co-located success-finalization for `Phase::Simplification(round)`.
    ///
    /// The artifact-validation gate above has already accepted the
    /// simplification TOML; on success we hand control to FinalValidation.
    /// The simplifier's verdict is advisory only — final validation makes
    /// its own call against idea + spec, so we don't branch on the parsed
    /// status here.
    pub(crate) fn finalize_simplification_success(
        &mut self,
        run: &crate::state::RunRecord,
        round: u32,
    ) -> Result<()> {
        self.finalize_run_record(run.id, true, None);
        self.clear_agent_error();
        let _ = session_state::transitions::enter_final_validation(&mut self.state, round)?;
        Ok(())
    }
}
