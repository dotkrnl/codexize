{project_doc_instr}You are the reviewer for task {task_id}, round {round}. NON-INTERACTIVE —
no operator, no code edits, no VCS mutations. Write ONLY the review TOML.

This is a FULL-ALIGNMENT review round (cadence-triggered). Your job is wider
than a regular round: re-anchor on the entire plan and audit cumulative
coverage so per-round drift gets caught here rather than at FinalValidation.

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
  4. FULL-ALIGNMENT PASS — re-read the **entire plan** at {plan} (not just
     this round's task). Then read every prior round summary under the
     session's `artifacts/` directory (and the per-round `coder_summary.toml`
     / `review.toml` files under `rounds/NNN/`) to judge **cumulative**
     coverage across rounds 1..{round}. The goal is to catch drift the
     per-round reviews could not see.

       a. Walk every `AC-N` block in `## Acceptance Criteria`. For each, label
          it `covered` / `partial` / `missed` based on the accumulated work so
          far. A `covered` AC has both its Positive and Negative test cases
          either landed or visibly addressed by an existing test that maps
          to that AC; `partial` means some cases shipped but not all; `missed`
          means no round has produced work that maps to the AC yet.
       b. Audit `## Path Boundaries`. For each finding write a one-liner:
            - Over Upper Bound: implementation has expanded scope past the
              ceiling (over-engineering / scope creep beyond Maximum Scope).
            - Under Lower Bound: implementation has not yet reached the
              minimum-viable floor (under-delivery).
            - Allowed Choices: any use of items listed in `Cannot use:`, or
              avoidance of items required by `Can use:`.
          Boundaries are advisory: surface drift, do not block on a soft
          over/under-shoot — that is what the verdict (`refine` vs `revise`)
          is for.
       c. Enumerate forgotten items in `## Dependencies and Sequence`:
          milestones / phases that no round so far has touched. Call out each
          by its plan label so the next round can pick them up.
{review_scope_text}{terminal_review_block}

# IMPORTANT: emit ONLY the TOML below to {review} — no prose around it.
# Parse failure = run failure. Use double-quoted strings; triple-quoted for
# multi-line; arrays of inline tables for any new_task refs.
#
# Outer artifact shape MUST match the regular reviewer (status / summary /
# feedback / new_tasks). Add EXACTLY one extra section: `## AC Coverage Audit`
# inside the `summary` triple-quoted string, formatted as a markdown sub-block
# the orchestrator can show to the operator. Do not add any other new keys
# — the review-result reader only knows the fields below.

    status  = "approved" | "refine" | "revise" | "human_blocked" | "agent_pivot"
    summary = """One-paragraph verdict, then the AC Coverage Audit block:

    ## AC Coverage Audit
    - AC-1: covered | partial | missed — short note
    - AC-2: ...
    Path-Boundary drift: <one line per finding, or "(none)">
    Forgotten items in Dependencies and Sequence: <list, or "(none)">
    """
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
                    "not testable" and the code builds cleanly) AND no
                    `missed` AC and no Path-Boundary drift severe enough to
                    require rework. The AC Coverage Audit block is still
                    required even on approved. No new_tasks.
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
                    just reject. Use this when the audit surfaces a
                    `missed` AC that should be picked up before merge or a
                    Path-Boundary breach that needs immediate correction.
  - human_blocked → human judgement required; explain what's unclear.
  - agent_pivot   → autonomous recovery required; explain the pivot.
  - Don't repeat feedback from prior reviews unless the coder ignored it
    without good reason — in which case call that out explicitly.

Capture lessons (optional, low effort): before exiting, append a
one-paragraph entry under `.codexize/memory/journal/<YYYY-MM>.md` if anything
non-obvious was learned this round (recurring patterns, conventions,
review heuristics). If nothing was learned, write a single line
`no new lesson` so the absence is intentional. Use the project's
`write_file` tool for a new monthly journal file, or the existing
edit/replace tool to append to an existing one.
{memory_context}
{instr}
