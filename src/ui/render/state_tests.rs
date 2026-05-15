use super::*;
use crate::{
    state::{NodeKind, NodeStatus},
    ui::widgets::tree::view::NodeKey,
};

fn row(depth: usize) -> VisibleNodeRow {
    VisibleNodeRow {
        depth,
        path: Vec::new(),
        key: NodeKey::new(Vec::new()),
        kind: NodeKind::Stage,
        status: NodeStatus::Done,
        has_children: false,
        has_transcript: false,
        has_body: false,
        backing_leaf_run_id: None,
    }
}

#[test]
fn row_is_not_last_sibling_when_next_peer_has_same_depth() {
    let rows = vec![row(0), row(1), row(2), row(1)];

    assert!(!is_last_sibling(&rows, 1));
}

#[test]
fn row_is_last_sibling_when_next_boundary_is_ancestor() {
    let rows = vec![row(0), row(1), row(2), row(0)];

    assert!(is_last_sibling(&rows, 1));
}

#[test]
fn sanitize_live_summary_collapses_and_truncates() {
    let text = format!("\u{1b}[31m{}\u{1b}[0m", "x ".repeat(600));
    assert_eq!(sanitize_live_summary(&text).chars().count(), 500);
    assert!(!sanitize_live_summary("a\nb").contains('\n'));
}

#[test]
fn skip_to_impl_content_uses_no_rationale_fallback() {
    let lines = skip_to_impl_content(None, None, 80);
    assert!(format!("{lines:?}").contains("(no rationale provided)"));
}

#[test]
fn guard_content_shortens_heads() {
    let decision = PendingGuardDecision {
        stage: "coder".to_string(),
        task_id: Some(1),
        round: 1,
        attempt: 1,
        run_id: 1,
        captured_head: "abcdef123456".to_string(),
        current_head: "9876543210".to_string(),
        warnings: Vec::new(),
    };
    let lines = guard_content(Some(&decision));
    assert!(format!("{lines:?}").contains("abcdef1"));
    assert!(format!("{lines:?}").contains("9876543"));
}

#[test]
fn stage_error_content_uses_default_error_details() {
    let lines = stage_error_content(StageId::Brainstorm, None, 80);
    assert!(format!("{lines:?}").contains("(no error details)"));
}

fn lines_text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn validation_report_renders_goal_met_summary_and_findings() {
    use crate::data::validation::{ValidationStatus, ValidationVerdict};
    let verdict = ValidationVerdict {
        status: ValidationStatus::GoalMet,
        summary: "All goals achieved".to_string(),
        findings: vec!["Inspected src/ and tests/".to_string()],
        gaps: Vec::new(),
        new_tasks: Vec::new(),
        dream_recommendation: None,
        dream_reason: None,
    };
    let text = lines_text(&final_validation_report_lines(&verdict, "", 200));
    assert!(text.contains("goal_met"));
    assert!(text.contains("All goals achieved"));
    assert!(text.contains("Inspected src/ and tests/"));
}

#[test]
fn validation_report_renders_dream_recommendation() {
    use crate::data::validation::{DreamRecommendation, ValidationStatus, ValidationVerdict};
    let verdict = ValidationVerdict {
        status: ValidationStatus::GoalMet,
        summary: "All goals achieved".to_string(),
        findings: Vec::new(),
        gaps: Vec::new(),
        new_tasks: Vec::new(),
        dream_recommendation: Some(DreamRecommendation::Suggest),
        dream_reason: Some("Memory lessons should be consolidated.".to_string()),
    };
    let text = lines_text(&final_validation_report_lines(&verdict, "", 200));
    assert!(text.contains("Dreaming: suggest"));
    assert!(text.contains("Dream reason: Memory lessons should be consolidated."));
}

