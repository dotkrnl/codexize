// Public prompt-builder functions for every agent stage. Long prompt bodies
// live in `src/app/prompts/*.md`; this file stays focused on path binding and
// prompt-specific dynamic blocks.
use indoc::formatdoc;
use std::path::{Component, Path, PathBuf};

macro_rules! prompt {
    ($template:expr $(, $name:ident = $value:expr )* $(,)?) => {
        {
            // External prompt files include literal TOML examples with braces, so only declared tokens are substituted.
            let mut rendered = $template.to_owned();
            $(rendered = rendered.replace(concat!("{", stringify!($name), "}"), &$value.to_string());)*
            rendered
        }
    };
}

struct PromptCtx {
    project_doc_instr: String,
}

impl PromptCtx {
    fn new() -> Self {
        Self {
            project_doc_instr: project_doc_instr(),
        }
    }

    fn path(&self, path: impl AsRef<Path>) -> String {
        agent_path(path.as_ref())
    }

    fn live_summary_instruction(&self, path: impl AsRef<Path>) -> String {
        formatdoc!(
            "\n\nImmediately create {path}, then every 2–3 min — including across long tool calls and while sub-agents are running — and on each sub-goal change, overwrite it with `<short title ≤5 words, varies as focus shifts> | <one-paragraph summary of progress + next action>`. If you delegate to a sub-agent or any other long-running call, poll its progress periodically and rewrite the live summary on each poll; do not \"fire and wait\" on a sub-agent for more than 2–3 min without an update. Keep this file current until you exit. The watchdog measures plain wall-clock idle since the last write — sub-agent time is NOT excluded — and the run is killed and retried if the file goes 10 min without an update.\n",
            path = self.path(path)
        )
    }

    fn live_summary_instruction_interactive(&self, path: impl AsRef<Path>) -> String {
        formatdoc!(
            "\n\nImmediately create {path}, then every 2–3 min — including across long tool calls and while sub-agents are running — overwrite it with `<short title> | <one-paragraph summary>` so the operator can follow along. If you delegate to a sub-agent or any other long-running call, poll its progress periodically and rewrite the live summary on each poll. Keep this file current until you exit.\n",
            path = self.path(path)
        )
    }
}

fn resolved_agent_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = resolved.pop();
            }
            Component::Normal(part) => resolved.push(part),
        }
    }
    resolved
}

fn agent_path(path: &Path) -> String {
    resolved_agent_path(path).display().to_string()
}

