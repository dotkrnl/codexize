use crate::logic::pipeline::Stage;
use crate::state::RunRecord;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryTarget {
    Task(u32),
    Stage(&'static str),
}
pub fn retry_stage_for_stage(stage: &str) -> Option<Stage> {
    match stage {
        "brainstorm" => Some(Stage::BrainstormRunning),
        "spec-review" => Some(Stage::SpecReviewRunning),
        "planning" => Some(Stage::PlanningRunning),
        "plan-review" => Some(Stage::PlanReviewRunning),
        // Spec §Data model line 96: manual retry of sharding must pause in
        // WaitingToImplement so the scheduler re-verifies baseline state.
        "sharding" => Some(Stage::WaitingToImplement),
        "repo-state-update" => Some(Stage::RepoStateUpdateRunning),
        _ => None,
    }
}
pub fn retry_target_for_run(run: &RunRecord) -> Option<RetryTarget> {
    run.task_id
        .map(RetryTarget::Task)
        .or_else(|| stage_str(&run.stage).map(RetryTarget::Stage))
}
pub fn stage_str(stage: &str) -> Option<&'static str> {
    match stage {
        "brainstorm" => Some("brainstorm"),
        "spec-review" => Some("spec-review"),
        "planning" => Some("planning"),
        "plan-review" => Some("plan-review"),
        "sharding" => Some("sharding"),
        "repo-state-update" => Some("repo-state-update"),
        _ => None,
    }
}
