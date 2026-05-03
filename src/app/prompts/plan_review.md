{project_doc_instr}You review an implementation plan. NON-INTERACTIVE — no clarifying
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
{instr}