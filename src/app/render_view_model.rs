use super::tree::VisibleNodeRow;
use super::{ModalKind, StageId};
use crate::state::{NodeStatus, PendingGuardDecision};
use crate::tui::wrap_input;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Determines whether the row at `index` is the last sibling at its depth.
///
/// Scans forward from `index + 1` until a row with depth <= current depth is
/// found. Returns true if no such row exists or if the found row has depth less
/// than the current depth.
pub(super) fn is_last_sibling(visible_rows: &[VisibleNodeRow], index: usize) -> bool {
    let cur_depth = visible_rows[index].depth;
    visible_rows[index + 1..]
        .iter()
        .find(|r| r.depth <= cur_depth)
        .map(|r| r.depth < cur_depth)
        .unwrap_or(true)
}

pub(super) fn spinner_frame(count: usize) -> &'static str {
    SPINNER[count % SPINNER.len()]
}

pub(super) fn status_highlight_bg(status: NodeStatus) -> Option<Color> {
    match status {
        NodeStatus::Running => Some(Color::Cyan),
        NodeStatus::Done => Some(Color::Green),
        NodeStatus::Failed => Some(Color::Red),
        NodeStatus::FailedUnverified => Some(Color::LightYellow),
        NodeStatus::Pending | NodeStatus::WaitingUser | NodeStatus::Skipped => None,
    }
}

fn strip_ansi_codes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Render the parsed final-validation verdict for the dashboard body.
///
/// Always emits the full report (status, summary, findings, gaps with
/// citations, and any pushed gap tasks) regardless of verdict — per spec,
/// goal validation is high-trust and the operator should always see what
/// the validator checked.
pub fn final_validation_report_lines(
    verdict: &crate::final_validation::ValidationVerdict,
    indent: &str,
) -> Vec<Line<'static>> {
    use crate::final_validation::ValidationStatus;
    let dim = Style::default().fg(Color::DarkGray);
    let (status_text, status_style) = match verdict.status {
        ValidationStatus::GoalMet => (
            "goal_met",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        ValidationStatus::GoalGap => (
            "goal_gap",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        ValidationStatus::NeedsHuman => (
            "needs_human",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(indent.to_string(), dim),
        Span::styled(
            "Validation report",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · ", dim),
        Span::styled(status_text.to_string(), status_style),
    ]));
    lines.push(Line::from(vec![
        Span::styled(indent.to_string(), dim),
        Span::styled("Summary: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(verdict.summary.clone()),
    ]));
    if !verdict.findings.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(indent.to_string(), dim),
            Span::styled("Findings:", Style::default().add_modifier(Modifier::BOLD)),
        ]));
        for finding in &verdict.findings {
            lines.push(Line::from(vec![
                Span::styled(format!("{indent}  • "), dim),
                Span::raw(finding.clone()),
            ]));
        }
    }
    if !verdict.gaps.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(indent.to_string(), dim),
            Span::styled("Gaps:", Style::default().add_modifier(Modifier::BOLD)),
        ]));
        for gap in &verdict.gaps {
            lines.push(Line::from(vec![
                Span::styled(format!("{indent}  • "), dim),
                Span::raw(gap.description.clone()),
            ]));
            for citation in &gap.checked {
                lines.push(Line::from(vec![
                    Span::styled(format!("{indent}      checked: "), dim),
                    Span::styled(citation.clone(), Style::default().fg(Color::Cyan)),
                ]));
            }
        }
    }
    if !verdict.new_tasks.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(indent.to_string(), dim),
            Span::styled(
                "Follow-up tasks:",
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        for task in &verdict.new_tasks {
            lines.push(Line::from(vec![
                Span::styled(format!("{indent}  • "), dim),
                Span::raw(task.title.clone()),
            ]));
        }
    }
    lines
}

/// Banner shown when `BlockedNeedsUser` was entered with
/// `block_origin = FinalValidation`. Highlights the two prominent
/// affordances (force-ship → Done, rewind to spec review) while explicitly
/// noting that the other blocked-state transitions remain available.
pub fn final_validation_block_banner_lines(width: u16) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let highlight = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let bar = "─".repeat(width.max(1) as usize);
    vec![
        Line::from(Span::styled(bar.clone(), dim)),
        Line::from(vec![
            Span::styled(
                "Final validation blocked. ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw("Operator action required."),
        ]),
        Line::from(vec![
            Span::styled("→ ", highlight),
            Span::styled("Force ship to Done", highlight),
            Span::styled("    ", Style::default()),
            Span::styled("→ ", highlight),
            Span::styled("Rewind to spec review", highlight),
        ]),
        Line::from(Span::styled(
            "Other blocked-state transitions remain available.".to_string(),
            dim,
        )),
        Line::from(Span::styled(bar, dim)),
    ]
}

