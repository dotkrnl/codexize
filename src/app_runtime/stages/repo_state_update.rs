//! Non-interactive repo-state update stage.
//!
//! Runs when a `WaitingToImplement` session's recorded
//! `planned_after_session_id` no longer matches the current newest
//! earlier-`Done` baseline. The agent reconciles the current session's
//! spec/plan against the new repository state (and the tasks +
//! final-validation verdicts of every newly-completed earlier session)
//! and either:
//!
//! - rewrites both `spec.md` and `plan.md` and reports
//!   `status = "implementable"` — the orchestrator then updates the
//!   session's `planned_after_session_id` to the current baseline at the
//!   moment of finalization and transitions to `ShardingRunning`; or
//! - reports `status = "not_implementable"` — the orchestrator routes the
//!   session to `BlockedNeedsUser` with `BlockOrigin::RepoStateUpdate`.
//!
//! See spec § Repo-state update stage and AC-6.
use crate::app::prompts::{
    RepoStateUpdateCompletedSession, RepoStateUpdatePromptInputs, repo_state_update_prompt,
};
use crate::app::{App, guard};
use crate::data::adapters::{AgentRun, EffortLevel, run_label_with_model};
use crate::data::artifacts::ArtifactKind;
use crate::data::repo_state_update::{RepoStateUpdateReport, RepoStateUpdateStatus};
use crate::scheduler::{WaitingDispatch, decide_waiting_dispatch};
use crate::selection::CachedModel;
use crate::state::{self as session_state, BlockOrigin, Stage};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const STAGE: &str = "repo-state-update";
/// Hidden directory under the session root that holds the byte-for-byte
/// snapshot of `spec.md` and `plan.md` taken at repo-state-update launch
/// time. Finalization compares the current file contents against these
/// snapshots; a report that claims `rewrote_spec=true`/`rewrote_plan=true`
/// while the on-disk content is byte-identical to the snapshot is a stage
/// failure (spec § Failure and edge behavior line 306).
const REPO_STATE_UPDATE_BASELINE_DIR: &str = ".repo-state-update-baseline";

