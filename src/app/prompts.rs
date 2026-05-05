// prompts.rs
//
// Public prompt-builder functions for every agent stage. Long prompt bodies
// live in `src/app/prompts/*.md` and are rendered via static token replacement
// in this module — keep this file thin and reserve it for orchestration-side
// helpers and the public function signatures.
use crate::{
    adapters::EffortLevel,
    artifacts::ReviewScopeArtifact,
    state::{self as session_state},
    tasks,
};
use anyhow::Context;
use std::path::{Component, Path, PathBuf};

const LIVE_SUMMARY_TEMPLATE: &str = include_str!("prompts/live_summary.md");
const LIVE_SUMMARY_INTERACTIVE_TEMPLATE: &str = include_str!("prompts/live_summary_interactive.md");
const SPEC_REVIEW_TEMPLATE: &str = include_str!("prompts/spec_review.md");
const PLAN_REVIEW_TEMPLATE: &str = include_str!("prompts/plan_review.md");
const BRAINSTORM_INTERACTIVE_TEMPLATE: &str = include_str!("prompts/brainstorm_interactive.md");
const BRAINSTORM_YOLO_TEMPLATE: &str = include_str!("prompts/brainstorm_yolo.md");
const PLANNING_INTERACTIVE_TEMPLATE: &str = include_str!("prompts/planning_interactive.md");
const PLANNING_YOLO_TEMPLATE: &str = include_str!("prompts/planning_yolo.md");
const SHARDING_TEMPLATE: &str = include_str!("prompts/sharding.md");
const FINAL_VALIDATION_TEMPLATE: &str = include_str!("prompts/final_validation.md");
const CODER_TEMPLATE: &str = include_str!("prompts/coder.md");
const REVIEWER_TEMPLATE: &str = include_str!("prompts/reviewer.md");
const SIMPLIFIER_TEMPLATE: &str = include_str!("prompts/simplifier.md");
const RECOVERY_INTERACTIVE_TEMPLATE: &str = include_str!("prompts/recovery_interactive.md");
const RECOVERY_NONINTERACTIVE_TEMPLATE: &str = include_str!("prompts/recovery_noninteractive.md");
const RECOVERY_PLAN_REVIEW_TEMPLATE: &str = include_str!("prompts/recovery_plan_review.md");
const RECOVERY_SHARDING_TEMPLATE: &str = include_str!("prompts/recovery_sharding.md");

macro_rules! prompt_template {
    ($template:expr $(, $name:ident = $value:expr )* $(,)?) => {{
        let mut rendered = $template.to_owned();
        $(
            rendered = rendered.replace(concat!("{", stringify!($name), "}"), $value);
        )*
        rendered
    }};
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

    fn path(&self, path: &Path) -> String {
        agent_path(path)
    }

    fn project_doc_instr(&self) -> &str {
        &self.project_doc_instr
    }

    fn live_summary_instruction(&self, path: &Path) -> String {
        let path = self.path(path);
        prompt_template!(LIVE_SUMMARY_TEMPLATE, path = &path)
    }

    fn live_summary_instruction_interactive(&self, path: &Path) -> String {
        let path = self.path(path);
        prompt_template!(LIVE_SUMMARY_INTERACTIVE_TEMPLATE, path = &path)
    }
}
pub(crate) fn cancel_run_label(base: &str) {
    crate::runner::cancel_run_labels_matching(base);
}

pub(crate) fn restore_artifacts(pairs: &[(&std::path::Path, &std::path::Path)]) {
    for (backup, target) in pairs {
        if backup.exists() {
            let _ = std::fs::copy(backup, target);
        }
    }
}

pub(crate) fn task_toml_for(session_dir: &std::path::Path, task_id: u32) -> anyhow::Result<String> {
    use anyhow::Context;
    let tasks_path = session_dir.join("artifacts").join("tasks.toml");
    let parsed = tasks::validate(&tasks_path).context("load tasks.toml")?;
    let task = parsed
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .ok_or_else(|| anyhow::anyhow!("task id {task_id} not found"))?;
    toml::to_string_pretty(task).context("serialize task.toml")
}

