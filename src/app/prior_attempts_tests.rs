use super::*;
use crate::adapters::EffortLevel;
use crate::state::{LaunchModes, MessageSender, RunRecord, RunStatus};
use chrono::{TimeZone, Utc};

fn sample_run(id: u64, stage: &str, round: u32, attempt: u32, error: Option<&str>) -> RunRecord {
    RunRecord {
        id,
        stage: stage.to_string(),
        task_id: None,
        round,
        attempt,
        model: format!("model-{id}"),
        vendor: "openai".to_string(),
        window_name: format!("[{stage} a{attempt}]"),
        started_at: Utc.with_ymd_and_hms(2026, 5, 8, 0, 0, 0).unwrap(),
        ended_at: None,
        status: if error.is_some() {
            RunStatus::Failed
        } else {
            RunStatus::Done
        },
        error: error.map(|s| s.to_string()),
        effort: EffortLevel::Normal,
        modes: LaunchModes::default(),
        hostname: None,
        mount_device_id: None,
        section_path: None,
    }
}

fn msg(run_id: u64, kind: MessageKind, text: &str) -> Message {
    Message {
        ts: Utc.with_ymd_and_hms(2026, 5, 8, 0, 0, 0).unwrap(),
        run_id,
        kind,
        sender: match kind {
            MessageKind::UserInput => MessageSender::System,
            _ => MessageSender::Agent {
                model: "model-1".into(),
                vendor: "openai".into(),
            },
        },
        text: text.to_string(),
    }
}

#[test]
fn no_prior_runs_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let result = write_prior_attempts_transcript(dir.path(), &[], &[], "brainstorm", 1);
    assert!(result.is_none());
    assert!(
        !dir.path().join("prompts").exists(),
        "no prompts dir should be created when there is nothing to write"
    );
}

#[test]
fn prior_runs_without_relevant_messages_returns_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let runs = vec![sample_run(1, "brainstorm", 1, 1, Some("missing artifact"))];
    let messages = vec![
        // Started/End/Brief are not part of the user-visible conversation.
        msg(1, MessageKind::Started, "agent started"),
        msg(1, MessageKind::Brief, "summary"),
    ];
    let result = write_prior_attempts_transcript(dir.path(), &messages, &runs, "brainstorm", 1);
    assert!(result.is_none());
}

#[test]
fn writes_one_section_per_attempt_in_order() {
    let dir = tempfile::TempDir::new().unwrap();
    let runs = vec![
        sample_run(2, "brainstorm", 1, 2, Some("exit code 1")),
        sample_run(1, "brainstorm", 1, 1, Some("missing artifact")),
        sample_run(3, "brainstorm", 1, 3, Some("aborted by user")),
    ];
    let messages = vec![
        msg(1, MessageKind::UserInput, "use sqlite"),
        msg(1, MessageKind::AgentText, "got it. what schema?"),
        msg(2, MessageKind::UserInput, "two tables"),
        msg(2, MessageKind::AgentText, "tables: users, posts"),
        msg(3, MessageKind::AgentThought, "considering migration"),
    ];
    let path = write_prior_attempts_transcript(dir.path(), &messages, &runs, "brainstorm", 1)
        .expect("transcript should be written");
    assert_eq!(
        path,
        dir.path()
            .join("prompts")
            .join("brainstorm-prior-attempts-r1.md")
    );
    let body = std::fs::read_to_string(&path).unwrap();
    let a1 = body.find("## Attempt 1").unwrap();
    let a2 = body.find("## Attempt 2").unwrap();
    let a3 = body.find("## Attempt 3").unwrap();
    assert!(
        a1 < a2 && a2 < a3,
        "attempts must render in ascending order"
    );
    assert!(body.contains("**operator:** use sqlite"));
    assert!(body.contains("**agent:** got it. what schema?"));
    assert!(body.contains("**operator:** two tables"));
    assert!(body.contains("**agent (thought):** considering migration"));
    assert!(body.contains("failed: missing artifact"));
    assert!(body.contains("failed: exit code 1"));
    assert!(body.contains("failed: aborted by user"));
}

#[test]
fn filters_by_stage_and_round() {
    let dir = tempfile::TempDir::new().unwrap();
    let runs = vec![
        sample_run(1, "brainstorm", 1, 1, Some("x")),
        sample_run(2, "planning", 1, 1, Some("x")),
        sample_run(3, "recovery", 4, 1, Some("x")),
        sample_run(4, "recovery", 5, 1, Some("x")),
    ];
    let messages = vec![
        msg(1, MessageKind::UserInput, "brainstorm input"),
        msg(2, MessageKind::UserInput, "planning input"),
        msg(3, MessageKind::UserInput, "recovery r4 input"),
        msg(4, MessageKind::UserInput, "recovery r5 input"),
    ];
    let path = write_prior_attempts_transcript(dir.path(), &messages, &runs, "recovery", 5)
        .expect("recovery r5 transcript should be written");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("recovery r5 input"));
    assert!(!body.contains("brainstorm input"));
    assert!(!body.contains("planning input"));
    assert!(
        !body.contains("recovery r4 input"),
        "different round must not leak into transcript"
    );
}

#[test]
fn ignores_messages_for_unrelated_runs_and_skips_empty_attempts() {
    let dir = tempfile::TempDir::new().unwrap();
    let runs = vec![
        sample_run(10, "brainstorm", 1, 1, Some("x")),
        sample_run(11, "brainstorm", 1, 2, Some("x")),
    ];
    let messages = vec![
        // Attempt 1 has visible Q&A.
        msg(10, MessageKind::UserInput, "answer A"),
        msg(10, MessageKind::AgentText, "agent reply"),
        // Attempt 2 produced nothing visible — only a Started.
        msg(11, MessageKind::Started, "agent started"),
        // Stray message attached to an unrelated run id.
        msg(99, MessageKind::UserInput, "should not appear"),
    ];
    let path = write_prior_attempts_transcript(dir.path(), &messages, &runs, "brainstorm", 1)
        .expect("attempt 1 has content; transcript must be written");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("## Attempt 1"));
    assert!(body.contains("answer A"));
    assert!(
        !body.contains("## Attempt 2"),
        "attempts with no user/agent text must be omitted"
    );
    assert!(!body.contains("should not appear"));
}