impl App {
    pub(crate) fn launch_repo_state_update_with_model(
        &mut self,
        override_model: Option<CachedModel>,
    ) -> bool {
        self.clear_agent_error();
        if !self.guard_models_loaded() {
            return false;
        }
        let session_id = self.state.session_id.clone();
        let session_dir = session_state::session_dir(&session_id);
        let artifacts = session_dir.join("artifacts");
        let spec_path = artifacts.join(ArtifactKind::Spec.filename());
        let plan_path = artifacts.join(ArtifactKind::Plan.filename());
        let report_path = artifacts.join(ArtifactKind::RepoStateUpdate.filename());
        // A leftover report from a prior attempt must not be mistaken for
        // this run's verdict during finalization.
        let _ = std::fs::remove_file(&report_path);
        // Snapshot spec.md/plan.md bytes before the agent starts so
        // finalization can detect a false-positive "rewrote_*=true" report.
        // ACP allowed_write_paths only restrict agent-side writes; the
        // orchestrator writes the baseline directly.
        if let Err(err) = capture_baseline(&session_dir, &spec_path, &plan_path) {
            self.surface_boundary_error(
                format!("error capturing repo-state update baseline: {err}"),
                true,
            );
            return false;
        }

        let modes = self.state.launch_modes();
        let stage = Self::selection_stage_for_stage(STAGE);
        let effort = modes.effort_for(EffortLevel::Normal, stage);
        let Some(chosen) =
            self.choose_primary_model(override_model.as_ref(), stage, effort, modes.cheap)
        else {
            self.record_agent_error("no model available with quota".to_string());
            self.save_state();
            self.rebuild_tree_view(None);
            return false;
        };
        let (model, subscription_tag, cli, launch_name, effort_mapping, effort_eligible) = chosen;

        let attempt = self.attempt_for(STAGE, None, 1);
        let live_summary_path = self.live_summary_path_for_run(STAGE, None, 1, attempt);

        // Snapshot every input before rendering so the prompt agrees with
        // what finalization will compare baselines against.
        let recorded_baseline = self
            .state
            .planned_after_session_id
            .clone()
            .unwrap_or_default();
        let (current_baseline, newly_completed) = self.compute_repo_state_update_inputs();
        let current_baseline_str = current_baseline.unwrap_or_default();
        // Convention used elsewhere in the runtime (e.g. `write_review_scope_artifact`):
        // production paths read the live git HEAD; tests pin a deterministic
        // placeholder so the prompt snapshot is stable.
        #[cfg(test)]
        let git_head = String::from("test-head");
        #[cfg(not(test))]
        let git_head = crate::app::prompts::git_rev_parse_head().unwrap_or_default();
        let completed_inputs: Vec<RepoStateUpdateCompletedSession<'_>> = newly_completed
            .iter()
            .map(|entry| RepoStateUpdateCompletedSession {
                session_id: entry.session_id.as_str(),
                tasks_toml: entry.tasks_toml.as_path(),
                final_validation_paths: entry.final_validation_paths.as_slice(),
            })
            .collect();
        let prompt = repo_state_update_prompt(RepoStateUpdatePromptInputs {
            spec_path: spec_path.as_path(),
            plan_path: plan_path.as_path(),
            report_path: report_path.as_path(),
            live_summary_path: live_summary_path.as_path(),
            recorded_baseline: &recorded_baseline,
            current_baseline: &current_baseline_str,
            git_head: &git_head,
            newly_completed: &completed_inputs,
            meta: self.prompt_meta(),
        });
        let prompt_path = session_dir.join("prompts").join("repo-state-update.md");
        if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(err) = std::fs::write(&prompt_path, prompt) {
            self.surface_boundary_error(format!("error writing prompt: {err}"), true);
            return false;
        }
        let run = AgentRun {
            model: model.clone(),
            cli,
            launch_name,
            prompt_path: prompt_path.clone(),
            effort,
            effort_mapping: effort_mapping.clone(),
            effort_eligible,
            modes,
        };
        let dirty = self.capture_run_guard(STAGE, None, 1, attempt, guard::GuardMode::AutoReset);
        let window_name = run_label_with_model(
            "[RepoStateUpdate]",
            &model,
            effort,
            effort_eligible,
            &effort_mapping,
        );
        let run_id = self.state.next_agent_run_id();
        let run_key = Self::run_key_for(STAGE, None, 1, attempt);
        let policy = crate::data::acp::AcpLaunchPolicy::repo_state_update(
            &spec_path,
            &plan_path,
            &report_path,
            &live_summary_path,
        );
        let launch_result =
            if let Some(result) = self.try_test_launch(Some(&report_path), &run_key, &artifacts) {
                result
            } else {
                self.runner_supervisor.launch_noninteractive_with_policy(
                    run_id,
                    &window_name,
                    &run,
                    &run_key,
                    &artifacts,
                    Some(&report_path),
                    policy,
                )
            };
        match launch_result {
            Ok(()) => {
                self.start_run_tracking(
                    run_id,
                    STAGE,
                    None,
                    1,
                    model,
                    subscription_tag,
                    window_name,
                    effort,
                    effort_mapping,
                    effort_eligible,
                    modes,
                    prompt_path,
                );
                if dirty {
                    self.emit_dirty_tree_warning();
                }
                true
            }
            Err(e) => {
                self.surface_boundary_error(
                    format!("failed to launch repo-state update: {e}"),
                    true,
                );
                false
            }
        }
    }