pub(crate) fn task_effort_for(session_dir: &std::path::Path, task_id: u32) -> EffortLevel {
    let tasks_path = session_dir.join("artifacts").join("tasks.toml");
    let Ok(parsed) = tasks::validate(&tasks_path) else {
        // Preserve the existing launch fallback when task metadata is unavailable.
        return EffortLevel::Normal;
    };
    parsed
        .tasks
        .iter()
        .find(|task| task.id == task_id && task.tough)
        .map(|_| EffortLevel::Tough)
        .unwrap_or_default()
}

pub(crate) fn assigned_revise_task_ids(
    builder: &session_state::BuilderState,
    count: usize,
) -> Vec<u32> {
    let mut ids = Vec::with_capacity(count);
    for next_id in builder.max_task_id() + 1..builder.max_task_id() + 1 + count as u32 {
        ids.push(next_id);
    }
    ids
}

pub(crate) fn rewrite_tasks_for_revise(
    session_dir: &std::path::Path,
    current_task_id: u32,
    new_tasks: &[tasks::Task],
    assigned_ids: &[u32],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        new_tasks.len() == assigned_ids.len(),
        "new task count does not match assigned id count"
    );
    let tasks_path = session_dir.join("artifacts").join("tasks.toml");
    let parsed = tasks::validate(&tasks_path).context("load tasks.toml before revise")?;
    let Some(current_idx) = parsed
        .tasks
        .iter()
        .position(|task| task.id == current_task_id)
    else {
        anyhow::bail!("task id {current_task_id} not found in tasks.toml");
    };

    let mut rewritten = Vec::with_capacity(parsed.tasks.len() + new_tasks.len());
    rewritten.extend(parsed.tasks[..current_idx].iter().cloned());

    for (task, id) in new_tasks.iter().zip(assigned_ids.iter().copied()) {
        let mut inserted = task.clone();
        inserted.id = id;
        rewritten.push(inserted);
    }

    let next_pending_id = assigned_ids
        .iter()
        .copied()
        .max()
        .unwrap_or_else(|| parsed.tasks.iter().map(|task| task.id).max().unwrap_or(0));
    for (next_pending_id, task) in
        ((next_pending_id + 1)..).zip(parsed.tasks[current_idx + 1..].iter().cloned())
    {
        let mut renumbered = task;
        renumbered.id = next_pending_id;
        rewritten.push(renumbered);
    }

    let file = tasks::TasksFile { tasks: rewritten };
    let text = toml::to_string_pretty(&file).context("serialize revised tasks.toml")?;
    std::fs::write(&tasks_path, text)
        .with_context(|| format!("write revised {}", tasks_path.display()))?;
    Ok(())
}

pub(crate) fn validate_stage_toml_writes(
    session_dir: &std::path::Path,
    stage: &str,
    round: u32,
) -> anyhow::Result<()> {
    let Some(io) = session_state::transitions::stage_io(stage) else {
        return Ok(());
    };
    let round_token = format!("{round:03}");
    let paths = io
        .writes
        .iter()
        .filter(|template| template.ends_with(".toml"))
        .map(|template| session_dir.join(template.replace("{round}", &round_token)))
        .collect::<Vec<_>>();
    let refs = paths.iter().map(|path| path.as_path()).collect::<Vec<_>>();
    crate::runner::validate_toml_artifacts(&refs)
}

pub(crate) fn read_review_scope(path: &std::path::Path) -> anyhow::Result<ReviewScopeArtifact> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let scope: ReviewScopeArtifact =
        toml::from_str(&text).with_context(|| format!("malformed TOML in {}", path.display()))?;
    if scope.base_sha.trim().is_empty() {
        anyhow::bail!("base_sha is empty in {}", path.display());
    }
    Ok(scope)
}

pub(crate) fn read_review_scope_base_sha(path: &std::path::Path) -> anyhow::Result<String> {
    Ok(read_review_scope(path)?.base_sha.trim().to_string())
}

