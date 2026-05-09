use crate::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::app::prompts::dreaming_prompt;
use crate::app::{App, guard};
use crate::selection::CachedModel;
use crate::selection::config::SelectionPhase;
use crate::state::{self as session_state, Phase};
use anyhow::Result;

impl App {
    pub(crate) fn launch_dreaming(&mut self) {
        let _ = self.launch_dreaming_with_model(None);
    }

    pub(crate) fn launch_dreaming_with_model(
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
        let Phase::Dreaming(round) = self.state.current_phase else {
            return false;
        };
        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let memory_root = self.memory_root();
        let dream_report_path = crate::logic::memory::dream_report_path(&memory_root, round);
        if let Some(parent) = dream_report_path.parent()
            && let Err(err) = std::fs::create_dir_all(parent)
        {
            self.record_agent_error(format!("error creating dream report dir: {err}"));
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }
        // Re-entry of Phase::Dreaming(round) always restarts from scratch;
        // remove any prior report so the current attempt does not accidentally
        // finalize against stale output.
        let _ = std::fs::remove_file(&dream_report_path);

        let modes = self.state.launch_modes();
        let effort = modes.effort_for(EffortLevel::Normal, SelectionPhase::Review);
        let chosen = self.choose_primary_model(
            override_model.as_ref(),
            SelectionPhase::Review,
            effort,
            modes.cheap,
        );
        let Some((model, vendor_kind, vendor, cli, launch_name)) = chosen else {
            self.record_agent_error("no model available for dreaming".to_string());
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        };

        let attempt = self.attempt_for("dreaming", None, round);
        let live_summary_path = self.live_summary_path_for_run("dreaming", None, round, attempt);
        let prompt = dreaming_prompt(
            &session_dir,
            &dream_report_path,
            &live_summary_path,
            self.prompt_meta(),
        );
        let prompt_path = session_dir
            .join("prompts")
            .join(format!("dreaming-r{round}.md"));
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt) {
            self.record_agent_error(format!("error writing prompt: {err}"));
            let _ = self.state.save();
            self.rebuild_tree_view(None);
            return false;
        }

