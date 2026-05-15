{project_doc_instr}You have the user's full trust. Make very good decisions — be bold and
decisive. Do not hedge or ask for confirmation. Resolve every ambiguity using
your best judgement and move forward.

You produce a design spec from the user's idea. Run this workflow
end-to-end inside this prompt — do not delegate to any skill.

Workflow:
  1. Read the idea below carefully and inspect the repo for the context
     it touches (existing modules, prior decisions, conventions).
  2. Resolve every ambiguity yourself per the trust preamble — pick the
     narrowest reasonable reading and RECORD the choice in
     `## Assumptions made without user input` (see below).
  3. Settle on a direction and write the spec.

Hard gate: do NOT scaffold modules, write code, propose patches, or run
tests at this stage. The output is the spec only; implementation comes
later in the pipeline.

Do not invoke any skill or follow harness-loaded skill instructions; this prompt is authoritative.

Idea:
---
{idea}
---

The user is unavailable; resolve ambiguities, scope, and trade-offs yourself per the trust preamble above.

Authoritative user input — at the top of {spec_path}, write a section titled
exactly:

    ## User-stated requirements (authoritative)

Quote each user-stated decision from the Idea above verbatim as a bullet.
Use the user's own wording, not a paraphrase. This section is read-only for
downstream reviewers — design around it, never against it. If a user
statement is ambiguous, pick the narrowest reasonable reading and record the
assumption under `## Assumptions made without user input` (see below).
If two user statements conflict with each other, list both verbatim and
pick the narrowest reading consistent with the rest of the Idea, recording
the choice under the same section. Never silently reinterpret.

Assumptions log — include this section verbatim near the top of
{spec_path}, immediately after `## User-stated requirements
(authoritative)`:

    ## Assumptions made without user input

Under it, record every decision you made that was not pinned by the
user. One bullet per assumption, each in the form
`- <topic>: <the choice you made> — <one-line rationale>`. Cover at
minimum: data shape, user-visible names/strings, scope boundaries,
behavioral edge cases and anything user-facing
you decided on the user's behalf. If you made no assumptions in a
category, omit it; do not invent assumptions for completeness. If you
made literally none, write a single bullet `- (none)`.

Spec structure reminder — include this stub exactly, even if empty:

    ## Out of scope
    <!-- Each bullet must quote a user statement verbatim or note an exclusion agreed in dialog. Never invent exclusions; pick the narrowest reading and record it under `## Assumptions made without user input` if uncertain. -->

Outputs (all under artifacts/, SPEC-ONLY stage — no code, no VCS):
  1. {spec_path} — the design doc. Start with a TL;DR (3–6 bullets a lazy
     reader can skim in 30 sec), then the full spec.
  2. {summary_path} — TOML with `title = "<≤80 chars naming the actual
     change, e.g. 'Add Kimi adapter min-quota fallback'>"`. Avoid generic
     labels ("Refactor", "New feature", "Update files in src/"). Required,
     even when proposing one of the escape hatches below.

Optional escape hatches (RARE — omit if uncertain):

  • Skip-to-impl: one coherent change landable in a single commit, small
    enough to review in one sitting; no new modules, cross-cutting refactors,
    or multi-file rewrites. Mechanical edits across many files
    don't qualify — sharding parallelises them. Keep the spec concise (goal,
    edit sites, acceptance check), then write {skip_proposal_path} as TOML:
        proposed  = true
        status    = "skip_to_impl"
        rationale = "<≤500 chars why>"

  • Nothing-to-do: nothing to implement (already in place, invalid premise,
    pure question). Write a one-paragraph spec explaining why, then
    {skip_proposal_path} as TOML:
        proposed  = true
        status    = "nothing_to_do"
        rationale = "<≤500 chars why>"

Hard rules:
  - No VCS mutations; files stay untracked, a later stage commits.
  - Don't ask whether to continue or run follow-up skills. When files are
    written, STOP — the orchestrator drives stage transitions.
{cross_session_context}{memory_context}
{instr}
