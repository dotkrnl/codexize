use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::{App, guard};
use crate::app::models::vendor_tag;
use crate::app::prompts::{read_review_scope, simplifier_prompt};
use crate::runner::launch_noninteractive_with_policy;
use crate::selection::CachedModel;
use crate::selection::config::SelectionPhase;
use crate::state::{self as session_state, Phase};

impl App {
    pub(in crate::app) fn launch_simplifier(&mut self) {
        let _ = self.launch_simplifier_with_model(None);
    }

    pub(in crate::app) fn launch_simplifier_with_model(
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
        let review_scope_file = round_dir.join("review_scope.toml");
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
                )
            })
            .or_else(|| self.round_stage_model("simplifier", round))
            .or_else(|| self.round_stage_model("coder", round))
            .or_else(|| {
                self.choose_primary_model(None, SelectionPhase::Build, effort, modes.cheap)
            });
        let Some((model, vendor_kind, vendor)) = chosen else {
            self.record_agent_error("no model available for simplifier".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };

        let attempt = self.attempt_for("simplifier", None, round);
        let live_summary_path = self.live_summary_path_for_run("simplifier", None, round, attempt);
        let prompt = simplifier_prompt(
            &session_dir,
            &review_scope_file,
            &simplification_path,
            &live_summary_path,
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
        let run_key = Self::run_key_for("simplifier", None, round, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&simplification_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            launch_noninteractive_with_policy(
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
                    "simplifier",
                    None,
                    round,
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
            Err(err) => {
                self.record_agent_error(format!("failed to launch simplifier: {err}"));
                false
            }
        }
    }
}