    /// Resolve the inputs to a repo-state update launch from the current
    /// scheduler scan: the current newest-earlier-`Done` baseline, plus
    /// every earlier non-archived session that is `Done` and sorts strictly
    /// after the session's recorded `planned_after_session_id` (or all
    /// earlier `Done` sessions when no baseline is recorded). Each entry
    /// carries that session's `tasks.toml` path and every
    /// `final_validation_*.toml` it produced.
    pub(crate) fn compute_repo_state_update_inputs(
        &self,
    ) -> (Option<String>, Vec<NewlyCompletedSession>) {
        let entries =
            match crate::data::picker_io::scan_sessions_by_creation_order(&self.sessions_root()) {
                Ok(list) => list,
                Err(_) => return (None, Vec::new()),
            };
        let current_baseline =
            crate::data::picker_io::newest_earlier_done_baseline(&self.state.session_id, &entries);
        // Spec § Failure and edge behavior: when `planned_after_session_id`
        // references a missing session, run against all earlier completed
        // sessions. A `None` recorded baseline is treated the same way: all
        // earlier Done sessions are newly-completed from the session's
        // perspective.
        let recorded = self
            .state
            .planned_after_session_id
            .as_deref()
            .filter(|rec| entries.iter().any(|e| e.session_id.as_str() == *rec));
        let mut newly_completed = Vec::new();
        for entry in &entries {
            if entry.archived {
                continue;
            }
            if entry.current_stage != Stage::Done {
                continue;
            }
            if entry.session_id >= self.state.session_id {
                continue;
            }
            if let Some(rec) = recorded
                && entry.session_id.as_str() <= rec
            {
                // Already part of the planned-after baseline; nothing new.
                continue;
            }
            let session_dir = session_state::session_dir(&entry.session_id);
            let tasks_toml = session_dir.join("artifacts").join("tasks.toml");
            let final_validation_paths =
                collect_final_validation_artifacts(&session_dir.join("artifacts"));
            newly_completed.push(NewlyCompletedSession {
                session_id: entry.session_id.clone(),
                tasks_toml,
                final_validation_paths,
            });
        }
        (current_baseline, newly_completed)
    }

    /// Co-located success-finalization for `Stage::RepoStateUpdateRunning`.
    ///
    /// On `implementable`: require both `spec.md` and `plan.md` to have
    /// been rewritten (otherwise stage failure), update
    /// `planned_after_session_id` to the current baseline at this
    /// moment, and transition to `ShardingRunning`. On `not_implementable`:
    /// route to `BlockedNeedsUser`. The report file itself is the verdict
    /// — partial responses fail validation in `repo_state_update::parse`.
    pub(crate) fn finalize_repo_state_update_success(
        &mut self,
        run: &crate::state::RunRecord,
    ) -> Result<()> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let report_path = artifacts.join(ArtifactKind::RepoStateUpdate.filename());
        let report = crate::data::repo_state_update::validate(&report_path)
            .with_context(|| format!("invalid {}", report_path.display()))?;
        match report.status {
            RepoStateUpdateStatus::Implementable => {
                let spec_path = artifacts.join(ArtifactKind::Spec.filename());
                let plan_path = artifacts.join(ArtifactKind::Plan.filename());
                // Belt-and-suspenders: the parser already enforces both
                // rewrote_* fields, but the underlying files must exist so
                // a successor stage can read them. A report that claims
                // rewrote_spec/plan but left empty files is a stage failure.
                require_nonempty_artifact(&spec_path, &report)?;
                require_nonempty_artifact(&plan_path, &report)?;
                // Spec § Failure and edge behavior line 306: a report
                // claiming success but leaving either file byte-identical
                // to its pre-run snapshot is treated as a stage failure.
                // The baseline was captured by the launcher; tests that
                // exercise finalize directly must first call
                // `capture_repo_state_update_baseline` to simulate it.
                require_rewrote_against_baseline(&session_dir, &spec_path, "spec.md")?;
                require_rewrote_against_baseline(&session_dir, &plan_path, "plan.md")?;
                let (current_baseline, _) = self.compute_repo_state_update_inputs();
                self.state.planned_after_session_id = current_baseline;
                self.finalize_run_record(run.id, true, None);
                self.clear_agent_error();
                clear_baseline(&session_dir);
                self.transition_to_stage(Stage::ShardingRunning)?;
            }
            RepoStateUpdateStatus::NotImplementable => {
                self.finalize_run_record(run.id, true, None);
                self.clear_agent_error();
                clear_baseline(&session_dir);
                self.transition_to_blocked(BlockOrigin::RepoStateUpdate)?;
            }
        }
        Ok(())
    }

    /// Production dispatch out of `Stage::WaitingToImplement`: pure stage
    /// transition that the per-tick auto-launch loop consumes. Compares
    /// the session's recorded `planned_after_session_id` with the current
    /// newest-earlier-`Done` baseline and transitions to either
    /// `RepoStateUpdateRunning` (baselines differ) or `ShardingRunning`
    /// (baselines match, including both `None`).
    ///
    /// This is the production wiring for the scheduler's
    /// `decide_waiting_dispatch` helper; the launch of the resulting stage
    /// is left to the next auto-launch tick so the same code path covers
    /// resumed sessions where the stage was already advanced.
    pub(crate) fn dispatch_waiting_to_implement(&mut self) {
        let (current_baseline, _) = self.compute_repo_state_update_inputs();
        let decision = decide_waiting_dispatch(
            self.state.planned_after_session_id.as_deref(),
            current_baseline.as_deref(),
        );
        let next_stage = match decision {
            WaitingDispatch::Sharding => Stage::ShardingRunning,
            WaitingDispatch::RepoStateUpdate => Stage::RepoStateUpdateRunning,
        };
        if let Err(err) = self.transition_to_stage(next_stage) {
            self.surface_boundary_error(format!("failed to dispatch waiting session: {err}"), true);
        }
    }

    /// Public for tests that exercise finalize directly: snapshot the
    /// current `spec.md`/`plan.md` bytes into the hidden baseline dir so
    /// `finalize_repo_state_update_success` can detect a false-positive
    /// rewrite. In production this fires from `launch_repo_state_update_with_model`.
    #[cfg(test)]
    pub(crate) fn capture_repo_state_update_baseline(&self) -> Result<()> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        capture_baseline(
            &session_dir,
            &artifacts.join(ArtifactKind::Spec.filename()),
            &artifacts.join(ArtifactKind::Plan.filename()),
        )
    }
}