fn join_ids(ids: &[u32], empty: &str) -> String {
    if ids.is_empty() {
        empty.to_string()
    } else {
        ids.iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Prepended to every agent prompt. Surfaces project-specific guidance
/// (CLAUDE.md / AGENTS.md) before the agent acts. Returns an empty string
/// if neither file is present in the cwd, to avoid wasting prompt context
/// directing the agent to read files that don't exist.
pub(crate) fn project_doc_instr() -> String {
    let claude_path = Path::new("CLAUDE.md");
    let agents_path = Path::new("AGENTS.md");
    let docs = match (claude_path.exists(), agents_path.exists()) {
        (true, true) => format!(
            "{} and {}",
            agent_path(claude_path),
            agent_path(agents_path)
        ),
        (true, false) => agent_path(claude_path),
        (false, true) => agent_path(agents_path),
        (false, false) => return String::new(),
    };
    format!("Read {docs} in the repo first and follow those directions carefully.\n\n")
}

#[cfg(test)]
pub(crate) fn live_summary_instruction(path: &Path) -> String {
    PromptCtx::new().live_summary_instruction(path)
}

#[cfg(test)]
pub(crate) fn live_summary_instruction_interactive(path: &Path) -> String {
    PromptCtx::new().live_summary_instruction_interactive(path)
}

pub(crate) fn spec_review_prompt(
    spec_path: &str,
    review_path: &str,
    live_summary_path: &str,
) -> String {
    let ctx = PromptCtx::new();
    prompt!(
        include_str!("prompts/spec_review.md"),
        project_doc_instr = ctx.project_doc_instr,
        spec_path = ctx.path(spec_path),
        review_path = ctx.path(review_path),
        instr = ctx.live_summary_instruction(live_summary_path),
    )
}

pub(crate) fn plan_review_prompt(
    spec_path: &str,
    plan_path: &str,
    review_path: &str,
    round: u32,
    live_summary_path: &str,
) -> String {
    let ctx = PromptCtx::new();
    let review_path = ctx.path(review_path);
    let review_dir = Path::new(&review_path)
        .parent()
        .map(|p| p.display().to_string())
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
    prompt!(
        include_str!("prompts/plan_review.md"),
        project_doc_instr = ctx.project_doc_instr,
        spec_path = ctx.path(spec_path),
        plan_path = ctx.path(plan_path),
        review_path = review_path,
        prior_block = prior_block,
        instr = ctx.live_summary_instruction(live_summary_path),
    )
}

/// Builds the brainstorm-stage prompt. The prompt embeds the workflow inline
/// and explicitly refuses to invoke skills.
pub(crate) fn brainstorm_prompt(
    idea: &str,
    spec_path: &str,
    summary_path: &str,
    live_summary_path: &str,
    yolo: bool,
) -> String {
    let ctx = PromptCtx::new();
    let summary_path = ctx.path(summary_path);
    let skip_proposal_path = Path::new(&summary_path)
        .parent()
        .map(|dir| dir.join("skip_proposal.toml"))
        .unwrap_or_else(|| resolved_agent_path(Path::new("skip_proposal.toml")));
    let instr = if yolo {
        ctx.live_summary_instruction(live_summary_path)
    } else {
        ctx.live_summary_instruction_interactive(live_summary_path)
    };
    prompt!(
        if yolo {
            include_str!("prompts/brainstorm_yolo.md")
        } else {
            include_str!("prompts/brainstorm_interactive.md")
        },
        project_doc_instr = ctx.project_doc_instr,
        idea = idea,
        spec_path = ctx.path(spec_path),
        summary_path = summary_path,
        skip_proposal_path = ctx.path(skip_proposal_path),
        instr = instr,
    )
}

pub(crate) fn planning_prompt(
    spec_path: &Path,
    review_paths: &[PathBuf],
    plan_path: &Path,
    live_summary_path: &Path,
    yolo: bool,
) -> String {
    let ctx = PromptCtx::new();
    let reviews_block = if review_paths.is_empty() {
        "(no spec reviews available — work from the spec alone)".to_string()
    } else {
        review_paths
            .iter()
            .enumerate()
            .map(|(i, p)| format!("  - review {}: {}", i + 1, ctx.path(p)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let instr = if yolo {
        ctx.live_summary_instruction(live_summary_path)
    } else {
        ctx.live_summary_instruction_interactive(live_summary_path)
    };
    prompt!(
        if yolo {
            include_str!("prompts/planning_yolo.md")
        } else {
            include_str!("prompts/planning_interactive.md")
        },
        project_doc_instr = ctx.project_doc_instr,
        spec = ctx.path(spec_path),
        reviews = reviews_block,
        plan = ctx.path(plan_path),
        instr = instr,
    )
}

pub(crate) fn sharding_prompt(
    spec_path: &Path,
    plan_path: &Path,
    tasks_path: &Path,
    live_summary_path: &Path,
) -> String {
    let ctx = PromptCtx::new();
    prompt!(
        include_str!("prompts/sharding.md"),
        project_doc_instr = ctx.project_doc_instr,
        spec = ctx.path(spec_path),
        plan = ctx.path(plan_path),
        tasks = ctx.path(tasks_path),
        instr = ctx.live_summary_instruction(live_summary_path),
    )
}

pub(crate) fn final_validation_prompt(
    idea_text: &str,
    spec_text: &str,
    verdict_path: &Path,
    live_summary_path: &Path,
    simplification_path: Option<&Path>,
) -> String {
    let ctx = PromptCtx::new();
    // The validator may inspect the simplifier self-report, but its verdict
    // remains independent.
    let simplification_block = match simplification_path {
        Some(path) if path.exists() => format!(
            "\nSimplification context (advisory only — the simplifier's self-report; do not let it override your independent judgment):\n  {}\n",
            ctx.path(path)
        ),
        _ => String::new(),
    };
    prompt!(
        include_str!("prompts/final_validation.md"),
        project_doc_instr = ctx.project_doc_instr,
        idea_text = idea_text,
        spec_text = spec_text,
        verdict = ctx.path(verdict_path),
        live_summary = ctx.path(live_summary_path),
        simplification_block = simplification_block,
        instr = ctx.live_summary_instruction(live_summary_path),
    )
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
    let ctx = PromptCtx::new();
    let instr = if interactive {
        ctx.live_summary_instruction_interactive(live_summary_path)
    } else {
        ctx.live_summary_instruction(live_summary_path)
    };
    prompt!(
        if interactive {
            include_str!("prompts/recovery_interactive.md")
        } else {
            include_str!("prompts/recovery_noninteractive.md")
        },
        project_doc_instr = ctx.project_doc_instr,
        spec = ctx.path(spec_path),
        plan = ctx.path(plan_path),
        tasks = ctx.path(tasks_path),
        recovery = ctx.path(recovery_path),
        trigger_task = trigger_task_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "(none)".to_string()),
        trigger_summary = trigger_summary.unwrap_or("(none recorded)"),
        completed = join_ids(completed_task_ids, "(none)"),
        started = join_ids(started_task_ids, "(none)"),
        instr = instr,
    )
}

pub(crate) fn recovery_plan_review_prompt(
    spec_path: &Path,
    plan_path: &Path,
    triggering_review_path: &Path,
    recovery_path: &Path,
    live_summary_path: &Path,
    plan_review_output_path: &Path,
) -> String {
    let ctx = PromptCtx::new();
    prompt!(
        include_str!("prompts/recovery_plan_review.md"),
        project_doc_instr = ctx.project_doc_instr,
        spec = ctx.path(spec_path),
        plan = ctx.path(plan_path),
        review = ctx.path(triggering_review_path),
        recovery = ctx.path(recovery_path),
        output = ctx.path(plan_review_output_path),
        instr = ctx.live_summary_instruction(live_summary_path),
    )
}

pub(crate) fn recovery_sharding_prompt(
    spec_path: &Path,
    plan_path: &Path,
    live_summary_path: &Path,
    tasks_output_path: &Path,
    completed_ids: &[u32],
    id_floor: u32,
) -> String {
    let ctx = PromptCtx::new();
    prompt!(
        include_str!("prompts/recovery_sharding.md"),
        project_doc_instr = ctx.project_doc_instr,
        spec = ctx.path(spec_path),
        plan = ctx.path(plan_path),
        completed = join_ids(completed_ids, "none"),
        id_floor = id_floor,
        output = ctx.path(tasks_output_path),
        instr = ctx.live_summary_instruction(live_summary_path),
    )
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
    let ctx = PromptCtx::new();
    let prev_review = if round > 1 {
        let p = session_dir
            .join("rounds")
            .join(format!("{:03}", round - 1))
            .join("review.toml");
        if p.exists() {
            format!(
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
    prompt!(
        include_str!("prompts/coder.md"),
        project_doc_instr = ctx.project_doc_instr,
        task_id = task_id,
        round = round,
        task = ctx.path(task_file),
        spec = ctx.path(session_dir.join("artifacts/spec.md")),
        plan = ctx.path(session_dir.join("artifacts/plan.md")),
        coder_summary = ctx.path(
            session_dir
                .join("rounds")
                .join(format!("{round:03}"))
                .join("coder_summary.toml")
        ),
        prev_review = prev_review,
        refine_block = refine_block,
        resume_hint = if resume {
            "\nThis is a RESUME of a previous coding session on the same task — pick up where\nyou left off, honour the reviewer feedback above, and finish the work.\n"
        } else {
            ""
        },
        instr = ctx.live_summary_instruction(live_summary_path),
    )
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
    let ctx = PromptCtx::new();
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
        format!(
            "  Coder summary: {}\n  Coder rebuttal (round {}):\n    Read it before your verdict.\n    If the coder rebuts prior feedback convincingly, do not repeat that item as blocking feedback.\n    Rebuttal entries use the prefix \"[Round N, Item M]\".\n",
            ctx.path(path),
            inputs.round
        )
    });
    prompt!(
        include_str!("prompts/reviewer.md"),
        project_doc_instr = ctx.project_doc_instr,
        task_id = inputs.task_id,
        round = inputs.round,
        task = ctx.path(inputs.task_file),
        spec = ctx.path(inputs.session_dir.join("artifacts/spec.md")),
        plan = ctx.path(inputs.session_dir.join("artifacts/plan.md")),
        review_scope = ctx.path(inputs.review_scope_file),
        prior_reviews = prior_reviews,
        coder_summary_section = coder_summary_section,
        review_scope_text = "  4. Check correctness, missing edge cases, broken contracts, bad error\n     handling, test gaps. Uncommitted working-tree changes are NOT in scope —\n     review only `base..HEAD`.\n",
        review = ctx.path(inputs.review_file),
        instr = ctx.live_summary_instruction(inputs.live_summary_path),
    )
}

pub(crate) fn simplifier_prompt(
    session_dir: &Path,
    review_scope_file: &Path,
    simplification_path: &Path,
    live_summary_path: &Path,
) -> String {
    let ctx = PromptCtx::new();
    prompt!(
        include_str!("prompts/simplifier.md"),
        project_doc_instr = ctx.project_doc_instr,
        spec_path = ctx.path(session_dir.join("artifacts/spec.md")),
        plan_path = ctx.path(session_dir.join("artifacts/plan.md")),
        review_scope_path = ctx.path(review_scope_file),
        simplification_path = ctx.path(simplification_path),
        instr = ctx.live_summary_instruction(live_summary_path),
    )
}