pub fn sanitize_live_summary(text: &str) -> String {
    let stripped = strip_ansi_codes(text);
    let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(500).collect()
}

pub(super) fn skip_to_impl_content(
    rationale: Option<&str>,
    kind: Option<crate::artifacts::SkipToImplKind>,
    width: u16,
) -> Vec<Line<'static>> {
    use crate::artifacts::SkipToImplKind;

    let is_nothing = kind == Some(SkipToImplKind::NothingToDo);
    let header = if is_nothing {
        "The brainstorm agent found nothing to implement."
    } else {
        "The brainstorm agent proposes skipping directly to implementation."
    };

    let rationale_text = rationale
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(no rationale provided)");
    let rationale_lines = wrap_input(rationale_text, width.max(1) as usize);

    let mut lines = vec![
        Line::from(Span::styled(
            header.to_string(),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Rationale: ".to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ];
    for line in rationale_lines {
        lines.push(Line::from(line));
    }

    lines
}

pub(super) fn guard_content(decision: Option<&PendingGuardDecision>) -> Vec<Line<'static>> {
    let (captured_short, current_short) = decision
        .map(|d| {
            let cap = d.captured_head.get(..7).unwrap_or(&d.captured_head);
            let cur = d.current_head.get(..7).unwrap_or(&d.current_head);
            (cap.to_string(), cur.to_string())
        })
        .unwrap_or_else(|| ("???????".to_string(), "???????".to_string()));

    vec![
        Line::from(Span::styled(
            "An interactive agent advanced HEAD during a stage that must not commit.".to_string(),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Before: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(captured_short),
            Span::raw("  →  "),
            Span::styled("After: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(current_short),
        ]),
    ]
}

pub(super) fn stage_error_title(stage_id: StageId) -> &'static str {
    match stage_id {
        StageId::Brainstorm => "Brainstorm failed",
        StageId::SpecReview => "Spec review failed",
        StageId::Planning => "Planning failed",
        StageId::PlanReview => "Plan review failed",
        StageId::Sharding => "Sharding failed",
        StageId::Implementation => "Implementation failed",
        StageId::Review => "Review failed",
    }
}

/// Semantic accent for a modal dialog: Red = error/failure, Yellow =
/// warning/guard/skip/quit/confirmation, Cyan = paused/informational.
/// The renderer applies the bold modifier itself; callers pass the bare
/// accent color.
pub(super) fn modal_accent_color(kind: ModalKind) -> Color {
    match kind {
        ModalKind::StageError(_) => Color::Red,
        ModalKind::SkipToImpl
        | ModalKind::GitGuard
        | ModalKind::QuitRunningAgent
        | ModalKind::InteractiveExitPrompt => Color::Yellow,
        ModalKind::SpecReviewPaused | ModalKind::PlanReviewPaused => Color::Cyan,
    }
}

pub(super) fn modal_title(kind: ModalKind) -> Option<&'static str> {
    match kind {
        ModalKind::SkipToImpl => Some("Skip to implementation?"),
        ModalKind::GitGuard => Some("Git guard"),
        ModalKind::QuitRunningAgent => Some("Stop running agent and quit?"),
        ModalKind::InteractiveExitPrompt => Some("Any requests?"),
        ModalKind::SpecReviewPaused => Some("Spec review complete"),
        ModalKind::PlanReviewPaused => Some("Plan review complete"),
        ModalKind::StageError(stage_id) => Some(stage_error_title(stage_id)),
    }
}

