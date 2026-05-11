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
        &[],
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
        &[],
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
fn brainstorm_prompt_includes_earlier_waiting_specs() {
    let earlier_specs = vec![
        PathBuf::from("/tmp/codexize/sessions/01-earlier/artifacts/spec.md"),
        PathBuf::from("/tmp/codexize/sessions/02-earlier/artifacts/spec.md"),
    ];
    let prompt = brainstorm_prompt(
        "fictional idea",
        "/tmp/codexize-test-session/artifacts/spec.md",
        "/tmp/codexize-test-session/artifacts/session_summary.toml",
        "/tmp/codexize-test-session/artifacts/live.txt",
        false,
        None,
        &earlier_specs,
        PromptMeta::with_topics(6),
    );
    assert!(
        prompt.contains("Expected future repository state"),
        "header missing:\n{prompt}"
    );
    assert!(
        prompt.contains("01-earlier/artifacts/spec.md"),
        "first spec missing:\n{prompt}"
    );
    assert!(
        prompt.contains("02-earlier/artifacts/spec.md"),
        "second spec missing:\n{prompt}"
    );
    assert!(
        prompt.contains("MUST flag the conflict"),
        "conflict flagging instruction missing:\n{prompt}"
    );
}

#[test]
fn spec_review_prompt_includes_earlier_waiting_specs() {
    let earlier_specs = vec![PathBuf::from(
        "/tmp/codexize/sessions/01-earlier/artifacts/spec.md",
    )];
    let prompt = spec_review_prompt(
        "/tmp/codexize-test-session/artifacts/spec.md",
        "/tmp/codexize-test-session/artifacts/spec-review-1.md",
        "/tmp/codexize-test-session/artifacts/live.txt",
        &earlier_specs,
        PromptMeta::with_topics(6),
    );
    assert!(
        prompt.contains("Expected future repository state"),
        "header missing:\n{prompt}"
    );
    assert!(
        prompt.contains("01-earlier/artifacts/spec.md"),
        "spec path missing:\n{prompt}"
    );
    assert!(
        prompt.contains("MUST flag the conflict"),
        "conflict flagging instruction missing:\n{prompt}"
    );
}

#[test]
fn planning_prompt_does_not_splice_spec_review_paths_or_bodies() {
    // The planner now runs strictly against the spec. The builder must
    // neither accept nor render any spec-review-*.md content even if such
    // a file exists on disk next to the spec.
    let session_dir = PathBuf::from("/tmp/codexize-test-planning-no-reviews");
    let artifacts = session_dir.join("artifacts");
    let spec = artifacts.join("spec.md");
    let plan = artifacts.join("plan.md");
    let live = artifacts.join("live.txt");

    let prompt = planning_prompt(
        &spec,
        &plan,
        &live,
        false,
        None,
        &[],
        PromptMeta::with_topics(6),
    );

    assert!(
        prompt.contains(&spec.display().to_string()),
        "planner prompt must reference the spec path:\n{prompt}"
    );
    assert!(
        !prompt.contains("spec-review-"),
        "planner prompt must not name any spec-review-*.md file:\n{prompt}"
    );
}

#[test]
fn planning_prompt_yolo_variant_also_omits_spec_review_splice() {
    let session_dir = PathBuf::from("/tmp/codexize-test-planning-no-reviews-yolo");
    let artifacts = session_dir.join("artifacts");
    let spec = artifacts.join("spec.md");
    let plan = artifacts.join("plan.md");
    let live = artifacts.join("live.txt");

    let prompt = planning_prompt(
        &spec,
        &plan,
        &live,
        true,
        None,
        &[],
        PromptMeta::with_topics(6),
    );

    assert!(
        !prompt.contains("spec-review-"),
        "yolo planner prompt must not name any spec-review-*.md file:\n{prompt}"
    );
}

#[test]
fn planning_prompt_includes_earlier_waiting_specs() {
    let session_dir = PathBuf::from("/tmp/codexize-test-planning-future-state");
    let artifacts = session_dir.join("artifacts");
    let spec = artifacts.join("spec.md");
    let plan = artifacts.join("plan.md");
    let live = artifacts.join("live.txt");
    let earlier_specs = vec![PathBuf::from(
        "/tmp/codexize/sessions/01-earlier/artifacts/spec.md",
    )];

    let prompt = planning_prompt(
        &spec,
        &plan,
        &live,
        false,
        None,
        &earlier_specs,
        PromptMeta::with_topics(6),
    );

    assert!(
        prompt.contains("Expected future repository state"),
        "header missing:\n{prompt}"
    );
    assert!(
        prompt.contains("01-earlier/artifacts/spec.md"),
        "spec path missing:\n{prompt}"
    );
    assert!(
        prompt.contains("planning against"),
        "planning-specific framing missing:\n{prompt}"
    );
    // Planning must NOT receive instructions to "flag conflicts" (brainstorm/spec review only)
    assert!(
        !prompt.contains("MUST flag the conflict"),
        "planning must not receive brainstorm-only conflict instructions:\n{prompt}"
    );
}

#[test]
fn simplifier_prompt_omits_refine_block_when_empty() {
    let session_dir = PathBuf::from("/tmp/codexize-test-session");
    let review_scope = session_dir.join("rounds/001/review_scope.toml");
    let simplification = session_dir.join("rounds/001/simplification.toml");
    let live = session_dir.join("artifacts/live_summary.txt");

    let prompt = simplifier_prompt(
        &session_dir,
        &review_scope,
        &simplification,
        &live,
        &[],
        PromptMeta::with_topics(6),
    );

    assert!(
        !prompt.contains("Refine carryover"),
        "expected no refine carryover header when empty:\n{prompt}"
    );
}
