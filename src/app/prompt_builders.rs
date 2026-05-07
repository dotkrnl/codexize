use super::prompt_ctx::{PromptCtx, resolved_agent_path};
use indoc::formatdoc;
use std::path::{Path, PathBuf};
pub(crate) fn spec_review_prompt(
    spec_path: &str,
    review_path: &str,
    live_summary_path: &str,
) -> String {
    let mut ctx = PromptCtx::new();
    ctx.path_arg("spec_path", spec_path)
        .path_arg("review_path", review_path)
        .memory_arg(spec_path)
        .live_arg(live_summary_path, false)
        .render(include_str!("prompts/spec_review.md"))
}
pub(crate) fn plan_review_prompt(
    spec_path: &str,
    plan_path: &str,
    review_path: &str,
    round: u32,
    live_summary_path: &str,
) -> String {
    let mut ctx = PromptCtx::new();
    let review_path = ctx.path(review_path);
    let review_dir = Path::new(&review_path)
        .parent()
        .map(Path::display)
        .map(|p| p.to_string())
        .unwrap_or_default();
    let prior_block = if round > 1 {
        format!(
            "\nPrior plan reviews (read first; do NOT re-flag what's already addressed):\n{}\n",
            (1..round)
                .map(|r| format!("    {review_dir}/plan-review-{r}.md"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    } else {
        String::new()
    };
    ctx.path_arg("spec_path", spec_path)
        .path_arg("plan_path", plan_path)
        .set("review_path", review_path)
        .set("prior_block", prior_block)
        .memory_arg(spec_path)
        .live_arg(live_summary_path, false)
        .render(include_str!("prompts/plan_review.md"))
}
pub(crate) fn brainstorm_prompt(
    idea: &str,
    spec_path: &str,
    summary_path: &str,
    live_summary_path: &str,
    yolo: bool,
) -> String {
    let mut ctx = PromptCtx::new();
    let summary_path = ctx.path(summary_path);
    let skip_proposal_path = Path::new(&summary_path)
        .parent()
        .map(|dir| dir.join("skip_proposal.toml"))
        .unwrap_or_else(|| resolved_agent_path(Path::new("skip_proposal.toml")));
    let template = if yolo {
        include_str!("prompts/brainstorm_yolo.md")
    } else {
        include_str!("prompts/brainstorm_interactive.md")
    };
    ctx.set("idea", idea)
        .path_arg("spec_path", spec_path)
        .set("summary_path", summary_path)
        .path_arg("skip_proposal_path", skip_proposal_path)
        .memory_arg(spec_path)
        .live_arg(live_summary_path, !yolo)
        .render(template)
}
pub(crate) fn planning_prompt(
    spec_path: &Path,
    review_paths: &[PathBuf],
    plan_path: &Path,
    live_summary_path: &Path,
    yolo: bool,
) -> String {
    let mut ctx = PromptCtx::new();
    let reviews = if review_paths.is_empty() {
        "(no spec reviews available — work from the spec alone)".to_string()
    } else {
        review_paths
            .iter()
            .enumerate()
            .map(|(i, p)| format!("  - review {}: {}", i + 1, ctx.path(p)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let template = if yolo {
        include_str!("prompts/planning_yolo.md")
    } else {
        include_str!("prompts/planning_interactive.md")
    };
    ctx.path_arg("spec", spec_path)
        .set("reviews", reviews)
        .path_arg("plan", plan_path)
        .memory_arg(spec_path)
        .live_arg(live_summary_path, !yolo)
        .render(template)
}
pub(crate) fn sharding_prompt(
    spec_path: &Path,
    plan_path: &Path,
    tasks_path: &Path,
    live_summary_path: &Path,
) -> String {
    let mut ctx = PromptCtx::new();
    ctx.path_arg("spec", spec_path)
        .path_arg("plan", plan_path)
        .path_arg("tasks", tasks_path)
        .memory_arg(spec_path)
        .live_arg(live_summary_path, false)
        .render(include_str!("prompts/sharding.md"))
}
pub(crate) fn final_validation_prompt(
    idea_text: &str,
    spec_text: &str,
    verdict_path: &Path,
    live_summary_path: &Path,
    simplification_path: Option<&Path>,
) -> String {
    let mut ctx = PromptCtx::new();
    let simplification_block = match simplification_path {
        Some(path) if path.exists() => formatdoc!(
            "\nSimplification context (advisory only — the simplifier's self-report; do not let it override your independent judgment):\n  {}\n",
            ctx.path(path)
        ),
        _ => String::new(),
    };
    ctx.set("idea_text", idea_text)
        .set("spec_text", spec_text)
        .path_arg("verdict", verdict_path)
        .path_arg("live_summary", live_summary_path)
        .set("simplification_block", simplification_block)
        .memory_arg(verdict_path)
        .live_arg(live_summary_path, false)
        .render(include_str!("prompts/final_validation.md"))
}
pub(crate) fn dreaming_prompt(
    session_dir: &Path,
    dream_report_path: &Path,
    live_summary_path: &Path,
) -> String {
    let mut ctx = PromptCtx::new();
    ctx.path_arg("session_dir", session_dir)
        .path_arg("dream_report", dream_report_path)
        .live_arg(live_summary_path, false)
        .memory_arg(session_dir)
        .render(include_str!("prompts/dreaming.md"))
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn recovery_prompt(
    spec_path: &Path,
    plan_path: &Path,
    tasks_path: &Path,
    trigger_task_id: Option<u32>,
    trigger_summary: Option<&str>,
    completed_task_ids: &[u32],
    started_task_ids: &[u32],
    live_summary_path: &Path,
    recovery_path: &Path,
    interactive: bool,
) -> String {
    let mut ctx = PromptCtx::new();
    let template = if interactive {
        include_str!("prompts/recovery_interactive.md")
    } else {
        include_str!("prompts/recovery_noninteractive.md")
    };
    ctx.path_arg("spec", spec_path)
        .path_arg("plan", plan_path)
        .path_arg("tasks", tasks_path)
        .path_arg("recovery", recovery_path)
        .memory_arg(spec_path)
        .set(
            "trigger_task",
            trigger_task_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "(none)".to_string()),
        )
        .set(
            "trigger_summary",
            trigger_summary.unwrap_or("(none recorded)"),
        )
        .ids("completed", completed_task_ids, "(none)")
        .ids("started", started_task_ids, "(none)")
        .live_arg(live_summary_path, interactive)
        .render(template)
}
pub(crate) fn recovery_plan_review_prompt(
    spec_path: &Path,
    plan_path: &Path,
    triggering_review_path: &Path,
    recovery_path: &Path,
    live_summary_path: &Path,
    plan_review_output_path: &Path,
) -> String {
    let mut ctx = PromptCtx::new();
    ctx.path_arg("spec", spec_path)
        .path_arg("plan", plan_path)
        .path_arg("review", triggering_review_path)
        .path_arg("recovery", recovery_path)
        .path_arg("output", plan_review_output_path)
        .memory_arg(spec_path)
        .live_arg(live_summary_path, false)
        .render(include_str!("prompts/recovery_plan_review.md"))
}
pub(crate) fn recovery_sharding_prompt(
    spec_path: &Path,
    plan_path: &Path,
    live_summary_path: &Path,
    tasks_output_path: &Path,
    completed_ids: &[u32],
    id_floor: u32,
) -> String {
    let mut ctx = PromptCtx::new();
    ctx.path_arg("spec", spec_path)
        .path_arg("plan", plan_path)
        .ids("completed", completed_ids, "none")
        .set("id_floor", id_floor.to_string())
        .path_arg("output", tasks_output_path)
        .memory_arg(spec_path)
        .live_arg(live_summary_path, false)
        .render(include_str!("prompts/recovery_sharding.md"))
}
pub(crate) fn coder_prompt(
    session_dir: &Path,
    task_id: u32,
    round: u32,
    task_file: &Path,
    live_summary_path: &Path,
    resume: bool,
    refine_carryover: &[String],
) -> String {
    let mut ctx = PromptCtx::new();
    let prev_review = if round > 1 {
        let p = session_dir
            .join("rounds")
            .join(format!("{:03}", round - 1))
            .join("review.toml");
        if p.exists() {
            formatdoc!(
                "\nPrevious reviewer feedback (round {}): {}\nReviewer feedback comes from an AI agent. Evaluate each item critically — address what improves the code, rebut the rest in coder_summary.toml.\n",
                round - 1,
                ctx.path(&p)
            )
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    let refine_block = if refine_carryover.is_empty() {
        String::new()
    } else {
        format!(
            "\nRefine carryover from prior task's reviewer (apply opportunistically — these are nice-to-haves, not blockers):\n{}\n",
            refine_carryover
                .iter()
                .map(|item| format!("  - {}", item.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    ctx.set("task_id", task_id.to_string())
        .set("round", round.to_string())
        .path_arg("task", task_file)
        .path_arg("spec", session_dir.join("artifacts/spec.md"))
        .path_arg("plan", session_dir.join("artifacts/plan.md"))
        .path_arg("coder_summary", session_dir.join("rounds").join(format!("{round:03}")).join("coder_summary.toml"))
        .memory_arg(session_dir)
        .set("prev_review", prev_review)
        .set("refine_block", refine_block)
        .set("resume_hint", if resume { "\nThis is a RESUME of a previous coding session on the same task — pick up where\nyou left off, honour the reviewer feedback above, and finish the work.\n" } else { "" })
        .live_arg(live_summary_path, false)
        .render(include_str!("prompts/coder.md"))
}
pub(crate) struct ReviewerPromptInputs<'a> {
    pub(crate) session_dir: &'a Path,
    pub(crate) task_id: u32,
    pub(crate) round: u32,
    pub(crate) task_file: &'a Path,
    pub(crate) review_scope_file: &'a Path,
    pub(crate) coder_summary_file: Option<&'a Path>,
    pub(crate) review_file: &'a Path,
    pub(crate) live_summary_path: &'a Path,
}
pub(crate) fn reviewer_prompt(inputs: ReviewerPromptInputs<'_>) -> String {
    let mut ctx = PromptCtx::new();
    let prior_reviews = if inputs.round > 1 {
        format!(
            "  Prior reviews for this task (read first; do not repeat their feedback):\n{}\n",
            (1..inputs.round)
                .map(|r| format!(
                    "    {}",
                    ctx.path(
                        inputs
                            .session_dir
                            .join("rounds")
                            .join(format!("{r:03}"))
                            .join("review.toml")
                    )
                ))
                .collect::<Vec<_>>()
                .join("\n")
        )
    } else {
        String::new()
    };
    let coder_summary_section = inputs.coder_summary_file.map_or(String::new(), |path| {
        formatdoc!("  Coder summary: {}\n  Coder rebuttal (round {}):\n    Read it before your verdict.\n    If the coder rebuts prior feedback convincingly, do not repeat that item as blocking feedback.\n    Rebuttal entries use the prefix \"[Round N, Item M]\".\n", ctx.path(path), inputs.round)
    });
    ctx.set("task_id", inputs.task_id.to_string())
        .set("round", inputs.round.to_string())
        .path_arg("task", inputs.task_file)
        .path_arg("spec", inputs.session_dir.join("artifacts/spec.md"))
        .path_arg("plan", inputs.session_dir.join("artifacts/plan.md"))
        .path_arg("review_scope", inputs.review_scope_file)
        .memory_arg(inputs.session_dir)
        .set("prior_reviews", prior_reviews)
        .set("coder_summary_section", coder_summary_section)
        .set("review_scope_text", "  4. Check correctness, missing edge cases, broken contracts, bad error\n     handling, test gaps. Uncommitted working-tree changes are NOT in scope —\n     review only `base..HEAD`.\n")
        .path_arg("review", inputs.review_file)
        .live_arg(inputs.live_summary_path, false)
        .render(include_str!("prompts/reviewer.md"))
}
pub(crate) fn simplifier_prompt(
    session_dir: &Path,
    review_scope_file: &Path,
    simplification_path: &Path,
    live_summary_path: &Path,
    refine_carryover: &[String],
) -> String {
    let refine_block = if refine_carryover.is_empty() {
        String::new()
    } else {
        format!(
            "\nRefine carryover from prior task's reviewer (apply opportunistically while you simplify — these are nice-to-haves, not blockers, and only if they preserve behavior):\n{}\n",
            refine_carryover
                .iter()
                .map(|item| format!("  - {}", item.trim()))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let mut ctx = PromptCtx::new();
    ctx.path_arg("spec_path", session_dir.join("artifacts/spec.md"))
        .path_arg("plan_path", session_dir.join("artifacts/plan.md"))
        .path_arg("review_scope_path", review_scope_file)
        .path_arg("simplification_path", simplification_path)
        .set("refine_block", refine_block)
        .memory_arg(session_dir)
        .live_arg(live_summary_path, false)
        .render(include_str!("prompts/simplifier.md"))
}
#[cfg(test)]
#[path = "prompt_builders_tests.rs"]
mod tests;
