{project_doc_instr}You review a spec. NON-INTERACTIVE — no questions, code edits, VCS, or test runs. Write ONLY the review file.

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
  - The `## User-stated requirements (authoritative)` section is read-only:
    flag any contradiction against the offending spec section, never that
    section itself, and never propose edits to it.
  - Bottom-line judgement on the last line: ship-as-is / needs-revision /
    reject.
{memory_context}
{instr}