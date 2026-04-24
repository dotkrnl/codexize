# Schema v2 Migration

`codexize` now writes session schema v2.

Schema v2 changes:

- `session.toml` requires `schema_version = 2`.
- Agent execution metadata lives in `agent_runs`.
- Chat history lives in session-level `messages.jsonl`.
- Legacy per-phase fields such as `phase_models`, reviewer lists, and `PhaseAttempt.live_summary` are gone.

Operational impact:

- Existing schema v1 sessions are not migrated.
- Opening a v1 session now fails with:
  `session <id> uses schema v1; archive with codexize archive <id> and start fresh.`
- Corrupt lines in `messages.jsonl` are skipped during load so one bad append does not brick the session.

Recommended workflow:

1. Archive any old session you still want to keep: `codexize archive <id>`.
2. Start or resume only schema v2 sessions created by the current build.
3. If a session is interrupted mid-run, restart the TUI. The current `Running` run is resumed if its tmux window still exists; otherwise it is finalized as failed with a synthetic end message.

Reviewer notes:

- `messages.jsonl` is append-only and orchestrator-owned.
- Single-run stages can now render chat directly through collapsed tree nodes via `leaf_run_id`.
- Builder review verdicts still have one ambiguity: when a reviewer returns `revise`, the implementation currently advances to the next builder round to preserve round-as-iteration semantics. See the inline reviewer comment in `src/app/mod.rs` if that rule should change.
