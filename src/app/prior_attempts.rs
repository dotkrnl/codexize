//! Prior-attempt transcript writer for interactive stages.
//!
//! When an interactive run (brainstorm / planning / interactive recovery)
//! fails and the operator retries, the prior conversation — clarifying
//! questions the agent already asked, decisions the operator already typed —
//! used to vanish from the next attempt's prompt: the operator was forced
//! to re-answer the same questions to land back at the same understanding.
//!
//! This module renders a per-stage markdown transcript of every prior
//! attempt's `UserInput` + `AgentText` + `AgentThought` messages and writes
//! it to `{session_dir}/prompts/{stage}-prior-attempts-r{round}.md`. The
//! interactive prompt templates point the new agent at that file with
//! instructions to read it before re-asking anything.
//!
//! Scope is `(stage, round)`: brainstorm's three failed retries become three
//! sections in the same file; recovery round 5's history is independent of
//! round 4's because the underlying builder problem is different.
use crate::state::{Message, MessageKind, RunRecord, RunStatus};
use std::path::{Path, PathBuf};

/// Inspect prior `RunRecord`s for the same `(stage, round)` and emit a
/// markdown transcript at `{session_dir}/prompts/{stage}-prior-attempts-r{round}.md`.
///
/// Returns the file path when at least one prior attempt produced a useful
/// message; returns `None` (and writes nothing) when there are no prior
/// runs or none of them produced any user/agent messages — the caller's
/// prompt builder treats `None` as "render the empty block."
pub(crate) fn write_prior_attempts_transcript(
    session_dir: &Path,
    messages: &[Message],
    runs: &[RunRecord],
    stage: &str,
    round: u32,
) -> Option<PathBuf> {
    let mut prior: Vec<&RunRecord> = runs
        .iter()
        .filter(|run| run.stage == stage && run.round == round)
        .collect();
    if prior.is_empty() {
        return None;
    }
    prior.sort_by_key(|run| run.attempt);
    let mut sections: Vec<String> = Vec::new();
    for run in &prior {
        let body = render_attempt_section(run, messages);
        if !body.is_empty() {
            sections.push(body);
        }
    }
    if sections.is_empty() {
        return None;
    }
    let mut out = String::new();
    out.push_str(&format!(
        "# Prior {stage} attempts (round {round})\n\n\
        Read every section before sending your first message. Each was a \
        previous run on this same stage that failed or was aborted. The \
        operator already typed those answers — do not ask them again. \
        Build on what's there: take their stated decisions as authoritative, \
        only ask follow-ups that genuinely block the spec.\n\n"
    ));
    out.push_str(&sections.join("\n---\n\n"));
    let path = session_dir
        .join("prompts")
        .join(format!("{stage}-prior-attempts-r{round}.md"));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&path, out).is_err() {
        return None;
    }
    Some(path)
}

fn render_attempt_section(run: &RunRecord, messages: &[Message]) -> String {
    let mut buf = String::new();
    let outcome = match (run.status, run.error.as_deref()) {
        (RunStatus::Done, _) => "completed".to_string(),
        (_, Some(reason)) if !reason.trim().is_empty() => format!("failed: {}", reason.trim()),
        _ => "failed".to_string(),
    };
    buf.push_str(&format!(
        "## Attempt {} — {} ({})\n\n",
        run.attempt, run.model, outcome
    ));
    let mut wrote_any = false;
    for msg in messages.iter().filter(|m| m.run_id == run.id) {
        let label = match msg.kind {
            MessageKind::UserInput => "operator",
            MessageKind::AgentText => "agent",
            MessageKind::AgentThought => "agent (thought)",
            _ => continue,
        };
        let text = msg.text.trim();
        if text.is_empty() {
            continue;
        }
        buf.push_str(&format!("**{label}:** {text}\n\n"));
        wrote_any = true;
    }
    if !wrote_any {
        // Skip the section entirely when an attempt produced no relevant
        // messages — leaving an empty header on disk would tell the next
        // agent there's recoverable context where there isn't.
        return String::new();
    }
    buf
}

#[cfg(test)]
#[path = "prior_attempts_tests.rs"]
mod tests;
