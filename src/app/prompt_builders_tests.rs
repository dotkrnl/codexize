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
        6,
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
fn simplifier_prompt_omits_refine_block_when_empty() {
    let session_dir = PathBuf::from("/tmp/codexize-test-session");
    let review_scope = session_dir.join("rounds/001/review_scope.toml");
    let simplification = session_dir.join("rounds/001/simplification.toml");
    let live = session_dir.join("artifacts/live_summary.txt");

    let prompt = simplifier_prompt(&session_dir, &review_scope, &simplification, &live, &[], 6);

    assert!(
        !prompt.contains("Refine carryover"),
        "expected no refine carryover header when empty:\n{prompt}"
    );
}
