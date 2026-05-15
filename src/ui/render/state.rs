use crate::app::tree::VisibleNodeRow;
use crate::app::{ModalKind, StageId};
use crate::state::{NodeStatus, PendingGuardDecision};
use crate::ui::tui::{strip_ansi, wrap_lines_with_prefix, wrap_text};
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
pub(crate) fn is_last_sibling(visible_rows: &[VisibleNodeRow], index: usize) -> bool {
    let cur_depth = visible_rows[index].depth;
    visible_rows[index + 1..]
        .iter()
        .find(|r| r.depth <= cur_depth)
        .is_none_or(|r| r.depth < cur_depth)
}
pub(crate) fn spinner_frame(count: usize) -> &'static str {
    SPINNER[count % SPINNER.len()]
}
pub(crate) fn status_highlight_bg(status: NodeStatus) -> Option<Color> {
    match status {
        NodeStatus::Running => Some(Color::Cyan),
        NodeStatus::Done => Some(Color::Green),
        NodeStatus::Failed => Some(Color::Red),
        NodeStatus::FailedUnverified => Some(Color::LightYellow),
        NodeStatus::Pending | NodeStatus::WaitingUser | NodeStatus::Skipped => None,
    }
}
/// Render the parsed final-validation verdict for the dashboard body.
///
/// Always emits the full report (status, summary, findings, gaps with
/// citations, and any pushed gap tasks) regardless of verdict — per spec,
/// goal validation is high-trust and the operator should always see what
/// the validator checked.
///
/// `width` is the available terminal column count. Body text — summary,
/// findings, gap descriptions, citations, follow-up task titles — is wrapped
/// through the shared [`wrap_lines_with_prefix`] helper, the same one chat
/// messages route through, so long fields don't overflow the viewport.
/// Continuation lines indent to match the visual prefix on the first line so
/// wrapped bullets stay column-aligned with their first row.
pub(crate) fn final_validation_report_lines(
    verdict: &crate::data::validation::ValidationVerdict,
    indent: &str,
    width: usize,
) -> Vec<Line<'static>> {
    use crate::data::validation::ValidationStatus;
    let dim = Style::default().fg(Color::DarkGray);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let cyan = Style::default().fg(Color::Cyan);
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
    let indent_width = indent.chars().count();
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(indent.to_string(), dim),
        Span::styled("Validation report", bold),
        Span::styled(" · ", dim),
        Span::styled(status_text.to_string(), status_style),
    ]));
    push_wrapped_field(
        &mut lines,
        indent,
        indent_width,
        "Summary: ",
        bold,
        &verdict.summary,
        Style::default(),
        width,
    );
    if !verdict.findings.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(indent.to_string(), dim),
            Span::styled("Findings:", bold),
        ]));
        for finding in &verdict.findings {
            push_wrapped_field(
                &mut lines,
                indent,
                indent_width,
                "  • ",
                dim,
                finding,
                Style::default(),
                width,
            );
        }
    }
    if let Some(recommendation) = &verdict.dream_recommendation {
        let recommendation = match recommendation {
            crate::data::validation::DreamRecommendation::Suggest => "suggest",
            crate::data::validation::DreamRecommendation::Skip => "skip",
        };
        push_wrapped_field(
            &mut lines,
            indent,
            indent_width,
            "Dreaming: ",
            bold,
            recommendation,
            Style::default(),
            width,
        );
        if let Some(reason) = verdict.dream_reason.as_deref() {
            push_wrapped_field(
                &mut lines,
                indent,
                indent_width,
                "Dream reason: ",
                bold,
                reason,
                Style::default(),
                width,
            );
        }
    }
    if !verdict.gaps.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(indent.to_string(), dim),
            Span::styled("Gaps:", bold),
        ]));
        for gap in &verdict.gaps {
            push_wrapped_field(
                &mut lines,
                indent,
                indent_width,
                "  • ",
                dim,
                &gap.description,
                Style::default(),
                width,
            );
            for citation in &gap.checked {
                push_wrapped_field(
                    &mut lines,
                    indent,
                    indent_width,
                    "      checked: ",
                    dim,
                    citation,
                    cyan,
                    width,
                );
            }
        }
    }
    if !verdict.new_tasks.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(indent.to_string(), dim),
            Span::styled("Follow-up tasks:", bold),
        ]));
        for task in &verdict.new_tasks {
            push_wrapped_field(
                &mut lines,
                indent,
                indent_width,
                "  • ",
                dim,
                &task.title,
                Style::default(),
                width,
            );
        }
    }
    lines
}
/// Push a "<indent><prefix><body>" entry through the shared
/// [`wrap_lines_with_prefix`] helper so the validation report's wrap behavior
/// matches every other transcript-shaped surface (chat messages, status
/// surfaces). Continuation lines indent to align under the body's first
/// column.
#[allow(clippy::too_many_arguments)]
fn push_wrapped_field(
    lines: &mut Vec<Line<'static>>,
    indent: &str,
    indent_width: usize,
    prefix: &str,
    prefix_style: Style,
    body: &str,
    body_style: Style,
    width: usize,
) {
    let dim = Style::default().fg(Color::DarkGray);
    let prefix_visible_width = indent_width + prefix.chars().count();
    let prefix_spans = vec![
        Span::styled(indent.to_string(), dim),
        Span::styled(prefix.to_string(), prefix_style),
    ];
    lines.extend(wrap_lines_with_prefix(
        prefix_spans,
        prefix_visible_width,
        body,
        body_style,
        width,
    ));
}
pub(crate) fn sanitize_live_summary(text: &str) -> String {
    let stripped = strip_ansi(text);
    let collapsed = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(500).collect()
}
pub(crate) fn skip_to_impl_content(
    rationale: Option<&str>,
    kind: Option<crate::data::artifacts::SkipToImplKind>,
    width: u16,
) -> Vec<Line<'static>> {
    use crate::data::artifacts::SkipToImplKind;
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
    let rationale_lines = wrap_text(rationale_text, width.max(1) as usize);
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
pub(crate) fn guard_content(decision: Option<&PendingGuardDecision>) -> Vec<Line<'static>> {
    let (captured_short, current_short) = decision.map_or_else(
        || ("???????".to_string(), "???????".to_string()),
        |d| {
            let cap = d.captured_head.get(..7).unwrap_or(&d.captured_head);
            let cur = d.current_head.get(..7).unwrap_or(&d.current_head);
            (cap.to_string(), cur.to_string())
        },
    );
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
pub(crate) fn dreaming_decision_content(
    decision: Option<&crate::state::DreamingDecision>,
    width: u16,
) -> Vec<Line<'static>> {
    let reason = decision
        .and_then(|decision| decision.reason.as_deref())
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or("(no reason provided)");
    let mut lines = vec![
        Line::from(Span::styled(
            "The final-validation agent suggested Dreaming for this session.".to_string(),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Reason:".to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ];
    for line in wrap_text(reason, width.max(1) as usize) {
        lines.push(Line::from(Span::styled(
            line,
            Style::default().fg(Color::White),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter/r run Dreaming · s/Esc skip and finish".to_string(),
        Style::default().fg(Color::White),
    )));
    lines
}
pub(crate) fn stage_error_title(stage_id: StageId) -> &'static str {
    match stage_id {
        StageId::Brainstorm => "Brainstorm failed",
        StageId::SpecReview => "Spec review failed",
        StageId::Planning => "Planning failed",
        StageId::PlanReview => "Plan review failed",
        StageId::RepoStateUpdate => "Repo state update failed",
        StageId::Sharding => "Sharding failed",
        StageId::Implementation => "Implementation failed",
        StageId::Recovery => "Recovery failed",
        StageId::RecoveryPlanReview => "Recovery plan review failed",
        StageId::RecoverySharding => "Recovery sharding failed",
        StageId::Review => "Review failed",
        StageId::Simplification => "Simplification failed",
        StageId::FinalValidation => "Final validation failed",
        StageId::Dreaming => "Dreaming failed",
    }
}
/// Semantic accent for a modal dialog: Red = error/failure, Yellow =
/// warning/guard/skip/quit/confirmation, Cyan = paused/informational.
/// The renderer applies the bold modifier itself; callers pass the bare
/// accent color.
pub(crate) fn modal_accent_color(kind: ModalKind) -> Color {
    match kind {
        ModalKind::StageError(_) => Color::Red,
        ModalKind::SkipToImpl
        | ModalKind::GitGuard
        | ModalKind::QuitRunningAgent
        | ModalKind::CancelSession
        | ModalKind::InteractiveExitPrompt
        | ModalKind::DreamingDecision => Color::Yellow,
        ModalKind::SpecReviewPaused | ModalKind::PlanReviewPaused => Color::Cyan,
        ModalKind::FinalValidationBlocked => Color::Red,
    }
}
pub(crate) fn modal_title(kind: ModalKind) -> &'static str {
    match kind {
        ModalKind::SkipToImpl => "Skip to implementation?",
        ModalKind::GitGuard => "Git guard",
        ModalKind::QuitRunningAgent => "Stop running agent and quit?",
        ModalKind::CancelSession => "Cancel this session?",
        ModalKind::InteractiveExitPrompt => "Any requests?",
        ModalKind::SpecReviewPaused => "Spec review complete",
        ModalKind::PlanReviewPaused => "Plan review complete",
        ModalKind::StageError(stage_id) => stage_error_title(stage_id),
        ModalKind::FinalValidationBlocked => "Final Validation Blocked",
        ModalKind::DreamingDecision => "Run Dreaming?",
    }
}
pub(crate) fn stage_error_content(
    _stage_id: StageId,
    error: Option<&str>,
    width: u16,
) -> Vec<Line<'static>> {
    let error_text = error
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("(no error details)");
    let truncated: String = error_text.chars().take(300).collect();
    let wrapped = wrap_text(&truncated, width.max(1) as usize);
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
#[path = "state_tests.rs"]
mod tests;
