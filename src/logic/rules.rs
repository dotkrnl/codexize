use crate::logic::pipeline::{Phase, RunRecord};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetryTarget {
    Task(u32),
    Stage(&'static str),
}

pub fn retry_phase_for_stage(stage: &str) -> Option<Phase> {
    match stage {
        "brainstorm" => Some(Phase::BrainstormRunning),
        "spec-review" => Some(Phase::SpecReviewRunning),
        "planning" => Some(Phase::PlanningRunning),
        "plan-review" => Some(Phase::PlanReviewRunning),
        "sharding" => Some(Phase::ShardingRunning),
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
        _ => None,
    }
}