pub(super) fn stage_error_content(
    _stage_id: StageId,
    error: Option<&str>,
    width: u16,
) -> Vec<Line<'static>> {
    let error_text = error
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(no error details)");
    let truncated: String = error_text.chars().take(300).collect();
    let wrapped = wrap_input(&truncated, width.max(1) as usize);

    // The modal title already carries the semantic red accent. Keep the body
    // content light so stage errors follow the shared modal body-color
    // contract instead of repeating an accent-colored heading inside the body.
    let mut lines = vec![Line::from("")];
    for line in wrapped {
        lines.push(Line::from(Span::styled(
            line,
            Style::default().fg(Color::White),
        )));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::tree::NodeKey,
        state::{NodeKind, NodeStatus},
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
    fn quit_running_agent_modal_title() {
        assert_eq!(
            modal_title(ModalKind::QuitRunningAgent),
            Some("Stop running agent and quit?")
        );
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
    fn spinner_frame_wraps_by_spinner_length() {
        assert_eq!(spinner_frame(0), spinner_frame(10));
    }

    #[test]
    fn status_highlight_bg_maps_terminal_statuses() {
        assert_eq!(status_highlight_bg(NodeStatus::Running), Some(Color::Cyan));
        assert_eq!(status_highlight_bg(NodeStatus::Pending), None);
    }

    #[test]
    fn strip_ansi_codes_removes_escape_sequences() {
        assert_eq!(strip_ansi_codes("\u{1b}[31mred\u{1b}[0m"), "red");
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
        assert!(format!("{:?}", lines).contains("(no rationale provided)"));
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
        assert!(format!("{:?}", lines).contains("abcdef1"));
        assert!(format!("{:?}", lines).contains("9876543"));
    }

    #[test]
    fn stage_error_title_names_stage() {
        assert_eq!(stage_error_title(StageId::PlanReview), "Plan review failed");
    }

    #[test]
    fn modal_accent_color_colors_stage_errors_red() {
        assert_eq!(
            modal_accent_color(ModalKind::StageError(StageId::Review)),
            Color::Red
        );
    }

    #[test]
    fn modal_title_delegates_stage_error_title() {
        assert_eq!(
            modal_title(ModalKind::StageError(StageId::Sharding)),
            Some("Sharding failed")
        );
    }

    #[test]
    fn stage_error_content_uses_default_error_details() {
        let lines = stage_error_content(StageId::Brainstorm, None, 80);
        assert!(format!("{:?}", lines).contains("(no error details)"));
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
        use crate::final_validation::{ValidationStatus, ValidationVerdict};
        let verdict = ValidationVerdict {
            status: ValidationStatus::GoalMet,
            summary: "All goals achieved".to_string(),
            findings: vec!["Inspected src/ and tests/".to_string()],
            gaps: Vec::new(),
            new_tasks: Vec::new(),
        };
        let text = lines_text(&final_validation_report_lines(&verdict, ""));
        assert!(text.contains("goal_met"));
        assert!(text.contains("All goals achieved"));
        assert!(text.contains("Inspected src/ and tests/"));
    }

    #[test]
    fn validation_report_renders_goal_gap_with_citations_and_tasks() {
        use crate::final_validation::{Gap, ValidationStatus, ValidationVerdict, ValidatorGapTask};
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
        };
        let text = lines_text(&final_validation_report_lines(&verdict, ""));
        assert!(text.contains("goal_gap"));
        assert!(text.contains("No retry on HTTP 503"));
        assert!(text.contains("src/client.rs"));
        assert!(text.contains("src/lib.rs"));
        assert!(text.contains("Add backoff"));
    }

    #[test]
    fn validation_report_renders_needs_human_with_gap_citations() {
        use crate::final_validation::{Gap, ValidationStatus, ValidationVerdict};
        let verdict = ValidationVerdict {
            status: ValidationStatus::NeedsHuman,
            summary: "Operator must decide".to_string(),
            findings: vec!["clean tree".to_string()],
            gaps: vec![Gap {
                description: "A or B?".to_string(),
                checked: vec!["artifacts/spec.md".to_string()],
            }],
            new_tasks: Vec::new(),
        };
        let text = lines_text(&final_validation_report_lines(&verdict, ""));
        assert!(text.contains("needs_human"));
        assert!(text.contains("Operator must decide"));
        assert!(text.contains("A or B?"));
        assert!(text.contains("artifacts/spec.md"));
    }

    #[test]
    fn block_banner_highlights_force_ship_and_rewind() {
        let text = lines_text(&final_validation_block_banner_lines(60));
        assert!(text.contains("Final validation blocked"));
        assert!(text.contains("Force ship"));
        assert!(text.contains("Rewind to spec review"));
        // Other transitions remain available — do not remove them, just
        // de-emphasise.
        assert!(text.contains("Other blocked-state transitions remain available"));
    }
}
