// yolo_exit.rs
use super::*;
use crate::{
    artifacts::ArtifactKind,
    state::{
        self as session_state, LaunchModes, Phase, PipelineItem, PipelineItemStatus, RunRecord,
    },
};
use std::time::Duration;
impl App {
    pub(super) fn toggle_yolo_mode(&mut self, source: &str) {
        self.set_yolo_mode(!self.state.modes.yolo, source);
    }

    pub(super) fn set_yolo_mode(&mut self, value: bool, source: &str) {
        self.state.modes.yolo = value;
        if let Err(err) = self.state.save() {
            self.state.agent_error = Some(format!("failed to save yolo mode: {err:#}"));
            return;
        }
        let _ = self.state.log_event(format!(
            "mode_toggled: mode=yolo value={value} source={source}"
        ));
        self.pending_yolo_toggle_gate = if value {
            self.live_yolo_paused_gate()
        } else {
            None
        };
        let status = if value {
            "yolo: ON  (next agent launch will auto-approve gates)"
        } else {
            "yolo: OFF"
        };
        self.push_status(
            status.to_string(),
            status_line::Severity::Info,
            Duration::from_secs(5),
        );
    }

    pub(super) fn live_yolo_paused_gate(&self) -> Option<&'static str> {
        match self.state.current_phase {
            Phase::SpecReviewPaused => Some("spec_approval"),
            Phase::PlanReviewPaused => Some("plan_approval"),
            _ => None,
        }
    }

    pub(super) fn log_yolo_auto_approved(&mut self, gate: &'static str) {
        let _ = self
            .state
            .log_event(format!("yolo_auto_approved: gate={gate}"));
    }

    pub(super) fn queue_recovery_sharding_pipeline_item(&mut self, round: u32) {
        self.state.builder.push_pipeline_item(PipelineItem {
            id: 0,
            stage: "sharding".to_string(),
            task_id: None,
            round: Some(round),
            status: PipelineItemStatus::Pending,
            title: Some("Recovery sharding".to_string()),
            mode: Some("recovery".to_string()),
            trigger: None,
            interactive: Some(false),
        });
    }

    pub(super) fn yolo_exit_stage_artifacts(&self, run: &RunRecord) -> Vec<std::path::PathBuf> {
        let session_dir = session_state::session_dir(&self.state.session_id);
        let artifacts = session_dir.join("artifacts");
        let round_dir = session_dir.join("rounds").join(format!("{:03}", run.round));
        match run.stage.as_str() {
            "brainstorm" => vec![
                artifacts.join("spec.md"),
                artifacts.join(ArtifactKind::SessionSummary.filename()),
            ],
            "spec-review" => vec![artifacts.join(format!("spec-review-{}.md", run.round))],
            "planning" => vec![artifacts.join("plan.md")],
            "sharding" => vec![artifacts.join("tasks.toml")],
            "coder" => vec![round_dir.join("coder_summary.toml")],
            "reviewer" => vec![round_dir.join("review.toml")],
            "recovery" => vec![round_dir.join("recovery.toml")],
            _ => Vec::new(),
        }
    }

    pub(super) fn yolo_exit_snapshot(&self, run: &RunRecord) -> YoloExitSnapshot {
        YoloExitSnapshot {
            live_summary: Self::observed_path_state(&self.live_summary_path_for(run)),
            run_status: Self::observed_path_state(&self.run_status_path(run)),
            finish_stamp: Self::observed_path_state(&self.finish_stamp_path_for(run)),
            stage_artifacts: self
                .yolo_exit_stage_artifacts(run)
                .into_iter()
                .map(|path| Self::observed_path_state(&path))
                .collect(),
        }
    }

    pub(super) fn prime_yolo_exit_tracking(&mut self, run: &RunRecord) {
        self.yolo_exit_issued.remove(&run.id);
        self.yolo_exit_observations.insert(
            run.id,
            YoloExitObservation {
                snapshot: self.yolo_exit_snapshot(run),
                saw_new_update: false,
            },
        );
    }

    pub(super) fn yolo_exit_has_new_observable_update(&mut self, run: &RunRecord) -> bool {
        let snapshot = self.yolo_exit_snapshot(run);
        let observation = self
            .yolo_exit_observations
            .entry(run.id)
            .or_insert_with(|| YoloExitObservation {
                snapshot: snapshot.clone(),
                saw_new_update: false,
            });
        if observation.snapshot != snapshot {
            observation.snapshot = snapshot;
            observation.saw_new_update = true;
        }
        observation.saw_new_update
    }

    pub(super) fn yolo_exit_gate_name(stage: &str) -> String {
        // The spec leaves per-stage `/exit` event names open; keep them aligned
        // with the existing underscore-delimited yolo audit gates.
        format!("{}_exit", stage.replace('-', "_"))
    }

    pub(super) fn maybe_yolo_auto_resolve(&mut self) {
        if !self.state.modes.yolo {
            return;
        }
        match self.state.current_phase {
            Phase::SpecReviewPaused => {
                self.auto_approve_spec_review_pause("spec_approval");
            }
            Phase::PlanReviewPaused => {
                self.auto_approve_plan_review_pause("plan_approval");
            }
            _ => {}
        }
    }

    pub(super) fn auto_approve_spec_review_pause(&mut self, gate: &'static str) {
        self.log_yolo_auto_approved(gate);
        if self.pending_yolo_toggle_gate == Some(gate) {
            let _ = self
                .state
                .log_event(format!("yolo_toggled_resolved_gate={gate}"));
            self.pending_yolo_toggle_gate = None;
        }
        self.state.agent_error = None;
        let _ = self.transition_to_phase(Phase::PlanningRunning);
    }

    pub(super) fn auto_approve_plan_review_pause(&mut self, gate: &'static str) {
        self.log_yolo_auto_approved(gate);
        if self.pending_yolo_toggle_gate == Some(gate) {
            let _ = self
                .state
                .log_event(format!("yolo_toggled_resolved_gate={gate}"));
            self.pending_yolo_toggle_gate = None;
        }
        self.state.agent_error = None;
        self.queue_view_of_current_artifact("plan.md");
        let _ = self.transition_to_phase(Phase::ShardingRunning);
    }

    pub(super) fn record_dirty_worktree_yolo_gate(&mut self, dirty: bool, modes: LaunchModes) {
        if dirty && modes.yolo {
            self.log_yolo_auto_approved("dirty_worktree");
        }
    }

    pub(super) fn maybe_issue_yolo_exit(&mut self, run: &RunRecord) {
        if !run.modes.yolo || self.yolo_exit_issued.contains(&run.id) {
            return;
        }
        if !self.yolo_exit_has_new_observable_update(run) {
            return;
        }
        if !self.yolo_exit_artifact_ready(run) {
            return;
        }
        self.yolo_exit_issued.insert(run.id);
        let gate = Self::yolo_exit_gate_name(&run.stage);
        let _ = self
            .state
            .log_event(format!("yolo_auto_approved: gate={gate}"));
        #[cfg(not(test))]
        {
            let _ = std::process::Command::new("tmux")
                .args(["send-keys", "-t", &run.window_name, "/exit", "Enter"])
                .output();
        }
    }

    pub(super) fn yolo_exit_artifact_ready(&self, run: &RunRecord) -> bool {
        let paths = self.yolo_exit_stage_artifacts(run);
        !paths.is_empty() && paths.iter().all(|path| Self::artifact_present(path))
    }
}