fn baseline_dir(session_dir: &Path) -> PathBuf {
    session_dir.join(REPO_STATE_UPDATE_BASELINE_DIR)
}

fn capture_baseline(session_dir: &Path, spec_path: &Path, plan_path: &Path) -> Result<()> {
    let dir = baseline_dir(session_dir);
    // Wipe any prior snapshot so a leftover from a previous attempt
    // can't masquerade as this attempt's pre-run state.
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("removing stale {}", dir.display()))?;
    }
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    snapshot_file(spec_path, &dir.join("spec.md"))?;
    snapshot_file(plan_path, &dir.join("plan.md"))?;
    Ok(())
}

fn snapshot_file(source: &Path, dest: &Path) -> Result<()> {
    // A missing source is captured as an empty snapshot so a fresh write
    // counts as a rewrite. This also covers the "first run, no prior
    // spec/plan" case without forcing the launcher to special-case it.
    let bytes = std::fs::read(source).unwrap_or_default();
    std::fs::write(dest, bytes).with_context(|| format!("writing {}", dest.display()))
}

fn clear_baseline(session_dir: &Path) {
    let dir = baseline_dir(session_dir);
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn require_rewrote_against_baseline(
    session_dir: &Path,
    current_path: &Path,
    snapshot_name: &str,
) -> Result<()> {
    let baseline_path = baseline_dir(session_dir).join(snapshot_name);
    if !baseline_path.exists() {
        anyhow::bail!(
            "repo-state update reported success but the pre-run baseline at \
             {} is missing — cannot verify {} was actually rewritten",
            baseline_path.display(),
            snapshot_name
        );
    }
    let before = std::fs::read(&baseline_path).with_context(|| {
        format!(
            "reading repo-state update baseline {}",
            baseline_path.display()
        )
    })?;
    let after = std::fs::read(current_path)
        .with_context(|| format!("reading {}", current_path.display()))?;
    if before == after {
        anyhow::bail!(
            "repo-state update reported success but {} is byte-identical to its pre-run state — the agent did not rewrite it",
            current_path.display()
        );
    }
    Ok(())
}

/// One earlier `Done` session whose artifacts are part of this repo-state
/// update's input set.
#[derive(Debug, Clone)]
pub(crate) struct NewlyCompletedSession {
    pub session_id: String,
    pub tasks_toml: PathBuf,
    pub final_validation_paths: Vec<PathBuf>,
}

fn collect_final_validation_artifacts(artifacts_dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(artifacts_dir) else {
        return Vec::new();
    };
    let mut paths: Vec<(u32, PathBuf)> = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Match `final_validation_<round>.toml`; ignore everything else.
        let Some(rest) = name.strip_prefix("final_validation_") else {
            continue;
        };
        let Some(round_str) = rest.strip_suffix(".toml") else {
            continue;
        };
        let Ok(round) = round_str.parse::<u32>() else {
            continue;
        };
        paths.push((round, path));
    }
    paths.sort_by_key(|(round, _)| *round);
    paths.into_iter().map(|(_, p)| p).collect()
}

