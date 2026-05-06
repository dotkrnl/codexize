use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::prompts::brainstorm_prompt;
use crate::app::{App, guard};
use crate::artifacts::{ArtifactKind, SkipToImplProposal};
use crate::selection::CachedModel;
use crate::state::{self as session_state, Phase};
use anyhow::Result;
impl App {
    pub(crate) fn launch_brainstorm(&mut self, idea: String) {
        let _ = self.launch_brainstorm_with_model(idea, None);
    }
    pub(crate) fn launch_brainstorm_with_model(
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
        let guard_mode = if modes.yolo {
            guard::GuardMode::AutoReset
        } else {
            guard::GuardMode::AskOperator
        };
        let dirty = self.capture_run_guard("brainstorm", None, 1, attempt, guard_mode);
        let window_name = run_label_with_model("[Brainstorm]", &model, vendor_kind, effort);
        let run_id = self.state.next_agent_run_id();
        let run_key = Self::run_key_for("brainstorm", None, 1, attempt);
        let artifacts_dir = session_state::session_dir(&self.state.session_id).join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&spec_path), &run_key, &artifacts_dir)
        {
            result
        } else if modes.yolo {
            self.runner_supervisor.launch_noninteractive(
                run_id,
                &window_name,
                &run,
                vendor_kind,
                &run_key,
                &artifacts_dir,
                Some(&spec_path),
            )
        } else {
            self.runner_supervisor.launch_interactive(
                run_id,
                &window_name,
                &run,
                vendor_kind,
                &run_key,
                &artifacts_dir,
                Some(&spec_path),
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
                    run_id,
                    "brainstorm",
                    None,
                    1,
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
                self.record_agent_error(format!("failed to launch brainstorm: {e}"));
                false
            }
        }
    }
    /// Co-located success-finalization for `Phase::BrainstormRunning`.
    ///
    /// Reads the optional `skip_proposal.toml` and `session_summary.toml`
    /// artifacts, marks the run done, and routes the pipeline to either
    /// `SkipToImplPending` (proposed) or `SpecReviewRunning` (default).
    pub(crate) fn finalize_brainstorm_success(
        &mut self,
        run: &crate::state::RunRecord,
    ) -> Result<()> {
        let session_dir = session_state::session_dir(&self.state.session_id);
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
        Ok(())
    }
}
