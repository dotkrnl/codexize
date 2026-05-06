/// Per-stage definition of which artifacts are passed by pointer and which are
/// expected as output. Used by the orchestrator to validate agent runs.
#[derive(Debug, Clone)]
pub struct StageIO {
    pub stage: &'static str,
    pub pointer_artifacts: &'static [&'static str],
    pub writes: &'static [&'static str],
}

pub const BRAINSTORM_IO: StageIO = StageIO {
    stage: "brainstorm",
    pointer_artifacts: &["artifacts/live_summary.txt"],
    writes: &["artifacts/spec.md"],
};

pub const SPEC_REVIEWER_IO: StageIO = StageIO {
    stage: "spec-reviewer",
    pointer_artifacts: &["artifacts/spec.md", "artifacts/live_summary.txt"],
    writes: &["artifacts/spec_review.toml"],
};

pub const PLANNER_IO: StageIO = StageIO {
    stage: "planner",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "artifacts/spec_review.toml",
        "artifacts/live_summary.txt",
    ],
    writes: &["artifacts/plan.md"],
};

pub const PLAN_REVIEWER_IO: StageIO = StageIO {
    stage: "plan-reviewer",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "artifacts/plan.md",
        "artifacts/live_summary.txt",
    ],
    writes: &["artifacts/plan_review.toml"],
};

pub const SHARDER_IO: StageIO = StageIO {
    stage: "sharder",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "artifacts/plan.md",
        "artifacts/live_summary.txt",
    ],
    writes: &["artifacts/tasks.toml"],
};

pub const CODER_IO: StageIO = StageIO {
    stage: "coder",
    pointer_artifacts: &[
        "rounds/{round}/task.toml",
        "artifacts/spec.md",
        "artifacts/plan.md",
        "rounds/{round}/review.toml",
        "artifacts/live_summary.txt",
    ],
    writes: &["rounds/{round}/coder_summary.toml"],
};

pub const REVIEWER_IO: StageIO = StageIO {
    stage: "reviewer",
    pointer_artifacts: &[
        "rounds/{round}/task.toml",
        "rounds/{round}/review_scope.toml",
        "rounds/{round}/coder_summary.toml",
        "artifacts/spec.md",
        "artifacts/plan.md",
        "rounds/*/review.toml",
        "artifacts/live_summary.txt",
    ],
    writes: &["rounds/{round}/review.toml"],
};

pub const RECOVERY_IO: StageIO = StageIO {
    stage: "recovery",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "artifacts/plan.md",
        "artifacts/tasks.toml",
        "rounds/{round}/review.toml",
        "artifacts/live_summary.txt",
    ],
    writes: &[
        "artifacts/spec.md",
        "artifacts/plan.md",
        "artifacts/tasks.toml",
        "rounds/{round}/recovery.toml",
    ],
};

/// Recovery-mode plan review: verifies the recovered spec/plan addresses the
/// triggering review before sharding runs.
pub const RECOVERY_PLAN_REVIEWER_IO: StageIO = StageIO {
    stage: "plan-reviewer",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "artifacts/plan.md",
        "rounds/{round}/review.toml",
        "rounds/{round}/recovery.toml",
        "artifacts/live_summary.txt",
    ],
    writes: &["artifacts/plan_review.toml"],
};

/// Behavior-preserving cleanup pass that fires on every normal entry into
/// `FinalValidation`. The simplifier reads spec/plan, the round's review
/// scope (for `base_sha..HEAD`), and the live summary, and writes its
/// verdict to `rounds/{round}/simplification.toml`.
pub const SIMPLIFIER_IO: StageIO = StageIO {
    stage: "simplifier",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "rounds/{round}/review_scope.toml",
        "artifacts/live_summary.txt",
    ],
    writes: &["rounds/{round}/simplification.toml"],
};

/// Recovery-mode sharding: regenerates the task queue from the recovered
/// spec/plan while preserving completed task history.
pub const RECOVERY_SHARDER_IO: StageIO = StageIO {
    stage: "sharder",
    pointer_artifacts: &[
        "artifacts/spec.md",
        "artifacts/plan.md",
        "artifacts/live_summary.txt",
    ],
    writes: &["artifacts/tasks.toml"],
};

pub fn stage_io(stage: &str) -> Option<&'static StageIO> {
    stage_io_with_mode(stage, None)
}

/// Lookup StageIO by stage name and optional mode. The `"recovery"` mode
/// selects the recovery-specific variants for `plan-reviewer` and `sharder`.
pub fn stage_io_with_mode(stage: &str, mode: Option<&str>) -> Option<&'static StageIO> {
    match (stage, mode) {
        ("plan-reviewer", Some("recovery")) => Some(&RECOVERY_PLAN_REVIEWER_IO),
        ("sharder", Some("recovery")) => Some(&RECOVERY_SHARDER_IO),
        ("brainstorm", _) => Some(&BRAINSTORM_IO),
        ("spec-reviewer", _) => Some(&SPEC_REVIEWER_IO),
        ("planner", _) => Some(&PLANNER_IO),
        ("plan-reviewer", _) => Some(&PLAN_REVIEWER_IO),
        ("sharder", _) => Some(&SHARDER_IO),
        ("coder", _) => Some(&CODER_IO),
        ("reviewer", _) => Some(&REVIEWER_IO),
        ("simplifier", _) => Some(&SIMPLIFIER_IO),
        ("recovery", _) => Some(&RECOVERY_IO),
        _ => None,
    }
}

#[cfg(test)]
#[path = "stage_io_tests.rs"]
mod tests;
