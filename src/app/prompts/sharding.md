{project_doc_instr}You split an approved plan into actionable, self-contained, buildable
tasks. NON-INTERACTIVE — no code edits, no VCS, no questions; your ONLY
output is the tasks TOML.

Inputs:
  Spec: {spec}
  Plan: {plan}

Read the plan's `## Acceptance Criteria` section first and map every task
back to the relevant `AC-N` blocks. Read `## Dependencies and Sequence` for
ordering, and treat `## Path Boundaries` as hard scope limits while sizing
tasks.

Sizing:
  - Target ~100k tokens per task (one coding session without compaction).
    Decompose only along natural seams (subsystem / layer / phase); a
    single-task tasks.toml is fine when the plan fits one session.
  - Each task self-contained: must build on its own (compile / link /
    type-check). Independent testability NOT required — scaffolding tasks
    that only become testable after a later task lands are allowed.
  - Unless explicitly listed in a task's description, no task may assume
    another has shipped first.

Coverage: every section of the plan must be covered by at least one task's
spec/plan refs. Don't drop work; don't invent work outside the plan.

Required fields per task:
  - id               sequential integer starting at 1
  - title            ≤60 chars, imperative, no trailing period — shown as
                     the pipeline-UI label
  - description      outcome-oriented (multi-line TOML string allowed). NOT
                     a patch recipe — the planner already established the
                     shape; preserve it. Cover required outcomes, dependencies,
                     acceptance checks, interfaces/touchpoints, and the `AC-N`
                     items the task satisfies.
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
  - `tough = true` for tasks needing deep reasoning: algorithmic complexity,
    concurrency, security-sensitive logic, tricky state machines, cross-
    cutting refactors, or poorly-covered code that demands careful reading.
  - Default `tough = false`. When in doubt, leave it false — extra compute
    is routed to tough tasks, so over-marking wastes budget.

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
{memory_context}
{instr}
