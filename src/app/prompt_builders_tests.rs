use super::*;
use std::path::PathBuf;

#[test]
fn simplifier_prompt_includes_refine_carryover() {
    let session_dir = PathBuf::from("/tmp/codexize-test-session");
    let review_scope = session_dir.join("rounds/001/review_scope.toml");
    let simplification = session_dir.join("rounds/001/simplification.toml");
    let live = session_dir.join("artifacts/live_summary.txt");
    let carryover = vec![
        "rename foo to bar".to_string(),
        "tighten error handling".to_string(),
    ];

    let prompt = simplifier_prompt(
        &session_dir,
        &review_scope,
        &simplification,
        &live,
        &carryover,
        PromptMeta::with_topics(6),
    );

    assert!(
        prompt.contains("rename foo to bar"),
        "expected first refine carryover item in prompt:\n{prompt}"
    );
    assert!(
        prompt.contains("tighten error handling"),
        "expected second refine carryover item in prompt:\n{prompt}"
    );
}

#[test]
fn brainstorm_prompt_inlines_prior_attempts_pointer_when_supplied() {
    // Wires the prior_attempts_path argument all the way through to the
    // rendered template. The block must point at the supplied path and
    // tell the agent not to re-ask answered questions.
    let prior = PathBuf::from("/tmp/codexize-test-session/prompts/brainstorm-prior-attempts-r1.md");
    let prompt = brainstorm_prompt(
        "fictional idea",
        "/tmp/codexize-test-session/artifacts/spec.md",
        "/tmp/codexize-test-session/artifacts/session_summary.toml",
        "/tmp/codexize-test-session/artifacts/live.txt",
        false,
        Some(&prior),
        PromptMeta::with_topics(6),
    );
    assert!(
        prompt.contains("brainstorm-prior-attempts-r1.md"),
        "prior-attempts path must appear in rendered prompt:\n{prompt}"
    );
    assert!(
        prompt.contains("Do NOT re-ask"),
        "directive against re-asking must appear in rendered prompt:\n{prompt}"
    );
}

#[test]
fn brainstorm_prompt_omits_prior_attempts_block_when_none() {
    let prompt = brainstorm_prompt(
        "fictional idea",
        "/tmp/codexize-test-session/artifacts/spec.md",
        "/tmp/codexize-test-session/artifacts/session_summary.toml",
        "/tmp/codexize-test-session/artifacts/live.txt",
        false,
        None,
        PromptMeta::with_topics(6),
    );
    assert!(
        !prompt.contains("Prior failed attempts"),
        "no prior-attempts block expected when path is None:\n{prompt}"
    );
    assert!(
        !prompt.contains("{prior_attempts_block}"),
        "the placeholder must be substituted away even when empty:\n{prompt}"
    );
}

#[test]
fn simplifier_prompt_omits_refine_block_when_empty() {
    let session_dir = PathBuf::from("/tmp/codexize-test-session");
    let review_scope = session_dir.join("rounds/001/review_scope.toml");
    let simplification = session_dir.join("rounds/001/simplification.toml");
    let live = session_dir.join("artifacts/live_summary.txt");

    let prompt = simplifier_prompt(&session_dir, &review_scope, &simplification, &live, &[], PromptMeta::with_topics(6));

    assert!(
        !prompt.contains("Refine carryover"),
        "expected no refine carryover header when empty:\n{prompt}"
    );
}