        let run = AgentRun {
            model: model.clone(),
            cli,
            launch_name,
            prompt_path: prompt_path.clone(),
            effort,
            modes,
        };
        let dirty = self.capture_run_guard(
            "dreaming",
            None,
            round,
            attempt,
            guard::GuardMode::AutoReset,
        );
        let window_name = run_label_with_model("[Dreaming]", &model, vendor_kind, effort);
        let run_id = self.state.next_agent_run_id();
        let run_key = Self::run_key_for("dreaming", None, round, attempt);
        let artifacts_dir = session_dir.join("artifacts");
        let launch_result = if let Some(result) =
            self.try_test_launch(Some(&dream_report_path), &run_key, &artifacts_dir)
        {
            result
        } else {
            self.runner_supervisor.launch_noninteractive_with_policy(
                run_id,
                &window_name,
                &run,
                &run_key,
                &artifacts_dir,
                Some(&dream_report_path),
                crate::acp::AcpLaunchPolicy::dreaming(&dream_report_path, &live_summary_path),
            )
        };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    run_id,
                    "dreaming",
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
                self.record_agent_error(format!("failed to launch dreaming: {err}"));
                let _ = self.state.save();
                self.rebuild_tree_view(None);
                false
            }
        }
    }

    pub(crate) fn finalize_dreaming_success(
        &mut self,
        run: &crate::state::RunRecord,
        round: u32,
    ) -> Result<()> {
        // reasons.rs already gates completion on dream-report validity for the
        // noninteractive path; finalize the run first so a late validation miss
        // does not leave the run stuck in Running state.
        self.finalize_run_record(run.id, true, None);
        self.clear_agent_error();
        let memory_root = self.memory_root();
        let report_path = crate::logic::memory::dream_report_path(&memory_root, round);
        if let Err(err) = crate::data::memory::validate_dream_report_file(&report_path) {
            self.record_agent_error(format!("invalid dream report: {err}"));
            return Ok(());
        }
        self.transition_to_phase(Phase::Done)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::adapters::EffortLevel;
    use crate::app::test_support::{key, mk_app};
    use crate::app::{ModalKind, StageId, TestLaunchHarness, TestLaunchOutcome};
    use crate::selection::{CachedModel, IpbrPhaseScores, ScoreSource, SubscriptionKind};
    use crate::state::{
        self as session_state, DreamingDecision, DreamingDecisionKind, LaunchModes, Phase,
        RunRecord, RunStatus, SessionState,
    };
    use crossterm::event::KeyCode;
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    fn with_temp_session<T>(label: &str, f: impl FnOnce(String) -> T) -> T {
        let temp = tempfile::TempDir::new().unwrap();
        let session_id = temp
            .path()
            .join(".codexize")
            .join("sessions")
            .join(label)
            .display()
            .to_string();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(session_id)));
        result.unwrap()
    }

    fn cached_model() -> CachedModel {
        CachedModel {
            subscription: SubscriptionKind::Codex,
            name: "dream-model".to_string(),
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
            ipbr_match_key: Some("dream-model".to_string()),
            candidates: Vec::new(),
            selected_candidate: None,
            quota_percent: Some(100),
            quota_resets_at: None,
            display_order: 0,
            fallback_from: None,
        }
    }

    fn prepare_memory(session_id: &str) {
        let memory = crate::logic::memory::memory_root_from_session_path(
            &session_state::session_dir(session_id),
        );
        std::fs::create_dir_all(&memory).unwrap();
        std::fs::write(memory.join("index.md"), "# Memory\n").unwrap();
        std::fs::write(
            memory.join("manifest.toml"),
            "schema_version = 1\nentries = []\n",
        )
        .unwrap();
    }

    fn dream_report_body() -> String {
        r##"schema_version = 1
status = "completed"
summary = "Consolidated memory lessons."
started_at = "2026-05-06T22:00:00Z"
ended_at = "2026-05-06T22:01:00Z"
inputs = ["index.md", "manifest.toml"]

[[changes]]
kind = "index_updated"
target = "index.md#memory-lessons"
reason = "Captured durable session guidance."
"##
        .to_string()
    }

    fn running_dream(run_id: u64, round: u32) -> RunRecord {
        RunRecord {
            id: run_id,
            stage: "dreaming".to_string(),
            task_id: None,
            round,
            attempt: 1,
            model: "dream-model".to_string(),
            vendor: "codex".to_string(),
            window_name: "[Dreaming] dream-model".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            modes: LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        }
    }

    #[test]
    fn launch_dreaming_writes_prompt_and_tracks_review_effort_run() {
        with_temp_session("dream-launch", |session_id| {
            let mut state = SessionState::new(session_id);
            state.current_phase = Phase::Dreaming(2);
            prepare_memory(&state.session_id);
            let mut app = mk_app(state);
            app.models.push(cached_model());
            app.test_launch_harness = Some(Arc::new(Mutex::new(TestLaunchHarness {
                outcomes: VecDeque::from([TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: Some(dream_report_body()),
                    launch_error: None,
                }]),
            })));

            assert!(app.launch_dreaming_with_model(None));

            let run = app.state.agent_runs.last().expect("dream run recorded");
            assert_eq!(run.stage, "dreaming");
            assert_eq!(run.round, 2);
            assert_eq!(run.model, "dream-model");
            assert_eq!(run.effort, EffortLevel::Normal);
            assert_eq!(run.status, RunStatus::Running);

            let session_dir = session_state::session_dir(&app.state.session_id);
            let prompt = std::fs::read_to_string(session_dir.join("prompts/dreaming-r2.md"))
                .expect("dream prompt");
            assert!(prompt.contains("Dream report:"));
            assert!(prompt.contains(".codexize/memory/dreams/dream-0002.toml"));
            let memory = crate::logic::memory::memory_root_from_session_path(&session_dir);
            assert!(memory.join("dreams/dream-0002.toml").is_file());
        });
    }

    #[test]
    fn dreaming_success_validates_report_and_finishes_session() {
        with_temp_session("dream-success", |session_id| {
            let mut state = SessionState::new(session_id);
            state.current_phase = Phase::Dreaming(4);
            state.dreaming_decision = Some(DreamingDecision {
                kind: DreamingDecisionKind::OperatorAccepted,
                round: 4,
                reason: Some("Memory changed.".to_string()),
            });
            prepare_memory(&state.session_id);
            let run = running_dream(42, 4);
            state.agent_runs.push(run.clone());
            let memory = crate::logic::memory::memory_root_from_session_path(
                &session_state::session_dir(&state.session_id),
            );
            let dreams = memory.join("dreams");
            std::fs::create_dir_all(&dreams).unwrap();
            std::fs::write(dreams.join("dream-0004.toml"), dream_report_body()).unwrap();
            let mut app = mk_app(state);

            app.finalize_dreaming_success(&run, 4).unwrap();

            assert_eq!(app.state.current_phase, Phase::Done);
            assert_eq!(
                app.state.agent_runs.last().map(|run| run.status),
                Some(RunStatus::Done)
            );
            assert!(app.state.agent_error.is_none());
        });
    }

    #[test]
    fn dreaming_failure_can_be_skipped_without_rerunning_validation() {
        with_temp_session("dream-failure-skip", |session_id| {
            let mut state = SessionState::new(session_id);
            state.current_phase = Phase::Dreaming(5);
            state.dreaming_decision = Some(DreamingDecision {
                kind: DreamingDecisionKind::OperatorAccepted,
                round: 5,
                reason: Some("Consolidate memory.".to_string()),
            });
            state.agent_runs.push(running_dream(77, 5));
            let mut app = mk_app(state);
            app.record_agent_error("invalid dream report".to_string());

            assert_eq!(
                app.active_modal(),
                Some(ModalKind::StageError(StageId::Dreaming))
            );
            assert!(!app.handle_modal_key(
                ModalKind::StageError(StageId::Dreaming),
                key(KeyCode::Char('s')),
            ));

            assert_eq!(app.state.current_phase, Phase::Done);
            assert_eq!(
                app.state.dreaming_decision.as_ref().map(|d| d.kind),
                Some(DreamingDecisionKind::OperatorSkipped)
            );
        });
    }

    #[test]
    fn dreaming_failure_retry_relaunches_dreaming_run() {
        with_temp_session("dream-failure-retry", |session_id| {
            let mut state = SessionState::new(session_id);
            state.current_phase = Phase::Dreaming(6);
            state.dreaming_decision = Some(DreamingDecision {
                kind: DreamingDecisionKind::OperatorAccepted,
                round: 6,
                reason: Some("Consolidate memory.".to_string()),
            });
            let mut failed = running_dream(88, 6);
            failed.status = RunStatus::Failed;
            failed.error = Some("invalid dream report".to_string());
            state.agent_runs.push(failed);
            prepare_memory(&state.session_id);
            let mut app = mk_app(state);
            app.models.push(cached_model());
            app.record_agent_error("invalid dream report".to_string());
            app.test_launch_harness = Some(Arc::new(Mutex::new(TestLaunchHarness {
                outcomes: VecDeque::from([TestLaunchOutcome {
                    exit_code: 0,
                    artifact_contents: Some(dream_report_body()),
                    launch_error: None,
                }]),
            })));

            assert!(!app.handle_modal_key(
                ModalKind::StageError(StageId::Dreaming),
                key(KeyCode::Char('r')),
            ));

            let run = app.state.agent_runs.last().expect("dream retry run");
            assert_eq!(run.stage, "dreaming");
            assert_eq!(run.round, 6);
            assert_eq!(run.attempt, 2);
            assert_eq!(run.status, RunStatus::Running);
            assert!(app.state.agent_error.is_none());
        });
    }
}