pub(crate) fn write_review_scope_artifact(
    round_dir: &std::path::Path,
    base_sha: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(round_dir)?;
    std::fs::write(
        round_dir.join("review_scope.toml"),
        format!("base_sha = \"{base_sha}\"\n"),
    )
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

/// Prepended to every agent prompt. Surfaces project-specific guidance
/// (CLAUDE.md / AGENTS.md) before the agent acts. Returns an empty string
/// if neither file is present in the cwd, to avoid wasting prompt context
/// directing the agent to read files that don't exist.
pub(crate) fn project_doc_instr() -> String {
    let claude_path = Path::new("CLAUDE.md");
    let agents_path = Path::new("AGENTS.md");
    let claude = claude_path.exists();
    let agents = agents_path.exists();
    let docs = match (claude, agents) {
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
pub(crate) fn live_summary_instruction(path: &std::path::Path) -> String {
    PromptCtx::new().live_summary_instruction(path)
}

#[cfg(test)]
pub(crate) fn live_summary_instruction_interactive(path: &std::path::Path) -> String {
    PromptCtx::new().live_summary_instruction_interactive(path)
}

pub(crate) fn spec_review_prompt(
    spec_path: &str,
    review_path: &str,
    live_summary_path: &str,
) -> String {
    let ctx = PromptCtx::new();
    let instr = ctx.live_summary_instruction(Path::new(live_summary_path));
    let spec_path = ctx.path(Path::new(spec_path));
    let review_path = ctx.path(Path::new(review_path));
    prompt_template!(
        SPEC_REVIEW_TEMPLATE,
        project_doc_instr = ctx.project_doc_instr(),
        spec_path = &spec_path,
        review_path = &review_path,
        instr = &instr,
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
    let instr = ctx.live_summary_instruction(Path::new(live_summary_path));
    let spec_path = ctx.path(Path::new(spec_path));
    let plan_path = ctx.path(Path::new(plan_path));
    let review_path = ctx.path(Path::new(review_path));
    let review_dir = Path::new(&review_path)
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let prior_block = if round > 1 {
        let lines: Vec<String> = (1..round)
            .map(|r| format!("    {review_dir}/plan-review-{r}.md"))
            .collect();
        format!(
            "\nPrior plan reviews (read first; do NOT re-flag what's already addressed):\n{}\n",
            lines.join("\n")
        )
    } else {
        String::new()
    };
    prompt_template!(
        PLAN_REVIEW_TEMPLATE,
        project_doc_instr = ctx.project_doc_instr(),
        spec_path = &spec_path,
        plan_path = &plan_path,
        review_path = &review_path,
        prior_block = &prior_block,
        instr = &instr,
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
    let spec_path = ctx.path(Path::new(spec_path));
    let summary_path = ctx.path(Path::new(summary_path));
    let skip_proposal_path = Path::new(&summary_path)
        .parent()
        .map(|dir| dir.join("skip_proposal.toml"))
        .unwrap_or_else(|| resolved_agent_path(Path::new("skip_proposal.toml")));
    let skip_proposal_path = ctx.path(&skip_proposal_path);
    let (template, instr) = if yolo {
        (
            BRAINSTORM_YOLO_TEMPLATE,
            ctx.live_summary_instruction(Path::new(live_summary_path)),
        )
    } else {
        (
            BRAINSTORM_INTERACTIVE_TEMPLATE,
            ctx.live_summary_instruction_interactive(Path::new(live_summary_path)),
        )
    };
    prompt_template!(
        template,
        project_doc_instr = ctx.project_doc_instr(),
        idea = idea,
        spec_path = &spec_path,
        summary_path = &summary_path,
        skip_proposal_path = &skip_proposal_path,
        instr = &instr,
    )
}

pub(crate) fn planning_prompt(
    spec_path: &std::path::Path,
    review_paths: &[std::path::PathBuf],
    plan_path: &std::path::Path,
    live_summary_path: &std::path::Path,
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
    let (template, instr) = if yolo {
        (
            PLANNING_YOLO_TEMPLATE,
            ctx.live_summary_instruction(live_summary_path),
        )
    } else {
        (
            PLANNING_INTERACTIVE_TEMPLATE,
            ctx.live_summary_instruction_interactive(live_summary_path),
        )
    };
    let spec_str = ctx.path(spec_path);
    let plan_str = ctx.path(plan_path);
    prompt_template!(
        template,
        project_doc_instr = ctx.project_doc_instr(),
        spec = &spec_str,
        reviews = &reviews_block,
        plan = &plan_str,
        instr = &instr,
    )
}

pub(crate) fn final_validation_prompt(
    idea_text: &str,
    spec_text: &str,
    verdict_path: &std::path::Path,
    live_summary_path: &std::path::Path,
    simplification_path: Option<&std::path::Path>,
) -> String {
    let ctx = PromptCtx::new();
    let instr = ctx.live_summary_instruction(live_summary_path);
    let verdict_str = ctx.path(verdict_path);
    let live_summary_str = ctx.path(live_summary_path);
    // Pass the simplifier's self-report only when a TOML exists for this
    // round. The validator may inspect it for context but its own verdict
    // remains independent — the simplifier's claim is never authoritative.
    let simplification_block = match simplification_path {
        Some(path) if path.exists() => format!(
            "\nSimplification context (advisory only — the simplifier's self-report; do not let it override your independent judgment):\n  {}\n",
            ctx.path(path)
        ),
        _ => String::new(),
    };
    prompt_template!(
        FINAL_VALIDATION_TEMPLATE,
        project_doc_instr = ctx.project_doc_instr(),
        idea_text = idea_text,
        spec_text = spec_text,
        verdict = &verdict_str,
        live_summary = &live_summary_str,
        simplification_block = &simplification_block,
        instr = &instr,
    )
}

pub(crate) fn sharding_prompt(
    spec_path: &std::path::Path,
    plan_path: &std::path::Path,
    tasks_path: &std::path::Path,
    live_summary_path: &std::path::Path,
) -> String {
    let ctx = PromptCtx::new();
    let instr = ctx.live_summary_instruction(live_summary_path);
    let spec_str = ctx.path(spec_path);
    let plan_str = ctx.path(plan_path);
    let tasks_str = ctx.path(tasks_path);
    prompt_template!(
        SHARDING_TEMPLATE,
        project_doc_instr = ctx.project_doc_instr(),
        spec = &spec_str,
        plan = &plan_str,
        tasks = &tasks_str,
        instr = &instr,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn recovery_prompt(
    spec_path: &std::path::Path,
    plan_path: &std::path::Path,
    tasks_path: &std::path::Path,
    trigger_task_id: Option<u32>,
    trigger_summary: Option<&str>,
    completed_task_ids: &[u32],
    started_task_ids: &[u32],
    live_summary_path: &std::path::Path,
    recovery_path: &std::path::Path,
    interactive: bool,
) -> String {
    let ctx = PromptCtx::new();
    let instr = if interactive {
        ctx.live_summary_instruction_interactive(live_summary_path)
    } else {
        ctx.live_summary_instruction(live_summary_path)
    };
    let trigger_task = trigger_task_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "(none)".to_string());
    let trigger_summary = trigger_summary.unwrap_or("(none recorded)");
    let completed = if completed_task_ids.is_empty() {
        "(none)".to_string()
    } else {
        completed_task_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let started = if started_task_ids.is_empty() {
        "(none)".to_string()
    } else {
        started_task_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let template = if interactive {
        RECOVERY_INTERACTIVE_TEMPLATE
    } else {
        RECOVERY_NONINTERACTIVE_TEMPLATE
    };
    let spec_str = ctx.path(spec_path);
    let plan_str = ctx.path(plan_path);
    let tasks_str = ctx.path(tasks_path);
    let recovery_str = ctx.path(recovery_path);
    prompt_template!(
        template,
        project_doc_instr = ctx.project_doc_instr(),
        spec = &spec_str,
        plan = &plan_str,
        tasks = &tasks_str,
        recovery = &recovery_str,
        trigger_task = &trigger_task,
        trigger_summary = trigger_summary,
        completed = &completed,
        started = &started,
        instr = &instr,
    )
}

pub(crate) fn recovery_plan_review_prompt(
    spec_path: &std::path::Path,
    plan_path: &std::path::Path,
    triggering_review_path: &std::path::Path,
    recovery_path: &std::path::Path,
    live_summary_path: &std::path::Path,
    plan_review_output_path: &std::path::Path,
) -> String {
    let ctx = PromptCtx::new();
    let instr = ctx.live_summary_instruction(live_summary_path);
    let spec_str = ctx.path(spec_path);
    let plan_str = ctx.path(plan_path);
    let review_str = ctx.path(triggering_review_path);
    let recovery_str = ctx.path(recovery_path);
    let output_str = ctx.path(plan_review_output_path);
    prompt_template!(
        RECOVERY_PLAN_REVIEW_TEMPLATE,
        project_doc_instr = ctx.project_doc_instr(),
        spec = &spec_str,
        plan = &plan_str,
        review = &review_str,
        recovery = &recovery_str,
        output = &output_str,
        instr = &instr,
    )
}

pub(crate) fn recovery_sharding_prompt(
    spec_path: &std::path::Path,
    plan_path: &std::path::Path,
    live_summary_path: &std::path::Path,
    tasks_output_path: &std::path::Path,
    completed_ids: &[u32],
    id_floor: u32,
) -> String {
    let completed_str = if completed_ids.is_empty() {
        "none".to_string()
    } else {
        completed_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let ctx = PromptCtx::new();
    let instr = ctx.live_summary_instruction(live_summary_path);
    let spec_str = ctx.path(spec_path);
    let plan_str = ctx.path(plan_path);
    let output_str = ctx.path(tasks_output_path);
    let id_floor_str = id_floor.to_string();
    prompt_template!(
        RECOVERY_SHARDING_TEMPLATE,
        project_doc_instr = ctx.project_doc_instr(),
        spec = &spec_str,
        plan = &plan_str,
        completed = &completed_str,
        id_floor = &id_floor_str,
        output = &output_str,
        instr = &instr,
    )
}

#[allow(unused_imports)]
pub(super) use super::review_banner::{REVIEW_BANNER, prepend_review_banner, strip_review_banner};

// `capture_round_base` writes a deterministic placeholder in `cfg(test)`
// builds so transitions never shell out to git from the test process; this
// helper is only reachable on the production path.
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn git_rev_parse_head() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

pub(crate) fn coder_prompt(
    session_dir: &std::path::Path,
    task_id: u32,
    round: u32,
    task_file: &std::path::Path,
    live_summary_path: &std::path::Path,
    resume: bool,
    refine_carryover: &[String],
) -> String {
    let ctx = PromptCtx::new();
    let spec = session_dir.join("artifacts/spec.md");
    let plan = session_dir.join("artifacts/plan.md");
    let coder_summary = session_dir
        .join("rounds")
        .join(format!("{round:03}"))
        .join("coder_summary.toml");
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
        let bullets = refine_carryover
            .iter()
            .map(|item| format!("  - {}", item.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "\nRefine carryover from prior task's reviewer (apply opportunistically — these are nice-to-haves, not blockers):\n{bullets}\n"
        )
    };
    let resume_hint = if resume {
        "\nThis is a RESUME of a previous coding session on the same task — pick up where\nyou left off, honour the reviewer feedback above, and finish the work.\n"
    } else {
        ""
    };
    let instr = ctx.live_summary_instruction(live_summary_path);
    let task_id_str = task_id.to_string();
    let round_str = round.to_string();
    let task_str = ctx.path(task_file);
    let spec_str = ctx.path(&spec);
    let plan_str = ctx.path(&plan);
    let coder_summary_str = ctx.path(&coder_summary);
    prompt_template!(
        CODER_TEMPLATE,
        project_doc_instr = ctx.project_doc_instr(),
        task_id = &task_id_str,
        round = &round_str,
        task = &task_str,
        spec = &spec_str,
        plan = &plan_str,
        coder_summary = &coder_summary_str,
        prev_review = &prev_review,
        refine_block = &refine_block,
        resume_hint = resume_hint,
        instr = &instr,
    )
}

pub(crate) struct ReviewerPromptInputs<'a> {
    pub(crate) session_dir: &'a std::path::Path,
    pub(crate) task_id: u32,
    pub(crate) round: u32,
    pub(crate) task_file: &'a std::path::Path,
    pub(crate) review_scope_file: &'a std::path::Path,
    pub(crate) coder_summary_file: Option<&'a std::path::Path>,
    pub(crate) review_file: &'a std::path::Path,
    pub(crate) live_summary_path: &'a std::path::Path,
}

pub(crate) fn reviewer_prompt(inputs: ReviewerPromptInputs<'_>) -> String {
    let ctx = PromptCtx::new();
    let ReviewerPromptInputs {
        session_dir,
        task_id,
        round,
        task_file,
        review_scope_file,
        coder_summary_file,
        review_file,
        live_summary_path,
    } = inputs;
    let spec = session_dir.join("artifacts/spec.md");
    let plan = session_dir.join("artifacts/plan.md");
    let instr = ctx.live_summary_instruction(live_summary_path);
    let prior_reviews = if round > 1 {
        let lines: Vec<String> = (1..round)
            .map(|r| {
                let p = session_dir
                    .join("rounds")
                    .join(format!("{r:03}"))
                    .join("review.toml");
                format!("    {}", ctx.path(&p))
            })
            .collect();
        format!(
            "  Prior reviews for this task (read first; do not repeat their feedback):\n{}\n",
            lines.join("\n")
        )
    } else {
        String::new()
    };
    let coder_summary_section = coder_summary_file.map_or(String::new(), |path| {
        format!(
            "  Coder summary: {}\n  Coder rebuttal (round {}):\n    Read it before your verdict.\n    If the coder rebuts prior feedback convincingly, do not repeat that item as blocking feedback.\n    Rebuttal entries use the prefix \"[Round N, Item M]\".\n",
            ctx.path(path),
            round
        )
    });
    let review_scope_text = "  4. Check correctness, missing edge cases, broken contracts, bad error\n     handling, test gaps. Uncommitted working-tree changes are NOT in scope —\n     review only `base..HEAD`.\n";
    let task_id_str = task_id.to_string();
    let round_str = round.to_string();
    let task_str = ctx.path(task_file);
    let spec_str = ctx.path(&spec);
    let plan_str = ctx.path(&plan);
    let review_scope_str = ctx.path(review_scope_file);
    let review_str = ctx.path(review_file);
    prompt_template!(
        REVIEWER_TEMPLATE,
        project_doc_instr = ctx.project_doc_instr(),
        task_id = &task_id_str,
        round = &round_str,
        task = &task_str,
        spec = &spec_str,
        plan = &plan_str,
        review_scope = &review_scope_str,
        prior_reviews = &prior_reviews,
        coder_summary_section = &coder_summary_section,
        review_scope_text = review_scope_text,
        review = &review_str,
        instr = &instr,
    )
}

/// Builds the simplifier-stage prompt: a single behavior-preserving cleanup
/// pass over the round's `base_sha..HEAD` diff. The simplifier writes
/// `simplification_path` and a live summary, and produces `refactor:` /
/// `style:` commits when it has work to do.
pub(crate) fn simplifier_prompt(
    session_dir: &std::path::Path,
    review_scope_file: &std::path::Path,
    simplification_path: &std::path::Path,
    live_summary_path: &std::path::Path,
) -> String {
    let ctx = PromptCtx::new();
    let spec = session_dir.join("artifacts/spec.md");
    let plan = session_dir.join("artifacts/plan.md");
    let instr = ctx.live_summary_instruction(live_summary_path);
    let spec_str = ctx.path(&spec);
    let plan_str = ctx.path(&plan);
    let review_scope_str = ctx.path(review_scope_file);
    let simplification_str = ctx.path(simplification_path);
    prompt_template!(
        SIMPLIFIER_TEMPLATE,
        project_doc_instr = ctx.project_doc_instr(),
        spec_path = &spec_str,
        plan_path = &plan_str,
        review_scope_path = &review_scope_str,
        simplification_path = &simplification_str,
        instr = &instr,
    )
}