fn require_nonempty_artifact(path: &Path, report: &RepoStateUpdateReport) -> Result<()> {
    let metadata = std::fs::metadata(path).with_context(|| {
        format!(
            "repo-state update reported {} but {} is missing",
            if report.rewrote_spec && path.ends_with("spec.md") {
                "rewrote_spec = true"
            } else if report.rewrote_plan && path.ends_with("plan.md") {
                "rewrote_plan = true"
            } else {
                "rewrite"
            },
            path.display()
        )
    })?;
    if metadata.len() == 0 {
        anyhow::bail!(
            "repo-state update reported success but {} is empty",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::test_support::mk_app;
    use crate::state::{LaunchModes, RunRecord, RunStatus, SessionState};

    fn with_temp_root<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::state::test_fs_lock().lock();
        let temp = tempfile::TempDir::new().unwrap();
        let prev = std::env::var_os("CODEXIZE_ROOT");
        // SAFETY: serialized by test_fs_lock and restored before returning.
        unsafe {
            std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        unsafe {
            match prev {
                Some(v) => std::env::set_var("CODEXIZE_ROOT", v),
                None => std::env::remove_var("CODEXIZE_ROOT"),
            }
        }
        result.unwrap()
    }

    fn run_record(id: u64) -> RunRecord {
        RunRecord {
            id,
            stage: STAGE.to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "test-model".to_string(),
            subscription_label: "test-vendor".to_string(),
            window_name: "[RepoStateUpdate] test-model".to_string(),
            started_at: chrono::Utc::now(),
            ended_at: None,
            status: RunStatus::Running,
            error: None,
            effort: EffortLevel::Normal,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            effort_eligible: false,
            modes: LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
            section_path: None,
        }
    }

    fn write_report(session_id: &str, body: &str) {
        let artifacts = session_state::session_dir(session_id).join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(
            artifacts.join(ArtifactKind::RepoStateUpdate.filename()),
            body,
        )
        .unwrap();
    }

    fn save_done_session(id: &str) {
        let mut state = SessionState::new(id.to_string());
        state.idea_text = Some(format!("idea {id}"));
        state.current_stage = Stage::Done;
        state.save().unwrap();
        let artifacts = session_state::session_dir(id).join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(
            artifacts.join("tasks.toml"),
            "[[tasks]]\nid = 1\ntitle = \"x\"\n",
        )
        .unwrap();
        std::fs::write(
            artifacts.join("final_validation_1.toml"),
            "status = \"goal_met\"\n",
        )
        .unwrap();
    }

    fn write_spec_and_plan(session_id: &str) {
        let artifacts = session_state::session_dir(session_id).join("artifacts");
        std::fs::create_dir_all(&artifacts).unwrap();
        std::fs::write(artifacts.join("spec.md"), "# spec\nbody\n").unwrap();
        std::fs::write(artifacts.join("plan.md"), "# plan\nbody\n").unwrap();
    }

    #[test]
    fn implementable_transitions_to_sharding_and_updates_baseline() {
        with_temp_root(|| {
            save_done_session("20260511-090000-000000001");
            // Current session is later, in RepoStateUpdateRunning, with no
            // recorded baseline — finalization should set it to the
            // newly-discovered Done session id.
            let session_id = "20260511-092000-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::RepoStateUpdateRunning;
            state.planned_after_session_id = None;
            let run = run_record(7);
            state.agent_runs.push(run.clone());
            state.save().unwrap();
            write_spec_and_plan(session_id);
            let app_for_baseline = mk_app(state.clone());
            app_for_baseline
                .capture_repo_state_update_baseline()
                .unwrap();
            // Simulate the agent actually rewriting both files: contents
            // must differ byte-for-byte from the captured baseline.
            let artifacts = session_state::session_dir(session_id).join("artifacts");
            std::fs::write(artifacts.join("spec.md"), "# spec\nrewritten\n").unwrap();
            std::fs::write(artifacts.join("plan.md"), "# plan\nrewritten\n").unwrap();
            write_report(
                session_id,
                r#"status = "implementable"
summary = "Reconciled spec and plan against the new repo state."
rewrote_spec = true
rewrote_plan = true
"#,
            );
            let mut app = mk_app(state);
            app.finalize_repo_state_update_success(&run).unwrap();
            assert_eq!(app.state.current_stage, Stage::ShardingRunning);
            assert_eq!(
                app.state.planned_after_session_id.as_deref(),
                Some("20260511-090000-000000001")
            );
            // Baseline directory is cleared on success so a subsequent
            // attempt captures a fresh pre-run state.
            let baseline =
                session_state::session_dir(session_id).join(REPO_STATE_UPDATE_BASELINE_DIR);
            assert!(
                !baseline.exists(),
                "baseline dir should be cleared on success"
            );
        });
    }

    #[test]
    fn implementable_without_rewriting_spec_or_plan_fails() {
        // False-positive report: the agent claims rewrote_spec and
        // rewrote_plan, but the underlying files are byte-identical to
        // their pre-run snapshots. Spec § Failure and edge behavior
        // line 306: treat as stage failure.
        with_temp_root(|| {
            let session_id = "20260511-092500-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::RepoStateUpdateRunning;
            let run = run_record(21);
            state.agent_runs.push(run.clone());
            state.save().unwrap();
            write_spec_and_plan(session_id);
            mk_app(state.clone())
                .capture_repo_state_update_baseline()
                .unwrap();
            // No rewrite happens — spec.md and plan.md remain at their
            // pre-launch bytes. The report still claims success.
            write_report(
                session_id,
                r#"status = "implementable"
summary = "Pretending to have rewritten both files."
rewrote_spec = true
rewrote_plan = true
"#,
            );
            let mut app = mk_app(state);
            let err = app
                .finalize_repo_state_update_success(&run)
                .expect_err("expected failure when no rewrite actually happened");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("spec.md") && msg.contains("byte-identical"),
                "error should call out the unchanged spec.md, got: {msg}"
            );
            // Stage must stay put so the operator can intervene.
            assert_eq!(app.state.current_stage, Stage::RepoStateUpdateRunning);
        });
    }

    #[test]
    fn implementable_with_only_plan_unchanged_fails() {
        // Mixed case: spec.md was rewritten but plan.md was not. The
        // stage must still fail.
        with_temp_root(|| {
            let session_id = "20260511-092600-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::RepoStateUpdateRunning;
            let run = run_record(22);
            state.agent_runs.push(run.clone());
            state.save().unwrap();
            write_spec_and_plan(session_id);
            mk_app(state.clone())
                .capture_repo_state_update_baseline()
                .unwrap();
            // Rewrite only spec.md.
            let artifacts = session_state::session_dir(session_id).join("artifacts");
            std::fs::write(artifacts.join("spec.md"), "# spec\nnew bytes\n").unwrap();
            write_report(
                session_id,
                r#"status = "implementable"
summary = "Rewrote spec but forgot plan."
rewrote_spec = true
rewrote_plan = true
"#,
            );
            let mut app = mk_app(state);
            let err = app
                .finalize_repo_state_update_success(&run)
                .expect_err("expected failure when plan.md is unchanged");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("plan.md"),
                "error should call out the unchanged plan.md, got: {msg}"
            );
        });
    }

    #[test]
    fn implementable_missing_baseline_fails() {
        // Defensive: if the baseline snapshot is somehow absent at
        // finalize time (e.g., process crash between capture and
        // finalize), the stage cannot verify the rewrite and must fail
        // rather than silently accept the report.
        with_temp_root(|| {
            let session_id = "20260511-092700-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::RepoStateUpdateRunning;
            let run = run_record(23);
            state.agent_runs.push(run.clone());
            state.save().unwrap();
            write_spec_and_plan(session_id);
            // Note: no baseline capture.
            write_report(
                session_id,
                r#"status = "implementable"
summary = "Claims rewrites but no baseline exists."
rewrote_spec = true
rewrote_plan = true
"#,
            );
            let mut app = mk_app(state);
            let err = app
                .finalize_repo_state_update_success(&run)
                .expect_err("expected failure when baseline missing");
            assert!(
                format!("{err:#}").contains("baseline"),
                "error should mention the missing baseline"
            );
            assert_eq!(app.state.current_stage, Stage::RepoStateUpdateRunning);
        });
    }

    #[test]
    fn not_implementable_transitions_to_blocked_with_origin() {
        with_temp_root(|| {
            let session_id = "20260511-093000-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::RepoStateUpdateRunning;
            let run = run_record(11);
            state.agent_runs.push(run.clone());
            state.save().unwrap();
            write_report(
                session_id,
                r#"status = "not_implementable"
summary = "Earlier session already shipped this behavior."

[[blockers]]
description = "Cache layer is already in place."
evidence = ["src/cache.rs"]
"#,
            );
            let mut app = mk_app(state);
            app.finalize_repo_state_update_success(&run).unwrap();
            assert_eq!(app.state.current_stage, Stage::BlockedNeedsUser);
            assert_eq!(app.state.block_origin, Some(BlockOrigin::RepoStateUpdate));
        });
    }

    #[test]
    fn implementable_without_both_rewrites_fails() {
        with_temp_root(|| {
            let session_id = "20260511-094000-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::RepoStateUpdateRunning;
            let run = run_record(12);
            state.agent_runs.push(run.clone());
            state.save().unwrap();
            // Missing rewrote_plan — the schema validator rejects it.
            write_report(
                session_id,
                r#"status = "implementable"
summary = "Only rewrote one half."
rewrote_spec = true
"#,
            );
            let mut app = mk_app(state);
            let err = app
                .finalize_repo_state_update_success(&run)
                .expect_err("expected failure");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("rewrote_plan") || msg.contains("rewrote_spec"),
                "error should mention required rewrites, got: {msg}"
            );
            // Stage must stay put so the operator (or a retry) can decide.
            assert_eq!(app.state.current_stage, Stage::RepoStateUpdateRunning);
        });
    }

    #[test]
    fn implementable_with_empty_spec_or_plan_fails() {
        with_temp_root(|| {
            let session_id = "20260511-095000-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::RepoStateUpdateRunning;
            let run = run_record(13);
            state.agent_runs.push(run.clone());
            state.save().unwrap();
            // Report claims both rewritten but plan.md is empty on disk.
            let artifacts = session_state::session_dir(session_id).join("artifacts");
            std::fs::create_dir_all(&artifacts).unwrap();
            std::fs::write(artifacts.join("spec.md"), "# spec\nbody\n").unwrap();
            std::fs::write(artifacts.join("plan.md"), "").unwrap();
            write_report(
                session_id,
                r#"status = "implementable"
summary = "Claims rewrite but plan is empty."
rewrote_spec = true
rewrote_plan = true
"#,
            );
            let mut app = mk_app(state);
            let err = app
                .finalize_repo_state_update_success(&run)
                .expect_err("expected failure");
            assert!(format!("{err:#}").contains("plan.md"));
        });
    }

    #[test]
    fn inputs_include_only_newly_completed_earlier_sessions() {
        with_temp_root(|| {
            // Three earlier Done sessions; the recorded baseline already
            // covers the first two, so only the third is "newly completed".
            save_done_session("20260511-080000-000000001");
            save_done_session("20260511-081000-000000001");
            save_done_session("20260511-082000-000000001");
            let session_id = "20260511-092000-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::RepoStateUpdateRunning;
            state.planned_after_session_id = Some("20260511-081000-000000001".to_string());
            state.save().unwrap();
            let app = mk_app(state);
            let (current_baseline, newly) = app.compute_repo_state_update_inputs();
            assert_eq!(
                current_baseline.as_deref(),
                Some("20260511-082000-000000001")
            );
            let ids: Vec<&str> = newly.iter().map(|s| s.session_id.as_str()).collect();
            assert_eq!(ids, vec!["20260511-082000-000000001"]);
            let entry = &newly[0];
            assert!(entry.tasks_toml.ends_with("tasks.toml"));
            assert_eq!(entry.final_validation_paths.len(), 1);
        });
    }

    #[test]
    fn waiting_to_implement_dispatch_routes_to_sharding_when_baselines_match() {
        // Scheduler wiring: a `WaitingToImplement` session whose recorded
        // baseline matches the current newest-earlier-`Done` baseline must
        // skip the repo-state update and transition straight to sharding.
        with_temp_root(|| {
            save_done_session("20260511-080000-000000001");
            let session_id = "20260511-090000-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::WaitingToImplement;
            state.planned_after_session_id = Some("20260511-080000-000000001".to_string());
            state.save().unwrap();
            let mut app = mk_app(state);
            app.dispatch_waiting_to_implement();
            assert_eq!(app.state.current_stage, Stage::ShardingRunning);
            // The transition was persisted to disk.
            let reloaded = SessionState::load(session_id).unwrap();
            assert_eq!(reloaded.current_stage, Stage::ShardingRunning);
        });
    }

    #[test]
    fn waiting_to_implement_dispatch_routes_to_repo_state_update_when_baselines_differ() {
        // A new `Done` session has landed since this session was planned;
        // the recorded baseline points at the older one, so the stage must
        // run to reconcile spec/plan against the new repo state.
        with_temp_root(|| {
            save_done_session("20260511-080000-000000001");
            save_done_session("20260511-081000-000000001");
            let session_id = "20260511-090000-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::WaitingToImplement;
            state.planned_after_session_id = Some("20260511-080000-000000001".to_string());
            state.save().unwrap();
            let mut app = mk_app(state);
            app.dispatch_waiting_to_implement();
            assert_eq!(app.state.current_stage, Stage::RepoStateUpdateRunning);
            let reloaded = SessionState::load(session_id).unwrap();
            assert_eq!(reloaded.current_stage, Stage::RepoStateUpdateRunning);
        });
    }

    #[test]
    fn waiting_to_implement_dispatch_routes_to_sharding_when_no_earlier_done() {
        // Both baselines `None` is the "fresh queue, no earlier work to
        // reconcile against" case and routes to sharding.
        with_temp_root(|| {
            let session_id = "20260511-090000-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::WaitingToImplement;
            state.planned_after_session_id = None;
            state.save().unwrap();
            let mut app = mk_app(state);
            app.dispatch_waiting_to_implement();
            assert_eq!(app.state.current_stage, Stage::ShardingRunning);
        });
    }

    #[test]
    fn waiting_to_implement_dispatch_runs_when_recorded_baseline_is_missing() {
        // Spec § Failure and edge behavior: when the recorded
        // `planned_after_session_id` references a session that no longer
        // exists, the stage must still run against the current baseline.
        with_temp_root(|| {
            save_done_session("20260511-080000-000000001");
            let session_id = "20260511-090000-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::WaitingToImplement;
            state.planned_after_session_id = Some("does-not-exist".to_string());
            state.save().unwrap();
            let mut app = mk_app(state);
            app.dispatch_waiting_to_implement();
            assert_eq!(app.state.current_stage, Stage::RepoStateUpdateRunning);
        });
    }

    #[test]
    fn maybe_auto_launch_leaves_waiting_to_implement_for_shell_scheduler() {
        with_temp_root(|| {
            let session_id = "20260511-090000-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::WaitingToImplement;
            state.save().unwrap();
            let mut app = mk_app(state);
            app.run_launched = false;

            app.maybe_auto_launch();

            assert_eq!(app.state.current_stage, Stage::WaitingToImplement);
            let reloaded = SessionState::load(session_id).unwrap();
            assert_eq!(reloaded.current_stage, Stage::WaitingToImplement);
        });
    }

    #[test]
    fn inputs_include_all_earlier_done_when_baseline_missing() {
        with_temp_root(|| {
            save_done_session("20260511-080000-000000001");
            save_done_session("20260511-081000-000000001");
            let session_id = "20260511-090000-000000001";
            let mut state = SessionState::new(session_id.to_string());
            state.current_stage = Stage::RepoStateUpdateRunning;
            // Recorded baseline references a session that never existed; the
            // stage must still see every earlier Done session.
            state.planned_after_session_id = Some("nonexistent".to_string());
            state.save().unwrap();
            let app = mk_app(state);
            let (current_baseline, newly) = app.compute_repo_state_update_inputs();
            // Current baseline is the newest earlier Done session.
            assert_eq!(
                current_baseline.as_deref(),
                Some("20260511-081000-000000001")
            );
            // "nonexistent" sorts after "20260511-..." so neither entry is
            // filtered out by the "<= recorded" cutoff. Both must appear.
            let ids: Vec<&str> = newly.iter().map(|s| s.session_id.as_str()).collect();
            assert_eq!(
                ids,
                vec!["20260511-080000-000000001", "20260511-081000-000000001"]
            );
        });
    }
}