#[test]
fn validation_report_renders_goal_gap_with_citations_and_tasks() {
    use crate::data::validation::{Gap, ValidationStatus, ValidationVerdict, ValidatorGapTask};
    let verdict = ValidationVerdict {
        status: ValidationStatus::GoalGap,
        summary: "Missing retry logic".to_string(),
        findings: Vec::new(),
        gaps: vec![Gap {
            description: "No retry on HTTP 503".to_string(),
            checked: vec!["src/client.rs".to_string(), "src/lib.rs".to_string()],
        }],
        new_tasks: vec![ValidatorGapTask {
            title: "Add backoff".to_string(),
            description: "Wire backoff".to_string(),
            test: "cargo test retry::".to_string(),
            estimated_tokens: 5000,
        }],
        dream_recommendation: None,
        dream_reason: None,
    };
    let text = lines_text(&final_validation_report_lines(&verdict, "", 200));
    assert!(text.contains("goal_gap"));
    assert!(text.contains("No retry on HTTP 503"));
    assert!(text.contains("src/client.rs"));
    assert!(text.contains("src/lib.rs"));
    assert!(text.contains("Add backoff"));
}

#[test]
fn validation_report_renders_needs_human_with_gap_citations() {
    use crate::data::validation::{Gap, ValidationStatus, ValidationVerdict};
    let verdict = ValidationVerdict {
        status: ValidationStatus::NeedsHuman,
        summary: "Operator must decide".to_string(),
        findings: vec!["clean tree".to_string()],
        gaps: vec![Gap {
            description: "A or B?".to_string(),
            checked: vec!["artifacts/spec.md".to_string()],
        }],
        new_tasks: Vec::new(),
        dream_recommendation: None,
        dream_reason: None,
    };
    let text = lines_text(&final_validation_report_lines(&verdict, "", 200));
    assert!(text.contains("needs_human"));
    assert!(text.contains("Operator must decide"));
    assert!(text.contains("A or B?"));
    assert!(text.contains("artifacts/spec.md"));
}

#[test]
fn validation_report_wraps_long_summary_to_width() {
    // Bug regression: a verdict whose summary exceeds the body width used
    // to render as a single overflowing line because
    // `final_validation_report_lines` ignored the available width.
    use crate::data::validation::{ValidationStatus, ValidationVerdict};
    const WIDTH: usize = 30;
    let summary =
        "This is an intentionally long summary that should be wrapped across several rows"
            .to_string();
    let verdict = ValidationVerdict {
        status: ValidationStatus::GoalMet,
        summary: summary.clone(),
        findings: Vec::new(),
        gaps: Vec::new(),
        new_tasks: Vec::new(),
        dream_recommendation: None,
        dream_reason: None,
    };
    let lines = final_validation_report_lines(&verdict, "", WIDTH);
    // No printable line may exceed the requested width.
    for line in &lines {
        let printable: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            printable.chars().count() <= WIDTH,
            "report line {printable:?} ({} chars) exceeds width {WIDTH}",
            printable.chars().count()
        );
    }
    // Every word of the summary must survive the wrap intact.
    let text = lines_text(&lines);
    for word in summary.split_whitespace() {
        assert!(text.contains(word), "missing summary word {word:?}");
    }
}

#[test]
fn validation_report_wraps_long_finding_and_gap_description() {
    use crate::data::validation::{Gap, ValidationStatus, ValidationVerdict};
    const WIDTH: usize = 30;
    let long_finding = "audited every module under src/ and verified each public API".to_string();
    let long_gap =
        "The pipeline still allows agents to short-circuit retries past the cap".to_string();
    let verdict = ValidationVerdict {
        status: ValidationStatus::GoalGap,
        summary: "ok".to_string(),
        findings: vec![long_finding.clone()],
        gaps: vec![Gap {
            description: long_gap.clone(),
            checked: Vec::new(),
        }],
        new_tasks: Vec::new(),
        dream_recommendation: None,
        dream_reason: None,
    };
    let lines = final_validation_report_lines(&verdict, "", WIDTH);
    for line in &lines {
        let printable: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            printable.chars().count() <= WIDTH,
            "report line {printable:?} exceeds width {WIDTH}"
        );
    }
    let text = lines_text(&lines);
    for word in long_finding.split_whitespace() {
        assert!(text.contains(word), "missing finding word {word:?}");
    }
    for word in long_gap.split_whitespace() {
        assert!(text.contains(word), "missing gap word {word:?}");
    }
}
