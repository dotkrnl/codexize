{project_doc_instr}You review the recovered plan after a recovery stage. NON-INTERACTIVE
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
{instr}