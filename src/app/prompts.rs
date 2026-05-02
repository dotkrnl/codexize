// prompts.rs
use crate::{
    adapters::EffortLevel,
    artifacts::ReviewScopeArtifact,
    state::{self as session_state},
    tasks,
};
use anyhow::Context;
pub(super) fn cancel_run_label(base: &str) {
    crate::runner::cancel_run_labels_matching(base);
}

pub(super) fn restore_artifacts(pairs: &[(&std::path::Path, &std::path::Path)]) {
    for (backup, target) in pairs {
        if backup.exists() {
            let _ = std::fs::copy(backup, target);
        }
    }
}

pub(super) fn task_toml_for(session_dir: &std::path::Path, task_id: u32) -> anyhow::Result<String> {
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

pub(super) fn task_effort_for(session_dir: &std::path::Path, task_id: u32) -> EffortLevel {
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

pub(super) fn assigned_revise_task_ids(
    builder: &session_state::BuilderState,
    count: usize,
) -> Vec<u32> {
    let mut ids = Vec::with_capacity(count);
    for next_id in builder.max_task_id() + 1..builder.max_task_id() + 1 + count as u32 {
        ids.push(next_id);
    }
    ids
}

pub(super) fn rewrite_tasks_for_revise(
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

pub(super) fn validate_stage_toml_writes(
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

pub(super) fn read_review_scope(path: &std::path::Path) -> anyhow::Result<ReviewScopeArtifact> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let scope: ReviewScopeArtifact =
        toml::from_str(&text).with_context(|| format!("malformed TOML in {}", path.display()))?;
    if scope.base_sha.trim().is_empty() {
        anyhow::bail!("base_sha is empty in {}", path.display());
    }
    Ok(scope)
}

pub(super) fn read_review_scope_base_sha(path: &std::path::Path) -> anyhow::Result<String> {
    Ok(read_review_scope(path)?.base_sha.trim().to_string())
}

pub(super) fn write_review_scope_artifact(
    round_dir: &std::path::Path,
    base_sha: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(round_dir)?;
    std::fs::write(
        round_dir.join("review_scope.toml"),
        format!("base_sha = \"{base_sha}\"\n"),
    )
}

/// Prepended to every agent prompt. Surfaces project-specific guidance
/// (CLAUDE.md / AGENTS.md) before the agent acts. Returns an empty string
/// if neither file is present in the cwd, to avoid wasting prompt context
/// directing the agent to read files that don't exist.
pub(super) fn project_doc_instr() -> String {
    let claude = std::path::Path::new("CLAUDE.md").exists();
    let agents = std::path::Path::new("AGENTS.md").exists();
    let docs = match (claude, agents) {
        (true, true) => "CLAUDE.md and AGENTS.md",
        (true, false) => "CLAUDE.md",
        (false, true) => "AGENTS.md",
        (false, false) => return String::new(),
    };
    format!("Read {docs} in the repo first and follow those directions carefully.\n\n")
}

pub(super) fn live_summary_instruction(path: &std::path::Path) -> String {
    format!(
        "\n\nImmediately create {}, then every 2–3 min and on each sub-goal change, overwrite it with `<short title ≤5 words, varies as focus shifts> | <one-paragraph summary of progress + next action>`. Keep this file current until you exit. (process killed after 10 min wall-time idle, tool-call time excluded).\n",
        path.display()
    )
}

pub(super) fn live_summary_instruction_interactive(path: &std::path::Path) -> String {
    format!(
        "\n\nImmediately create {}, then every 2–3 min overwrite it with `<short title> | <one-paragraph summary>` so the operator can follow along. Keep this file current until you exit.\n",
        path.display()
    )
}

pub(super) fn spec_review_prompt(
    spec_path: &str,
    review_path: &str,
    live_summary_path: &str,
) -> String {
    let instr = live_summary_instruction(std::path::Path::new(live_summary_path));
    let project_doc_instr = project_doc_instr();
    format!(
        r#"{project_doc_instr}You review a spec. NON-INTERACTIVE — no clarifying questions, no code
changes, no VCS, no test runs. Write ONLY the review file.

Spec:   {spec_path}
Output: {review_path}

Evaluate clarity, completeness, buildability, risks, and gaps. The review
MUST cover, in this order:
  - Specific issues (if any), each with a suggested fix. Cite the spec
    section you're objecting to as `## Section name` or `(spec line N)` so
    the planner can triage cheaply.
  - Open risks the spec does not address.
  - TL;DR check: confirm the spec's TL;DR (top of file) matches the body —
    flag any decision in the body missing from the TL;DR or vice versa.
  - Treat the `## User-stated requirements (authoritative)` section as
    read-only. If the rest of the spec contradicts an item there, flag the
    contradiction with the spec section as the offender — never the
    authoritative section. Do not propose edits to that section.
  - Bottom-line judgement on the last line: ship-as-is / needs-revision /
    reject.
{instr}"#
    )
}

pub(super) fn plan_review_prompt(
    spec_path: &str,
    plan_path: &str,
    review_path: &str,
    round: u32,
    live_summary_path: &str,
) -> String {
    let instr = live_summary_instruction(std::path::Path::new(live_summary_path));
    let project_doc_instr = project_doc_instr();
    let review_dir = std::path::Path::new(review_path)
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
    format!(
        r#"{project_doc_instr}You review an implementation plan. NON-INTERACTIVE — no clarifying
questions, no source-code edits, no VCS, no test runs.

Inputs:
  Plan: {plan_path}
  Spec: {spec_path}{prior_block}

Flag ONLY critical issues — things that would block or break implementation:
  - Spec requirement with no corresponding plan step, or vice versa.
  - Plan steps ordered unbuildably (a step depends on output of a later step).
  - Plan↔spec or internal contradictions that would lead to the wrong build.
  - File paths / function names / interfaces inconsistent across steps in a
    way that would cause real breakage.
  - Spec-level ambiguity severe enough that an implementer could not proceed.
  - TL;DR drift — the plan's TL;DR misrepresents the body, or the spec's
    TL;DR misrepresents its body after planning edits.

Do NOT flag: cosmetic concerns (typos/grammar/wording/style/formatting/
structural polish), missing low-level implementation detail, or alternative-
but-valid implementation choices. Multiple valid implementations is NOT a
defect — don't force one internal design when several satisfy the spec and
the plan's explicit interfaces. When in doubt, leave it alone.

If — and only if — you find critical issues, directly edit {plan_path} (and
{spec_path} if spec-level — but NEVER the `## User-stated requirements
(authoritative)` section; if the issue lives there, it must be raised to the
operator, not patched) with the smallest fix. Write a markdown-bullet changelog
to {review_path}: one bullet per edit, naming the file changed and citing the
spec section / plan step that mandated the fix (audit trail). If nothing was
critical, write a single bullet saying so — do NOT invent issues.
{instr}"#
    )
}

fn brainstorm_package_instruction(package_path: Option<&std::path::Path>) -> String {
    let Some(package_path) = package_path else {
        return String::new();
    };
    // Metadata records the package root directory, and vendor adapters are
    // discovered from that directory rather than from an individual file.
    format!(
        "Use the brainstorming package installed at {}. Use that installed package for brainstorming.\n\n",
        package_path.display()
    )
}

pub(super) fn brainstorm_prompt(
    idea: &str,
    spec_path: &str,
    summary_path: &str,
    live_summary_path: &str,
    package_path: Option<&std::path::Path>,
    yolo: bool,
) -> String {
    let project_doc_instr = project_doc_instr();
    let package_instr = brainstorm_package_instruction(package_path);
    if !yolo {
        let instr = live_summary_instruction_interactive(std::path::Path::new(live_summary_path));
        return format!(
            r#"{project_doc_instr}Invoke your brainstorming skill now.

{package_instr}Idea:
---
{idea}
---

Operator IS available for design questions — interrogate them on ambiguities,
scope, and trade-offs BEFORE drafting. The "stop and exit" rule below covers
stage-transition asks only, not design clarifications.

Authoritative user input — at the top of {spec_path}, write a section titled
exactly:

    ## User-stated requirements (authoritative)

Quote each user-stated decision from the Idea above verbatim as a bullet.
Use the user's own wording, not a paraphrase. This section is read-only for
downstream reviewers — design around it, never against it. If a user
statement is ambiguous, ask the operator. If two user statements conflict with
each other, ask the operator. Never silently reinterpret.

Outputs (all under artifacts/, SPEC-ONLY phase — no code, no VCS):
  1. {spec_path} — the design doc. Start with a TL;DR (3–6 bullets a lazy
     reader can skim in 30 sec), then the full spec.
  2. {summary_path} — TOML with `title = "<≤80 chars naming the actual
     change, e.g. 'Add Kimi adapter min-quota fallback'>"`. Avoid generic
     labels ("Refactor", "New feature", "Update files in src/"). Required,
     even when proposing one of the escape hatches below.

Optional escape hatches (RARE — when in doubt, omit and let the normal
spec-review → planning → sharding pipeline run):

  • Skip-to-impl: write artifacts/skip_proposal.toml as TOML:
        proposed  = true
        status    = "skip_to_impl"
        rationale = "<≤500 chars why>"
    Hard gates (ALL must hold): one coherent change landable in a single
    commit, small enough to review in one sitting, no new modules /
    cross-cutting refactors / migrations / multi-file rewrites.
    "Simple but long" tasks (mechanical edits across many files) DO NOT
    qualify — sharding adds value via parallelisation. When skipping, keep
    the spec concise (goal, edit sites, acceptance check).

  • Nothing-to-do: when there is genuinely nothing to implement (already in
    place, invalid premise, pure question). Still required:
      - {spec_path} — one short paragraph explaining why nothing is needed.
      - artifacts/skip_proposal.toml as TOML:
            proposed  = true
            status    = "nothing_to_do"
            rationale = "<≤500 chars why>"

Hard rules (override the skill where it conflicts):
  - No `git add`/`commit`/`stash` or any version-control mutation — files
    stay untracked; a later phase commits.
  - Don't ask the operator whether to continue, proceed, or run follow-up
    skills (including any "continue to next stage" inline prompt). When
    your output files are written, STOP and exit; the orchestrator drives
    stage transitions.

Stage completion — ONLY once all pending design questions are resolved and
your output files are written: end that final message with a line asking the
operator to enter `/exit` if they have no further comments. While you are
still waiting for the operator's input, never include this cue.
{instr}"#
        );
    }

    let instr = live_summary_instruction(std::path::Path::new(live_summary_path));
    format!(
        r#"{project_doc_instr}You have the operator's full trust. Make very good decisions — be bold and
decisive. Do not hedge or ask for confirmation. Resolve every ambiguity using
your best judgement and move forward.

Invoke your brainstorming skill now.

{package_instr}Idea:
---
{idea}
---

Operator is unavailable; resolve ambiguities, scope, and trade-offs yourself per the trust preamble above.

Authoritative user input — at the top of {spec_path}, write a section titled
exactly:

    ## User-stated requirements (authoritative)

Quote each user-stated decision from the Idea above verbatim as a bullet.
Use the user's own wording, not a paraphrase. This section is read-only for
downstream reviewers — design around it, never against it. If a user
statement is ambiguous, pick the narrowest reasonable reading and note the
assumption in a sibling `## Assumptions` section. If two user statements
conflict with each other, list both verbatim and pick the narrowest reading
consistent with the rest of the Idea, recording the choice under
`## Assumptions`. Never silently reinterpret.

Outputs (all under artifacts/, SPEC-ONLY phase — no code, no VCS):
  1. {spec_path} — the design doc. Start with a TL;DR (3–6 bullets a lazy
     reader can skim in 30 sec), then the full spec.
  2. {summary_path} — TOML with `title = "<≤80 chars naming the actual
     change, e.g. 'Add Kimi adapter min-quota fallback'>"`. Avoid generic
     labels ("Refactor", "New feature", "Update files in src/"). Required,
     even when proposing one of the escape hatches below.

Optional escape hatches (RARE — when in doubt, omit and let the normal
spec-review → planning → sharding pipeline run):

  • Skip-to-impl: write artifacts/skip_proposal.toml as TOML:
        proposed  = true
        status    = "skip_to_impl"
        rationale = "<≤500 chars why>"
    Hard gates (ALL must hold): one coherent change landable in a single
    commit, small enough to review in one sitting, no new modules /
    cross-cutting refactors / migrations / multi-file rewrites.
    "Simple but long" tasks (mechanical edits across many files) DO NOT
    qualify — sharding adds value via parallelisation. When skipping, keep
    the spec concise (goal, edit sites, acceptance check).

  • Nothing-to-do: when there is genuinely nothing to implement (already in
    place, invalid premise, pure question). Still required:
      - {spec_path} — one short paragraph explaining why nothing is needed.
      - artifacts/skip_proposal.toml as TOML:
            proposed  = true
            status    = "nothing_to_do"
            rationale = "<≤500 chars why>"

Hard rules (override the skill where it conflicts):
  - No `git add`/`commit`/`stash` or any version-control mutation — files
    stay untracked; a later phase commits.
  - Don't ask the operator whether to continue, proceed, or run follow-up
    skills (including any "continue to next stage" inline prompt). When
    your output files are written, STOP and exit; the orchestrator drives
    stage transitions.
{instr}"#
    )
}

pub(super) fn planning_prompt(
    spec_path: &std::path::Path,
    review_paths: &[std::path::PathBuf],
    plan_path: &std::path::Path,
    live_summary_path: &std::path::Path,
    yolo: bool,
) -> String {
    let reviews_block = if review_paths.is_empty() {
        "(no spec reviews available — work from the spec alone)".to_string()
    } else {
        review_paths
            .iter()
            .enumerate()
            .map(|(i, p)| format!("  - review {}: {}", i + 1, p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let project_doc_instr = project_doc_instr();
    if !yolo {
        let instr = live_summary_instruction_interactive(live_summary_path);
        return format!(
            r#"{project_doc_instr}Invoke your superpowers:writing-plans skill now.

Turn the approved spec + spec reviews into an implementation plan.

Inputs:
  Spec:    {spec}
  Reviews:
{reviews}

Triage reviews first. They may contradict each other AND are written by AI
agents — be skeptical, accept only what genuinely improves the spec or plan,
reject the rest with a brief reason.

Escalation rules — ask the operator when:
• The feedback affects end-user-facing design (UI/UX, CLI behavior, config
  schema, output formats, user-facing prompts, file layout). MUST ask.
  Present a concise accept/reject choice; never decide alone.
• The feedback is an internal design decision (code structure, module
  boundaries, function signatures, invisible implementation patterns) and
  you are very unsure. If confident, decide and briefly explain why.
• Cosmetic / trivial (typos, naming nits, formatting, obvious fixes) —
  decide alone; no escalation.

Once trade-offs are resolved, do TWO things IN THIS ORDER:
  1. UPDATE {spec} in place to reflect every accepted decision. If you change
     the body, also update its TL;DR so the two stay consistent — an agent
     reading ONLY the spec must not be surprised by anything in the plan.
  2. Write {plan} starting with a TL;DR (3–6 bullets summarising key
     sequencing/interface decisions, skimmable in 30 sec), then the body.

Plan shape: an execution map for coordination — sequencing & dependencies
(what order matters and why), interfaces / integration points / execution
seams to honor, spec constraints that narrow the solution space, and (only
as orientation) likely file/module touchpoints. Do NOT write a patch recipe:
no checkbox to-dos, no function-by-function edit sequences, no "change line
X then Y", no mandated code shape (struct fields, method signatures, class
layout) unless the spec or an explicit interface commitment requires it.

Authority: spec is the design contract and wins any conflict; the plan is
authoritative ONLY for sequencing and the explicit interfaces it names —
everything else in the plan is advisory. Don't promote advisory detail into
an implementation contract.

Hard rules (override the skill where it conflicts):
  - No code/config/build-script edits, no `git add`/`commit`/`stash`, no test
    runs. You may only edit the spec and write the plan; both files stay
    untracked. Refuse if the skill offers to commit, push, or test.
  - Don't ask whether to continue, proceed, or run follow-up skills — when
    both files are written, STOP and exit. The orchestrator drives stage
    transitions.

Stage completion — ONLY once all pending trade-off decisions are resolved and
both files are written: end that final message with a line asking the operator
to enter `/exit` if they have no further comments. While you are still waiting
for the operator's input, never include this cue.
{instr}"#,
            spec = spec_path.display(),
            reviews = reviews_block,
            plan = plan_path.display(),
            instr = instr,
        );
    }

    let instr = live_summary_instruction(live_summary_path);
    format!(
        r#"{project_doc_instr}You have the operator's full trust. Make very good decisions — be bold and
decisive. Do not hedge or ask for confirmation. Resolve every ambiguity using
your best judgement and move forward.

Invoke your superpowers:writing-plans skill now.

Turn the approved spec + spec reviews into an implementation plan.

Inputs:
  Spec:    {spec}
  Reviews:
{reviews}

Triage reviews first. They may contradict each other AND are written by AI
agents — be skeptical, accept only what genuinely improves the spec or plan,
reject the rest with a brief reason. If a real trade-off exceeds your
confidence, resolve it yourself per the trust preamble above and choose the
approach that best satisfies the spec.

Once trade-offs are resolved, do TWO things IN THIS ORDER:
  1. UPDATE {spec} in place to reflect every accepted decision. If you change
     the body, also update its TL;DR so the two stay consistent — an agent
     reading ONLY the spec must not be surprised by anything in the plan.
  2. Write {plan} starting with a TL;DR (3–6 bullets summarising key
     sequencing/interface decisions, skimmable in 30 sec), then the body.

Plan shape: an execution map for coordination — sequencing & dependencies
(what order matters and why), interfaces / integration points / execution
seams to honor, spec constraints that narrow the solution space, and (only
as orientation) likely file/module touchpoints. Do NOT write a patch recipe:
no checkbox to-dos, no function-by-function edit sequences, no "change line
X then Y", no mandated code shape (struct fields, method signatures, class
layout) unless the spec or an explicit interface commitment requires it.

Authority: spec is the design contract and wins any conflict; the plan is
authoritative ONLY for sequencing and the explicit interfaces it names —
everything else in the plan is advisory. Don't promote advisory detail into
an implementation contract.

Hard rules (override the skill where it conflicts):
  - No code/config/build-script edits, no `git add`/`commit`/`stash`, no test
    runs. You may only edit the spec and write the plan; both files stay
    untracked. Refuse if the skill offers to commit, push, or test.
  - Don't ask whether to continue, proceed, or run follow-up skills — when
    both files are written, STOP and exit. The orchestrator drives stage
    transitions.
{instr}"#,
        spec = spec_path.display(),
        reviews = reviews_block,
        plan = plan_path.display(),
        instr = instr,
    )
}

pub(super) fn final_validation_prompt(
    idea_text: &str,
    spec_text: &str,
    verdict_path: &std::path::Path,
    live_summary_path: &std::path::Path,
) -> String {
    let instr = live_summary_instruction(live_summary_path);
    let project_doc_instr = project_doc_instr();
    format!(
        r#"{project_doc_instr}You are the final goal-validation agent. NON-INTERACTIVE — no operator,
no questions, no code edits, no VCS mutations. Your only outputs are the
verdict TOML and the live summary, written via the two allowed Write paths
below.

Heads up: you were intentionally not given the plan, any git diff, test or
build output, per-task review verdicts, or any prior validation rounds'
verdicts. The point is to evaluate the workspace independently against the
operator's stated goal — fresh eyes, no prior-pipeline anchoring.

Inputs (the only two; nothing else feeds your verdict):

Raw idea text (verbatim from session.toml — treat the operator's wording
literally; do not paraphrase requirements):
---
{idea_text}
---

Final spec (verbatim from artifacts/spec.md — pay explicit attention to the
`## User-stated requirements (authoritative)` and `## Out of scope`
sections):
---
{spec_text}
---

Source-of-truth precedence (apply in this order on every conflict):
  1. `## User-stated requirements (authoritative)` — binding; never
     contradicted by lower tiers.
  2. Explicit operator-agreed `## Out of scope` entries — items here are
     NOT gaps even if the raw idea text could be read as implying them.
  3. Rest of the spec — wins on conflicts with raw idea text only when it
     does not contradict the authoritative user-stated requirements.
  4. Raw idea text — canonical operator statement; loses to the above three
     when they conflict but otherwise governs.

Workspace inspection — required steps:
  - Run `git status --short` (or `git status --short --branch`) EARLY,
    before forming any opinion about gaps. Include a workspace-status note
    as one of the entries in `findings[]` so the operator can see what
    state the tree was in.
  - Use Read / Glob / Grep and the non-mutating Bash allowlist to inspect
    the tree. Allowed shell commands: `git status`, `git log` (read-only),
    `ls`, `cat`, `head`, `tail`, `wc`, `file`, `find` (no `-exec` /
    `-delete`), `pwd`. Anything else — including any `git` mutation, any
    `>` / `>>` / `|` redirect into a file, and any tool that could touch
    the working tree or VCS state — is forbidden.
  - Do **NOT** use `git diff`. Diff-based reasoning is the per-task
    reviewer's job; your value comes from judging the workspace as it
    stands against idea + spec.
  - You may NOT use Edit, NotebookEdit, or interactive Bash. You may NOT
    mutate the workspace under any circumstance. You may NOT write code.

Verdict scope — only flag gaps that trace back to a clause in the idea or
spec (under the precedence above). Do not flag tangential pre-existing
workspace issues unrelated to the goal. Items in `## Out of scope` are
never gaps.

Outputs (the ONLY two paths you may Write):
  - {verdict} — the verdict TOML.
  - {live_summary} — the live progress summary (rules below).

Verdict TOML schema (validated programmatically; parse failure or schema
violation = run failure):

    status  = "goal_met" | "goal_gap" | "needs_human"
    summary = "<one-paragraph human-readable verdict — required, non-empty>"
    findings = [
      "<one bullet per area you inspected (regardless of verdict); include the workspace-status note here>",
      # ...
    ]

    # Required when status = "goal_gap" or "needs_human"; forbidden when "goal_met".
    [[gaps]]
    description = "<what is missing or wrong, traced back to a clause in idea or spec>"
    checked     = ["src/foo.rs", "tests/bar.rs"]   # ≥1 inspected path per gap

    # Required when status = "goal_gap"; forbidden otherwise.
    # This is a validator-gap-task schema, NOT the orchestrator's tasks::Task
    # — omit `id`, `spec_refs`, and `plan_refs`; the orchestrator assigns those.
    [[new_tasks]]
    title           = "..."
    description     = "..."
    test            = "..."
    estimated_tokens = 1234

Status / gaps / new_tasks matrix:
  - goal_met     → empty gaps, empty new_tasks.
  - goal_gap    → non-empty gaps, non-empty new_tasks.
  - needs_human → non-empty gaps, empty new_tasks.

Hard rules (override any default skill behavior):
  - You may not mutate the workspace. You may not write code. Your only
    outputs are the verdict TOML and the live summary, using the two
    allowed Write paths above.
  - No `git add` / `commit` / `stash` / `checkout` / `reset` / `restore`
    or any other VCS mutation.
  - No shell redirection (`>`, `>>`, `|` into a file) — write to the
    allowed paths via the Write tool only.
  - Don't ask the operator to continue, proceed, or run follow-up skills
    — when the verdict is written, STOP and exit.
{instr}"#,
        verdict = verdict_path.display(),
        live_summary = live_summary_path.display(),
    )
}

pub(super) fn sharding_prompt(
    spec_path: &std::path::Path,
    plan_path: &std::path::Path,
    tasks_path: &std::path::Path,
    live_summary_path: &std::path::Path,
) -> String {
    let instr = live_summary_instruction(live_summary_path);
    let project_doc_instr = project_doc_instr();
    format!(
        r#"{project_doc_instr}You split an approved plan into actionable, self-contained, buildable
tasks. NON-INTERACTIVE — no code edits, no VCS, no questions; your ONLY
output is the tasks TOML.

Inputs:
  Spec: {spec}
  Plan: {plan}

Sizing:
  - Target ~100_000 tokens of implementation effort per task — fits one
    coding session without context compaction. Decompose only along natural
    seams (subsystem / layer / phase); if the plan fits one ~100k session,
    a single-task tasks.toml is correct. Prefer ≤10 tasks; exceed only when
    the plan genuinely demands it.
  - Each task self-contained: builds on its own (compiles / links /
    type-checks). It does NOT have to be independently testable —
    scaffolding/groundwork tasks that only become testable after a later
    task lands are allowed, as long as they still build cleanly.
  - Unless a dependency is explicitly listed in a task's description, no
    task may assume another has shipped first.

Coverage: every section of the plan must be covered by at least one task's
spec/plan refs. Don't drop work; don't invent work outside the plan.

Required fields per task:
  - id               sequential integer starting at 1
  - title            ≤60 chars, imperative, no trailing period — shown as
                     the pipeline-UI label
  - description      outcome-oriented (multi-line TOML string allowed). NOT
                     a patch recipe — the planner already established the
                     shape; preserve it. Cover required outcomes, dependencies,
                     acceptance checks, and interfaces/touchpoints.
  - test             concrete verification steps, OR `"not testable —
                     <one-line reason>"` for scaffolding tasks (reviewer
                     skips test-pass check, still requires the build to be
                     clean).
  - estimated_tokens integer (target ~100k)
  - spec_refs        array of {{ path, lines }} into the spec
  - plan_refs        array of {{ path, lines }} into the plan, pointing at
                     goals/sequencing/interface commitments — not at
                     recipe-style detail
  `lines` is a range like "12-45" or a single number.

Difficulty:
  - Mark `tough = true` on tasks that need deep reasoning: algorithmic
    complexity, concurrency, security-sensitive logic, tricky state
    machines, cross-cutting refactors touching many modules, or code
    with poor test coverage / documentation that demands careful reading.
  - Default is `tough = false` (the majority of tasks). When in doubt,
    leave it false — the system routes extra compute to tough tasks, so
    over-marking wastes budget.

Output: write {tasks} as TOML. No prose around it. Validated programmatically;
missing/empty fields cause rejection.

    [[tasks]]
    id = 1
    title = "Scaffold the worker pool"
    tough = false
    description = """
    Wire up a Tokio worker pool in src/pool.rs. …
    """
    test = """
    Run `cargo test pool::` — the new tests must pass.
    """
    estimated_tokens = 90000
    spec_refs = [{{ path = "artifacts/spec.md", lines = "10-45" }}]
    plan_refs = [
      {{ path = "artifacts/plan.md", lines = "22-60" }},
      {{ path = "artifacts/plan.md", lines = "110-125" }},
    ]
{instr}"#,
        spec = spec_path.display(),
        plan = plan_path.display(),
        tasks = tasks_path.display(),
        instr = instr,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn recovery_prompt(
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
    let instr = if interactive {
        live_summary_instruction_interactive(live_summary_path)
    } else {
        live_summary_instruction(live_summary_path)
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
    let project_doc_instr = project_doc_instr();
    let mode_label = if interactive {
        "INTERACTIVE — operator present"
    } else {
        "NON-INTERACTIVE — no operator questions"
    };
    let interactive_rule = if interactive {
        "  - Read the triggering feedback first to identify the human decision needed.\n  - Present the proposed correction to the operator and wait for explicit\n    confirmation BEFORE editing spec or plan.\n"
    } else {
        "  - Keep changes minimal and deterministic — no operator to consult.\n"
    };
    let exit_instruction = if interactive {
        "\n\nStage completion — ONLY once all pending confirmation decisions are resolved\nand your output files are written: end that final message with a line asking\nthe operator to enter `/exit` if they have no further comments. While you are\nstill waiting for the operator's confirmation, never include this cue."
    } else {
        ""
    };
    format!(
        r#"{project_doc_instr}You are the builder recovery agent. {mode_label} — no source-code
edits, no VCS mutations.

Heads up: your recovered artifacts will be reviewed downstream by an AI from
a DIFFERENT model vendor — bring care to the spec/plan edits and the audit
trail.

Your job is to repair builder artifacts so orchestration can reconcile and
resume. You may edit ONLY:
  - {spec}
  - {plan}
  - {tasks}
  - {recovery}

Context from orchestrator:
  - Triggering task id: {trigger_task}
  - Trigger summary / latest reviewer feedback:
    ```
    {trigger_summary}
    ```
  - Completed task ids (must stay completed): {completed}
  - Started task ids: {started}
    (started ⊇ completed; the difference is in-flight or abandoned work
    that may need to be reshaped or removed.)

Hard requirements:
{interactive_rule}  - Keep `tasks.toml` valid; include unfinished work only. Never include
    completed ids.
  - If you supersede or remove a started-but-unfinished task id, add a
    `## Recovery Notes` section to BOTH spec and plan with one bullet per
    superseded id and the reason. Example:
        ## Recovery Notes
        - Task 7 superseded: original approach (X) violated spec §3 after
          reviewer flagged Y. Replaced by tasks 9-10.
  - Write `{recovery}` as TOML in this exact shape:
        status        = "approved" | "revise"           # what the recovery did
        trigger       = "human_blocked" | "agent_pivot" # next recovery trigger;
                                                        # use human_blocked when
                                                        # operator judgement is required
        interactive   = true | false                    # whether the operator was consulted
        summary       = "One paragraph describing the decision."
        feedback      = ["one item per remediation step (optional)"]
        changed_files = ["artifacts/spec.md", "artifacts/plan.md", "artifacts/tasks.toml"]
                                                        # paths you actually edited (audit trail)
{exit_instruction}{instr}"#,
        spec = spec_path.display(),
        plan = plan_path.display(),
        tasks = tasks_path.display(),
        recovery = recovery_path.display(),
        trigger_task = trigger_task,
        trigger_summary = trigger_summary,
        completed = completed,
        started = started,
        exit_instruction = exit_instruction,
        instr = instr,
    )
}

pub(super) fn recovery_plan_review_prompt(
    spec_path: &std::path::Path,
    plan_path: &std::path::Path,
    triggering_review_path: &std::path::Path,
    recovery_path: &std::path::Path,
    live_summary_path: &std::path::Path,
    plan_review_output_path: &std::path::Path,
) -> String {
    let project_doc_instr = project_doc_instr();
    let instr = live_summary_instruction(live_summary_path);
    format!(
        r#"{project_doc_instr}You review the recovered plan after a recovery stage. NON-INTERACTIVE
— no operator, no source-code edits, no VCS, no test runs.

Heads up: the recovery agent who produced these artifacts is from a
DIFFERENT model vendor — bring fresh eyes, that's the point of pairing.

Inputs:
  - Spec: {spec}
  - Plan: {plan}
  - Triggering review (what caused recovery): {review}
  - Recovery artifact (what the recovery agent reported): {recovery}
    Schema: status / trigger / interactive / summary / feedback /
    changed_files. Cross-check that `changed_files` lines up with the
    actual edits visible in spec/plan.

Your job:
  1. Verify the recovered spec/plan directly addresses the triggering review.
  2. Verify the plan is coherent enough for sharding.
  3. Do NOT reopen broad product/design debate.
  4. Make minimal fixes to {spec} or {plan} only for critical issues. For
     each edit, add a bullet to `feedback` naming the file changed and the
     specific issue it resolves (audit trail).

# IMPORTANT: emit ONLY the TOML below to {output} — no prose around it.
# Parse failure = run failure.

    status   = "approved" | "refine" | "revise" | "human_blocked" | "agent_pivot"
    summary  = "One-line verdict."
    feedback = ["one item per issue or audit-trail edit"]
                                       # required unless approved with no issues

Routing downstream:
  - approved / refine → pipeline continues to sharding (refine carryover
    has no consumer here, so it behaves like approved).
  - revise / human_blocked / agent_pivot → recovery re-runs with your
    feedback. If the recovery artifact requested `trigger = "human_blocked"`,
    the retry is interactive so the operator can decide.
{instr}"#,
        spec = spec_path.display(),
        plan = plan_path.display(),
        review = triggering_review_path.display(),
        recovery = recovery_path.display(),
        output = plan_review_output_path.display(),
    )
}

pub(super) fn recovery_sharding_prompt(
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
    let project_doc_instr = project_doc_instr();
    let instr = live_summary_instruction(live_summary_path);
    format!(
        r#"{project_doc_instr}You are the recovery sharding agent. NON-INTERACTIVE — no operator,
no source-code edits, no VCS, no test runs. A recovery cycle has completed
and the recovered spec/plan have been approved. Regenerate the task list.

Heads up: your tasks.toml feeds coders and reviewers from DIFFERENT model
vendors than you — bring care to the descriptions and refs.

Inputs:
  - Spec: {spec}
  - Plan: {plan}

Read any `## Recovery Notes` sections in spec/plan FIRST — they list
superseded task ids and the reasons; don't re-create work that was
deliberately removed.

# IMPORTANT: completed task ids: {completed}.
# Every new task id MUST be strictly greater than {id_floor}; the
# orchestrator rejects ids ≤ {id_floor} (covers completed plus any id
# ever attempted, even if not finished).

Sizing & scope (same as initial sharding):
  - Target ~100k tokens of implementation effort per task; prefer ≤10
    tasks; decompose only along natural seams.
  - Each task self-contained: builds on its own (compiles / links /
    type-checks). Explicit dependencies in descriptions if any.
  - Coverage: every section of the recovered plan covered by at least
    one task's spec/plan refs.

Required fields per task: id / title (≤60 chars, imperative) / description
(outcome-oriented, NOT a patch recipe) / test (concrete steps, OR
`"not testable — <reason>"` for scaffolding) / estimated_tokens /
spec_refs / plan_refs (`{{ path, lines }}`).

Difficulty:
  - Mark `tough = true` on tasks that need deep reasoning: algorithmic
    complexity, concurrency, security-sensitive logic, tricky state
    machines, cross-cutting refactors touching many modules, or code
    with poor test coverage / documentation that demands careful reading.
  - Default is `tough = false` (the majority of tasks). When in doubt,
    leave it false — the system routes extra compute to tough tasks, so
    over-marking wastes budget.

Output: write {output} as TOML in this shape. No prose around it.
Validated programmatically; missing/empty fields or ids ≤ {id_floor}
cause rejection.

    [[tasks]]
    id = N                                      # N > {id_floor}
    title = "Imperative summary"
    tough = false
    description = """
    Outcome-oriented description...
    """
    test = """
    Concrete verification steps.
    """
    estimated_tokens = 90000
    spec_refs = [{{ path = "artifacts/spec.md", lines = "10-45" }}]
    plan_refs = [{{ path = "artifacts/plan.md", lines = "22-60" }}]
{instr}"#,
        spec = spec_path.display(),
        plan = plan_path.display(),
        completed = completed_str,
        id_floor = id_floor,
        output = tasks_output_path.display(),
    )
}

/// Prepended to spec/plan files when they're auto-opened for review, then
/// stripped (by exact match) once the editor closes. Keep the literal stable
/// — `strip_review_banner` removes only this exact string, so any drift
/// would leave the banner sitting in the file forever.
pub(super) const REVIEW_BANNER: &str = "\
████████████████████████████████████████████████████████████████████████
██                                                                    ██
██   PLEASE REVIEW THIS DOCUMENT, THEN CLOSE THE EDITOR TO CONTINUE.  ██
██                                                                    ██
██   This banner is auto-inserted on open and removed on close —      ██
██   leave it in place; it will not appear in the saved artifact.     ██
██                                                                    ██
████████████████████████████████████████████████████████████████████████

";

pub(super) fn prepend_review_banner(path: &std::path::Path) -> bool {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return false;
    };
    if existing.contains(REVIEW_BANNER) {
        return false;
    }
    let mut combined = String::with_capacity(REVIEW_BANNER.len() + existing.len());
    combined.push_str(REVIEW_BANNER);
    combined.push_str(&existing);
    std::fs::write(path, combined).is_ok()
}

pub(super) fn strip_review_banner(path: &std::path::Path) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path)?;
    let Some(idx) = existing.find(REVIEW_BANNER) else {
        return Ok(());
    };
    let mut stripped = String::with_capacity(existing.len() - REVIEW_BANNER.len());
    stripped.push_str(&existing[..idx]);
    stripped.push_str(&existing[idx + REVIEW_BANNER.len()..]);
    std::fs::write(path, stripped)
}

pub(super) fn git_rev_parse_head() -> Option<String> {
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

pub(super) fn coder_prompt(
    session_dir: &std::path::Path,
    task_id: u32,
    round: u32,
    task_file: &std::path::Path,
    live_summary_path: &std::path::Path,
    resume: bool,
    refine_carryover: &[String],
) -> String {
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
                p.display()
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
    let instr = live_summary_instruction(live_summary_path);
    let project_doc_instr = project_doc_instr();
    format!(
        r#"{project_doc_instr}You are the coder for task {task_id}, round {round}. NON-INTERACTIVE — the
operator is NOT available. Make your own judgement calls; put rationale in
the commit message and a line comment in the code on anything genuinely
ambiguous so the reviewer sees it.

Heads up: your work will be reviewed by an AI from a DIFFERENT model vendor
than you — a fresh pair of eyes that notices different things. Take the
extra moment on edge cases and clarity so the review goes smoothly.

Inputs:
  Task:  {task}   (what to do, test steps, line refs into spec/plan)
  Spec:  {spec}
  Plan:  {plan}
{prev_review}{refine_block}{resume_hint}
Job:
  1. Read the task file first.
  2. Implement end-to-end on the current branch. Match existing repo
     conventions; run the project's formatter/linter before committing.
  3. Run lint first (faster than full tests) and fix warnings, then make
     the task's tests pass — UNLESS the task's `test` field starts with
     "not testable" (genuine scaffolding). In that case skip the tests,
     but the code MUST still build cleanly (compiles / links / type-checks).
  4. Commit as a series of small atomic commits (see below). Reviewer sees
     `base..HEAD` for this round (`base` pinned by the orchestrator). No-
     commit is fine if the task was already done or you deliberately left
     changes uncommitted — declare it in `coder_summary.toml` (see below).

Commit granularity (MANDATORY):
  - One logical change per commit (a refactor, a function + its test, a
    bug fix). Every commit must build on its own at that SHA.
  - Don't mix unrelated changes; don't bundle formatting churn into
    functional commits — separate `style:`/`chore:` commits if needed.
  - If real-logic diff (excluding generated files, lockfiles, fixtures)
    exceeds ~200 lines, split. Single-commit-per-task only when the task
    genuinely is one atomic change.

Commit message (reviewer rejects violations):
  - Conventional Commits: `type(scope): summary` (feat/fix/refactor/test/
    docs/chore/perf/style/build). E.g. `fix(db): close pool on shutdown`.
  - No `Co-Authored-By:` trailers or co-author attribution.
  - No orchestrator vocabulary ("task N", "round N", "plan", "shard",
    "phase") or references to this prompt. Write as a standalone human
    engineer would.

Delegate bulk chores to subagents (renames, audits, sweeps, dependency
tracing) — never the implementation itself or the call on whether code is
correct. Give each subagent a self-contained brief; verify before committing.

Hard rules:
  - No clarifying questions — work from task + spec + plan.
  - Stay in this task's scope. Follow-up work you uncover → note for the
    reviewer, don't do it yourself.
  - Working tree must be clean on exit. Commit every change you intend to
    keep; revert anything you don't author or don't want to keep.
    `git status --porcelain` MUST be empty when you stop, even if the
    tree was already dirty when you started — inherited dirt is your
    problem to resolve (revert it; don't carry it forward). Leaving the
    tree dirty is a hard failure regardless of test/lint state.
  - No force-push, no history rewrite, no branch deletes.

Before exiting, write `{coder_summary}` in this exact TOML shape (REQUIRED):
    status       = "done" | "partial"      # "partial" makes the run retry
    summary      = "One short paragraph of what you completed."
    rebuttal     = ["[Round N, Item M] Response to prior reviewer feedback."]
                                            # only when prior feedback was wrong or already
                                            # addressed; prefix each item with [Round N, Item M]

If the task was already complete and you committed nothing, status = "done"
with the reason in summary — that's not a failure. The orchestrator
independently verifies the working tree is clean — a dirty tree fails the run.
{instr}"#,
        task_id = task_id,
        round = round,
        task = task_file.display(),
        spec = spec.display(),
        plan = plan.display(),
        coder_summary = coder_summary.display(),
        prev_review = prev_review,
        refine_block = refine_block,
        resume_hint = resume_hint,
        instr = instr,
    )
}

pub(super) struct ReviewerPromptInputs<'a> {
    pub(super) session_dir: &'a std::path::Path,
    pub(super) task_id: u32,
    pub(super) round: u32,
    pub(super) task_file: &'a std::path::Path,
    pub(super) review_scope_file: &'a std::path::Path,
    pub(super) coder_summary_file: Option<&'a std::path::Path>,
    pub(super) review_file: &'a std::path::Path,
    pub(super) live_summary_path: &'a std::path::Path,
}

pub(super) fn reviewer_prompt(inputs: ReviewerPromptInputs<'_>) -> String {
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
    let instr = live_summary_instruction(live_summary_path);
    let prior_reviews = if round > 1 {
        let lines: Vec<String> = (1..round)
            .map(|r| {
                let p = session_dir
                    .join("rounds")
                    .join(format!("{r:03}"))
                    .join("review.toml");
                format!("    {}", p.display())
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
            path.display(),
            round
        )
    });
    let review_scope_text = "  4. Check correctness, missing edge cases, broken contracts, bad error\n     handling, test gaps. Uncommitted working-tree changes are NOT in scope —\n     review only `base..HEAD`.\n";
    let project_doc_instr = project_doc_instr();
    format!(
        r#"{project_doc_instr}You are the reviewer for task {task_id}, round {round}. NON-INTERACTIVE —
no operator, no code edits, no VCS mutations. Write ONLY the review TOML.

Heads up: the coder you're reviewing is from a DIFFERENT model vendor — bring
fresh eyes, that's the whole point of pairing.

Inputs:
  Task:         {task}
  Spec:         {spec}
  Plan:         {plan}
  Review scope: {review_scope} (TOML with base_sha = HEAD at round start)
{prior_reviews}
{coder_summary_section}

Review:
  1. BASE=$(sed -n 's/^base_sha = "\(.*\)"$/\1/p' {review_scope})
     `git log --oneline $BASE..HEAD` — every commit in this round.
     `git diff $BASE..HEAD`           — aggregate change.
     `git show <sha>`                 — drill into any commit.
     Judge the aggregate delta; per-commit structure is the coder's choice.
  2. Judge task completion: does the aggregate delta actually deliver what's
     required? Read the task `description` AND the spec/plan sections it
     points to (via `spec_refs` and `plan_refs` in the task file). Task is
     complete only when the delta satisfies all of them. A green test run
     doesn't by itself prove completion; a missing test run doesn't by
     itself prove failure — read the code against the requirements.
  3. Verify the task's test description passes (run it, inspect code). If
     the task's `test` field starts with "not testable" (scaffolding/
     intermediate), SKIP the test-pass check — but still require the code
     to build cleanly (compiles / links / type-checks). Completion still
     matters.
{review_scope_text}

# IMPORTANT: emit ONLY the TOML below to {review} — no prose around it.
# Parse failure = run failure. Use double-quoted strings; triple-quoted for
# multi-line; arrays of inline tables for any new_task refs.

    status  = "approved" | "refine" | "revise" | "human_blocked" | "agent_pivot"
    summary = "One-paragraph summary of what was done and your verdict."
    feedback = [
      "Specific thing to fix (required for refine/revise/human_blocked/agent_pivot).",
      "One item per string.",
    ]

    # Optional: follow-up tasks for work genuinely out-of-scope for this
    # task but needed later. Use `id = 0` as a placeholder — the
    # orchestrator assigns real IDs (your value is discarded).
    [[new_tasks]]
    id = 0
    title = "…"
    description = """…"""
    test = """…"""
    estimated_tokens = 150000
    spec_refs = [{{ path = "artifacts/spec.md", lines = "10-30" }}]
    plan_refs = [{{ path = "artifacts/plan.md", lines = "50-70" }}]

Rules:
  - approved      → outcomes delivered AND (tests pass OR task is
                    "not testable" and the code builds cleanly). No new_tasks.
  - refine        → outcomes delivered, but you have small nice-to-have
                    suggestions (naming, cleanup, minor improvements that
                    aren't spec/plan violations). The task is accepted (no
                    re-review); your `feedback` items go to the NEXT
                    coder as opportunistic carryover — list things worth
                    surfacing, NOT things that must land before merge.
                    Use instead of `approved` when you genuinely have asks,
                    and instead of `revise` when nothing requires another
                    round. Requires ≥1 feedback item. No new_tasks.
  - revise        → list the specific issues. For complex tasks, also
                    suggest a direction (file/approach/sketch) — don't
                    just reject.
  - human_blocked → human judgement required; explain what's unclear.
  - agent_pivot   → autonomous recovery required; explain the pivot.
  - Don't repeat feedback from prior reviews unless the coder ignored it
    without good reason — in which case call that out explicitly.
{instr}"#,
        task_id = task_id,
        round = round,
        task = task_file.display(),
        spec = spec.display(),
        plan = plan.display(),
        review_scope = review_scope_file.display(),
        coder_summary_section = coder_summary_section,
        review_scope_text = review_scope_text,
        review = review_file.display(),
        instr = instr,
    )
}
