use codexize::logic::validation::{ValidationStatus, parse_verdict_toml};

#[test]
fn parse_verdict_toml_validates_goal_gap_without_fs_access() {
    let verdict = parse_verdict_toml(
        r#"status = "goal_gap"
summary = "Missing error handling"

[[gaps]]
description = "No retry logic in the client"
checked = ["src/client.rs"]

[[new_tasks]]
title = "Add retry logic"
description = "Wire exponential backoff into the HTTP client"
test = "cargo test retry::"
estimated_tokens = 5000
"#,
    )
    .expect("goal gap verdict should parse");

    assert_eq!(verdict.status, ValidationStatus::GoalGap);
    assert_eq!(verdict.gaps.len(), 1);
    assert_eq!(verdict.new_tasks.len(), 1);
}
