use chrono::{Datelike, FixedOffset, TimeZone, Timelike, Utc};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

use crate::app::chat_widget_view_model::chat_scroll_window;
use crate::app::footer::{HistoricalStyleHints, format_historical_message};
use crate::state::{Message, MessageKind, RunRecord, RunStatus};

pub struct ChatWidget<'a> {
    messages: &'a [Message],
    run: &'a RunRecord,
    scroll_offset: usize,
    local_offset: FixedOffset,
    running_tail: Option<Line<'static>>,
}

impl<'a> ChatWidget<'a> {
    pub fn new(
        messages: &'a [Message],
        run: &'a RunRecord,
        scroll_offset: usize,
        local_offset: FixedOffset,
        running_tail: Option<Line<'static>>,
    ) -> Self {
        Self {
            messages,
            run,
            scroll_offset,
            local_offset,
            running_tail,
        }
    }
}

struct SymbolStyle {
    symbol: &'static str,
    color: Color,
}

fn message_symbol(kind: MessageKind, run_status: RunStatus) -> SymbolStyle {
    match kind {
        MessageKind::Started => SymbolStyle {
            symbol: "○",
            color: Color::DarkGray,
        },
        MessageKind::Brief => SymbolStyle {
            symbol: "◐",
            color: Color::Cyan,
        },
        MessageKind::UserInput => SymbolStyle {
            symbol: "›",
            color: Color::Magenta,
        },
        MessageKind::AgentText => SymbolStyle {
            symbol: "▸",
            color: Color::White,
        },
        MessageKind::AgentThought => SymbolStyle {
            symbol: "·",
            color: Color::DarkGray,
        },
        MessageKind::Summary => SymbolStyle {
            symbol: "✓",
            color: Color::Green,
        },
        MessageKind::SummaryWarn => SymbolStyle {
            symbol: "⚠",
            color: Color::Yellow,
        },
        MessageKind::End => match run_status {
            RunStatus::Done => SymbolStyle {
                symbol: "●",
                color: Color::Green,
            },
            RunStatus::FailedUnverified => SymbolStyle {
                symbol: "!",
                color: Color::Yellow,
            },
            _ => SymbolStyle {
                symbol: "✗",
                color: Color::Red,
            },
        },
    }
}

fn kind_to_hints(kind: MessageKind, run_status: RunStatus) -> HistoricalStyleHints {
    match kind {
        MessageKind::Summary => HistoricalStyleHints {
            is_summary: true,
            ..Default::default()
        },
        MessageKind::SummaryWarn => HistoricalStyleHints {
            is_warning: true,
            ..Default::default()
        },
        MessageKind::AgentThought => HistoricalStyleHints {
            is_dim: true,
            ..Default::default()
        },
        MessageKind::End => match run_status {
            RunStatus::Failed | RunStatus::FailedUnverified => HistoricalStyleHints {
                is_error: true,
                ..Default::default()
            },
            _ => Default::default(),
        },
        _ => Default::default(),
    }
}

fn format_timestamp(
    ts: &chrono::DateTime<Utc>,
    local_offset: &FixedOffset,
    today_local: chrono::NaiveDate,
) -> String {
    let local_dt = local_offset.from_utc_datetime(&ts.naive_utc());
    let msg_date = local_dt.date_naive();
    if msg_date == today_local {
        format!(
            "{:02}:{:02}:{:02}",
            local_dt.hour(),
            local_dt.minute(),
            local_dt.second()
        )
    } else {
        format!(
            "{:02}-{:02} {:02}:{:02}",
            local_dt.month(),
            local_dt.day(),
            local_dt.hour(),
            local_dt.minute()
        )
    }
}

fn strip_ansi(s: &str) -> String {
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

fn wrap_text(text: &str, content_width: usize) -> Vec<String> {
    if content_width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for raw_line in text.split('\n') {
        let clean = strip_ansi(raw_line);
        if clean.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_len = 0usize;
        for word in clean.split_inclusive(' ') {
            let word_len = word.chars().count();
            if current_len + word_len <= content_width {
                current.push_str(word);
                current_len += word_len;
                continue;
            }
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
                current_len = 0;
            }
            if word_len <= content_width {
                current.push_str(word);
                current_len = word_len;
            } else {
                let mut remaining = word;
                while remaining.chars().count() > content_width {
                    let split_at = remaining
                        .char_indices()
                        .nth(content_width)
                        .map(|(i, _)| i)
                        .unwrap_or(remaining.len());
                    out.push(remaining[..split_at].to_string());
                    remaining = &remaining[split_at..];
                }
                if !remaining.is_empty() {
                    current.push_str(remaining);
                    current_len = remaining.chars().count();
                }
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
    }
    out
}

fn push_wrapped_span_line(
    lines: &mut Vec<Vec<Span<'static>>>,
    current: &mut Vec<Span<'static>>,
    content_width: usize,
) {
    if current.is_empty() {
        lines.push(Vec::new());
        return;
    }

    lines.extend(wrap_spans(std::mem::take(current), content_width));
}

fn wrap_spans(spans: Vec<Span<'static>>, content_width: usize) -> Vec<Vec<Span<'static>>> {
    if content_width == 0 {
        return Vec::new();
    }
    let mut lines = vec![Vec::new()];
    let mut current_len = 0usize;

    for span in spans {
        let style = span.style;
        let mut remaining = span.content.to_string();
        while !remaining.is_empty() {
            let room = content_width.saturating_sub(current_len);
            if room == 0 {
                lines.push(Vec::new());
                current_len = 0;
                continue;
            }
            let remaining_len = remaining.chars().count();
            if remaining_len <= room {
                current_len += remaining_len;
                lines
                    .last_mut()
                    .expect("line exists")
                    .push(Span::styled(remaining, style));
                break;
            }

            let split_at = remaining
                .char_indices()
                .nth(room)
                .map(|(i, _)| i)
                .unwrap_or(remaining.len());
            let chunk = remaining[..split_at].to_string();
            lines
                .last_mut()
                .expect("line exists")
                .push(Span::styled(chunk, style));
            remaining = remaining[split_at..].to_string();
            lines.push(Vec::new());
            current_len = 0;
        }
    }

    lines
}

fn render_agent_markdown(
    text: &str,
    content_width: usize,
    base_style: Style,
) -> Vec<Vec<Span<'static>>> {
    if content_width == 0 {
        return Vec::new();
    }

    let parser = Parser::new_ext(text, Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES);
    let code_style = Style::default().fg(Color::Cyan);
    let heading_style = base_style.add_modifier(Modifier::BOLD);
    let mut style_stack = vec![base_style];
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut list_depth = 0usize;
    let mut in_code_block = false;

    for event in parser {
        match event {
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) if !current.is_empty() => {
                push_wrapped_span_line(&mut lines, &mut current, content_width);
            }
            Event::End(TagEnd::Paragraph) => {}
            Event::Start(Tag::Heading { .. }) => {
                if !current.is_empty() {
                    push_wrapped_span_line(&mut lines, &mut current, content_width);
                }
                style_stack.push(heading_style);
            }
            Event::End(TagEnd::Heading(_)) => {
                if !current.is_empty() {
                    push_wrapped_span_line(&mut lines, &mut current, content_width);
                }
                style_stack.pop();
            }
            Event::Start(Tag::Strong) => {
                let style = *style_stack.last().unwrap_or(&base_style);
                style_stack.push(style.add_modifier(Modifier::BOLD));
            }
            Event::End(TagEnd::Strong) => {
                style_stack.pop();
            }
            Event::Start(Tag::Emphasis) => {
                let style = *style_stack.last().unwrap_or(&base_style);
                style_stack.push(style.add_modifier(Modifier::ITALIC));
            }
            Event::End(TagEnd::Emphasis) => {
                style_stack.pop();
            }
            Event::Start(Tag::List(_)) => {
                list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
            }
            Event::Start(Tag::Item) => {
                if !current.is_empty() {
                    push_wrapped_span_line(&mut lines, &mut current, content_width);
                }
                let indent = "  ".repeat(list_depth.saturating_sub(1));
                current.push(Span::styled(format!("{indent}• "), base_style));
            }
            Event::End(TagEnd::Item) if !current.is_empty() => {
                push_wrapped_span_line(&mut lines, &mut current, content_width);
            }
            Event::End(TagEnd::Item) => {}
            Event::Start(Tag::CodeBlock(_)) => {
                if !current.is_empty() {
                    push_wrapped_span_line(&mut lines, &mut current, content_width);
                }
                in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                if !current.is_empty() {
                    push_wrapped_span_line(&mut lines, &mut current, content_width);
                }
                in_code_block = false;
            }
            Event::Text(value) => {
                let style = if in_code_block {
                    code_style
                } else {
                    *style_stack.last().unwrap_or(&base_style)
                };
                for (index, raw_line) in value.split('\n').enumerate() {
                    if index > 0 {
                        push_wrapped_span_line(&mut lines, &mut current, content_width);
                    }
                    let clean = strip_ansi(raw_line);
                    if clean.is_empty() && in_code_block {
                        lines.push(Vec::new());
                    } else if in_code_block {
                        current.push(Span::styled(format!("  {clean}"), style));
                    } else {
                        current.push(Span::styled(clean, style));
                    }
                }
            }
            Event::Code(value) => {
                current.push(Span::styled(strip_ansi(&value), code_style));
            }
            Event::SoftBreak | Event::HardBreak => {
                push_wrapped_span_line(&mut lines, &mut current, content_width);
            }
            Event::Rule => {
                if !current.is_empty() {
                    push_wrapped_span_line(&mut lines, &mut current, content_width);
                }
                lines.push(vec![Span::styled("─".repeat(content_width), base_style)]);
            }
            _ => {}
        }
    }

    if !current.is_empty() {
        push_wrapped_span_line(&mut lines, &mut current, content_width);
    }

    if lines.is_empty() {
        wrap_text(text, content_width)
            .into_iter()
            .map(|line| vec![Span::styled(line, base_style)])
            .collect()
    } else {
        lines
    }
}

struct RenderedLine {
    spans: Vec<Span<'static>>,
}

fn push_blank_line_if_needed(lines: &mut Vec<RenderedLine>) {
    if lines
        .last()
        .is_none_or(|line| !line.spans.iter().all(|span| span.content.is_empty()))
    {
        lines.push(RenderedLine { spans: Vec::new() });
    }
}

fn capitalize_first_span(spans: &[Span<'static>]) -> Vec<Span<'static>> {
    let mut capitalized = spans.to_vec();
    for span in &mut capitalized {
        if span.content.is_empty() {
            continue;
        }
        let mut chars = span.content.chars();
        if let Some(first) = chars.next() {
            let text = first.to_uppercase().collect::<String>() + chars.as_str();
            span.content = text.into();
        }
        break;
    }
    capitalized
}

/// Render an agent run's transcript as a list of lines.
///
/// `running_tail` is appended after the historical messages when the run is
/// still active (status `Running` and no `End` message yet). The caller
/// chooses the tail's shape: leaf transcript rows pass a live-agent-message
/// line built via `format_running_transcript_leaf`; container rows pass a
/// tree-shape spinner; non-render callers (e.g., line counters) pass `None`.
pub fn message_lines(
    messages: &[Message],
    run: &RunRecord,
    local_offset: &FixedOffset,
    running_tail: Option<Line<'static>>,
    available_width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> =
        render_messages(messages, run, local_offset, available_width)
            .into_iter()
            .map(|rendered| Line::from(rendered.spans))
            .collect();
    let has_end = messages.iter().any(|m| m.kind == MessageKind::End);
    if run.status == RunStatus::Running
        && !has_end
        && let Some(tail) = running_tail
    {
        lines.push(tail);
    }
    lines
}

fn render_messages(
    messages: &[Message],
    run: &RunRecord,
    local_offset: &FixedOffset,
    available_width: usize,
) -> Vec<RenderedLine> {
    let now_local = local_offset.from_utc_datetime(&Utc::now().naive_utc());
    let today_local = now_local.date_naive();
    let mut lines = Vec::new();

    for msg in messages {
        let ts_str = format_timestamp(&msg.ts, local_offset, today_local);
        let ts_w = ts_str.chars().count();
        let sym = message_symbol(msg.kind, run.status);
        let prefix_width = ts_w + 3; // " ○ "
        let content_width = available_width.saturating_sub(prefix_width);

        let indent = " ".repeat(prefix_width);

        if msg.kind == MessageKind::Brief {
            let (title, details) = match msg.text.split_once('|') {
                Some((t, d)) => (t.trim().to_string(), d.trim().to_string()),
                None => (msg.text.trim().to_string(), String::new()),
            };
            let title_wrapped = wrap_text(&title, content_width);
            for (i, chunk) in title_wrapped.iter().enumerate() {
                let title_span =
                    Span::styled(chunk.clone(), Style::default().add_modifier(Modifier::BOLD));
                if i == 0 {
                    lines.push(RenderedLine {
                        spans: vec![
                            Span::styled(format!("{} ", ts_str), Style::default().fg(sym.color)),
                            Span::styled(
                                format!("{} ", sym.symbol),
                                Style::default().fg(sym.color),
                            ),
                            title_span,
                        ],
                    });
                } else {
                    lines.push(RenderedLine {
                        spans: vec![Span::raw(indent.clone()), title_span],
                    });
                }
            }
            if title_wrapped.is_empty() {
                lines.push(RenderedLine {
                    spans: vec![
                        Span::styled(format!("{} ", ts_str), Style::default().fg(sym.color)),
                        Span::styled(format!("{} ", sym.symbol), Style::default().fg(sym.color)),
                    ],
                });
            }
            if !details.is_empty() {
                for chunk in wrap_text(&details, content_width) {
                    lines.push(RenderedLine {
                        spans: vec![
                            Span::raw(indent.clone()),
                            Span::styled(chunk, Style::default().fg(Color::White)),
                        ],
                    });
                }
            }
            continue;
        }

        if msg.kind == MessageKind::UserInput {
            let wrapped = wrap_text(&msg.text, content_width);
            let body_style = Style::default().fg(Color::Magenta);
            if let Some((first, rest)) = wrapped.split_first() {
                lines.push(RenderedLine {
                    spans: vec![
                        Span::styled(format!("{} ", ts_str), Style::default().fg(sym.color)),
                        Span::styled(format!("{} ", sym.symbol), Style::default().fg(sym.color)),
                        Span::styled(first.clone(), body_style),
                    ],
                });
                for chunk in rest {
                    lines.push(RenderedLine {
                        spans: vec![
                            Span::raw(indent.clone()),
                            Span::styled(chunk.clone(), body_style),
                        ],
                    });
                }
            } else {
                lines.push(RenderedLine {
                    spans: vec![
                        Span::styled(format!("{} ", ts_str), Style::default().fg(sym.color)),
                        Span::styled(format!("{} ", sym.symbol), Style::default().fg(sym.color)),
                    ],
                });
            }
            continue;
        }

        let hints = kind_to_hints(msg.kind, run.status);
        let markdown_style = if hints.is_dim {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::White)
        };
        let renders_markdown =
            matches!(msg.kind, MessageKind::AgentText | MessageKind::AgentThought);
        let is_interactive_acp_output = run.modes.interactive && renders_markdown;
        if is_interactive_acp_output {
            push_blank_line_if_needed(&mut lines);
        }
        let markdown_lines = if renders_markdown {
            render_agent_markdown(&msg.text, content_width, markdown_style)
        } else {
            wrap_text(&msg.text, content_width)
                .into_iter()
                .map(|line| vec![Span::raw(line)])
                .collect()
        };

        if let Some((first, rest)) = markdown_lines.split_first() {
            let first_text: String = first.iter().map(|span| span.content.to_string()).collect();
            let first_line =
                format_historical_message(&ts_str, sym.symbol, &first_text, sym.color, hints);
            let mut first_spans = first_line.spans;
            if renders_markdown && first_spans.len() >= 2 {
                first_spans.truncate(2);
                if msg.kind == MessageKind::AgentThought {
                    first_spans.extend(capitalize_first_span(first));
                } else {
                    first_spans.extend(first.iter().cloned());
                }
            }
            lines.push(RenderedLine { spans: first_spans });

            let body_style = if hints.is_error {
                Style::default().fg(Color::Red)
            } else if hints.is_warning {
                Style::default().fg(Color::Yellow)
            } else if hints.is_summary {
                Style::default().fg(Color::Green)
            } else if hints.is_dim {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };
            for chunk in rest {
                if renders_markdown {
                    let mut spans = vec![Span::raw(indent.clone())];
                    spans.extend(chunk.iter().cloned());
                    lines.push(RenderedLine { spans });
                    continue;
                }
                let text: String = chunk.iter().map(|span| span.content.to_string()).collect();
                lines.push(RenderedLine {
                    spans: vec![Span::styled(format!("{}{}", indent, text), body_style)],
                });
            }
        } else {
            let first_line = format_historical_message(&ts_str, sym.symbol, "", sym.color, hints);
            lines.push(RenderedLine {
                spans: first_line.spans,
            });
        }
        if is_interactive_acp_output {
            push_blank_line_if_needed(&mut lines);
        }
    }

    lines
}

impl Widget for ChatWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let width = area.width as usize;
        let height = area.height as usize;

        let all_lines = message_lines(
            self.messages,
            self.run,
            &self.local_offset,
            self.running_tail.clone(),
            width,
        );

        let total = all_lines.len();
        if total == 0 {
            return;
        }

        let Some(window) = chat_scroll_window(total, height, self.scroll_offset) else {
            return;
        };

        let mut row = area.y;

        if window.show_above_indicator {
            let indicator = format!("  ↑ {} more above", window.above_count);
            let line = Line::from(Span::styled(
                indicator,
                Style::default().fg(Color::DarkGray),
            ));
            buf.set_line(area.x, row, &line, area.width);
            row += 1;
        }

        for line in &all_lines[window.offset..window.visible_end] {
            let line = line.clone();
            buf.set_line(area.x, row, &line, area.width);
            row += 1;
        }

        if window.show_below_indicator {
            let indicator = format!("  ↓ {} more below", window.below_count);
            let line = Line::from(Span::styled(
                indicator,
                Style::default().fg(Color::DarkGray),
            ));
            buf.set_line(area.x, row, &line, area.width);
        }
    }
}

pub fn chat_lines(
    messages: &[Message],
    run: &RunRecord,
    scroll_offset: usize,
    local_offset: &FixedOffset,
    running_tail: Option<Line<'static>>,
    available_width: usize,
    available_height: usize,
) -> Vec<Line<'static>> {
    let all_lines = message_lines(messages, run, local_offset, running_tail, available_width);

    let total = all_lines.len();
    if total == 0 {
        return Vec::new();
    }

    let Some(window) = chat_scroll_window(total, available_height, scroll_offset) else {
        return Vec::new();
    };

    let mut lines = Vec::new();

    if window.show_above_indicator {
        lines.push(Line::from(Span::styled(
            format!("  ↑ {} more above", window.above_count),
            Style::default().fg(Color::DarkGray),
        )));
    }

    for line in &all_lines[window.offset..window.visible_end] {
        lines.push(line.clone());
    }

    if window.show_below_indicator {
        lines.push(Line::from(Span::styled(
            format!("  ↓ {} more below", window.below_count),
            Style::default().fg(Color::DarkGray),
        )));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_msg(kind: MessageKind, text: &str) -> Message {
        Message {
            ts: Utc.with_ymd_and_hms(2026, 4, 24, 10, 30, 0).unwrap(),
            run_id: 1,
            kind,
            sender: crate::state::MessageSender::System,
            text: text.to_string(),
        }
    }

    fn make_run(status: RunStatus) -> RunRecord {
        RunRecord {
            id: 1,
            stage: "Brainstorm".to_string(),
            task_id: None,
            round: 1,
            attempt: 1,
            model: "claude-sonnet-4".to_string(),
            vendor: "claude".to_string(),
            window_name: "test".to_string(),
            started_at: Utc::now(),
            ended_at: None,
            status,
            error: None,
            effort: crate::adapters::EffortLevel::Normal,
            modes: crate::state::LaunchModes::default(),
            hostname: None,
            mount_device_id: None,
        }
    }

    #[test]
    fn timestamp_same_day() {
        let offset = FixedOffset::east_opt(0).unwrap();
        let ts = Utc.with_ymd_and_hms(2026, 4, 24, 14, 5, 9).unwrap();
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 24).unwrap();
        assert_eq!(format_timestamp(&ts, &offset, today), "14:05:09");
    }

    #[test]
    fn timestamp_different_day() {
        let offset = FixedOffset::east_opt(0).unwrap();
        let ts = Utc.with_ymd_and_hms(2026, 4, 20, 9, 30, 0).unwrap();
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 24).unwrap();
        assert_eq!(format_timestamp(&ts, &offset, today), "04-20 09:30");
    }

    #[test]
    fn timestamp_with_timezone_offset() {
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        let ts = Utc.with_ymd_and_hms(2026, 4, 23, 23, 0, 0).unwrap();
        let today = chrono::NaiveDate::from_ymd_opt(2026, 4, 24).unwrap();
        // UTC 23:00 + 8h = next day 07:00, same local date as today
        assert_eq!(format_timestamp(&ts, &offset, today), "07:00:00");
    }

    #[test]
    fn symbol_started() {
        let s = message_symbol(MessageKind::Started, RunStatus::Running);
        assert_eq!(s.symbol, "○");
        assert_eq!(s.color, Color::DarkGray);
    }

    #[test]
    fn symbol_brief() {
        let s = message_symbol(MessageKind::Brief, RunStatus::Running);
        assert_eq!(s.symbol, "◐");
        assert_eq!(s.color, Color::Cyan);
    }

    #[test]
    fn symbol_user_input() {
        let s = message_symbol(MessageKind::UserInput, RunStatus::Running);
        assert_eq!(s.symbol, "›");
        assert_eq!(s.color, Color::Magenta);
    }

    #[test]
    fn symbol_end_done() {
        let s = message_symbol(MessageKind::End, RunStatus::Done);
        assert_eq!(s.symbol, "●");
        assert_eq!(s.color, Color::Green);
    }

    #[test]
    fn symbol_end_failed() {
        let s = message_symbol(MessageKind::End, RunStatus::Failed);
        assert_eq!(s.symbol, "✗");
        assert_eq!(s.color, Color::Red);
    }

    #[test]
    fn symbol_end_failed_unverified() {
        let s = message_symbol(MessageKind::End, RunStatus::FailedUnverified);
        assert_eq!(s.symbol, "!");
        assert_eq!(s.color, Color::Yellow);
    }

    #[test]
    fn wrap_short_text() {
        let result = wrap_text("hello world", 20);
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn wrap_at_word_boundary() {
        let result = wrap_text("hello beautiful world today", 15);
        // "hello " (6) + "beautiful " (10) = 16 > 15, so splits after "hello "
        assert_eq!(result, vec!["hello ", "beautiful ", "world today"]);
    }

    #[test]
    fn wrap_force_split_long_word() {
        let result = wrap_text("abcdefghij", 5);
        assert_eq!(result, vec!["abcde", "fghij"]);
    }

    #[test]
    fn wrap_preserves_newlines() {
        let result = wrap_text("line one\nline two", 40);
        assert_eq!(result, vec!["line one", "line two"]);
    }

    #[test]
    fn wrap_strips_ansi() {
        let result = wrap_text("\x1b[31mred text\x1b[0m", 20);
        assert_eq!(result, vec!["red text"]);
    }

    fn tail_line(text: &str) -> Line<'static> {
        Line::from(Span::raw(text.to_string()))
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.to_string())
            .collect()
    }

    #[test]
    fn message_lines_appends_running_tail_when_active() {
        let msgs = vec![make_msg(MessageKind::Started, "agent started")];
        let run = make_run(RunStatus::Running);
        let offset = FixedOffset::east_opt(0).unwrap();
        let lines = message_lines(&msgs, &run, &offset, Some(tail_line("LIVE-TAIL")), 60);
        let last_text: String = lines
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(last_text, "LIVE-TAIL");
    }

    #[test]
    fn message_lines_renders_user_input_with_distinct_style() {
        let msgs = vec![make_msg(MessageKind::UserInput, "please continue")];
        let run = make_run(RunStatus::Running);
        let offset = FixedOffset::east_opt(0).unwrap();
        let lines = message_lines(&msgs, &run, &offset, None, 80);
        let text = line_text(&lines[0]);

        assert!(text.contains("› please continue"));
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content == "› " && span.style.fg == Some(Color::Magenta)),
            "user input should use a distinct prompt icon: {:?}",
            lines[0].spans
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content == "please continue"
                    && span.style.fg == Some(Color::Magenta)),
            "user input body should use a distinct color: {:?}",
            lines[0].spans
        );
    }

    #[test]
    fn thinking_continuation_lines_stay_dim() {
        let msgs = vec![make_msg(
            MessageKind::AgentThought,
            "first thinking line\nsecond thinking line",
        )];
        let run = make_run(RunStatus::Running);
        let offset = FixedOffset::east_opt(0).unwrap();
        let lines = message_lines(&msgs, &run, &offset, None, 80);

        assert_eq!(line_text(&lines[1]).trim(), "second thinking line");
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|span| span.content.contains("second thinking line")
                    && span.style.fg == Some(Color::DarkGray)),
            "thinking continuation should keep thinking color: {:?}",
            lines[1].spans
        );
    }

    #[test]
    fn thinking_text_renders_markdown_without_raw_markers() {
        let msgs = vec![make_msg(
            MessageKind::AgentThought,
            "Thinking with **bold text** and `code`.",
        )];
        let run = make_run(RunStatus::Running);
        let offset = FixedOffset::east_opt(0).unwrap();
        let lines = message_lines(&msgs, &run, &offset, None, 80);
        let text = line_text(&lines[0]);

        assert!(text.contains("Thinking with bold text and code."));
        assert!(!text.contains("**"));
        assert!(!text.contains('`'));
        assert!(
            lines[0].spans.iter().any(|span| span.content == "bold text"
                && span.style.fg == Some(Color::DarkGray)
                && span.style.add_modifier.contains(Modifier::BOLD)),
            "thinking markdown should keep dim color while applying bold: {:?}",
            lines[0].spans
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content == "code" && span.style.fg == Some(Color::Cyan)),
            "thinking inline code should be highlighted: {:?}",
            lines[0].spans
        );
    }

    #[test]
    fn interactive_acp_agent_outputs_get_padding_without_extra_indent() {
        let msgs = vec![
            make_msg(MessageKind::UserInput, "please continue"),
            make_msg(MessageKind::AgentThought, "thinking"),
            make_msg(MessageKind::AgentText, "answer"),
        ];
        let mut run = make_run(RunStatus::Running);
        run.modes.interactive = true;
        let offset = FixedOffset::east_opt(0).unwrap();
        let lines = message_lines(&msgs, &run, &offset, None, 80);
        let texts = lines.iter().map(line_text).collect::<Vec<_>>();

        assert!(texts[0].contains("› please continue"));
        assert_eq!(texts[1], "");
        assert!(texts[2].contains("· Thinking"), "{texts:?}");
        assert_eq!(texts[3], "");
        assert!(texts[4].contains("▸ answer"), "{texts:?}");
        assert_eq!(texts[5], "");
        assert!(
            texts
                .windows(2)
                .all(|pair| !(pair[0].is_empty() && pair[1].is_empty())),
            "ACP padding should never create consecutive blank lines: {texts:?}"
        );
    }

    #[test]
    fn agent_text_renders_markdown_emphasis_without_raw_markers() {
        let msgs = vec![make_msg(
            MessageKind::AgentText,
            "Here is **bold text** and `code`.",
        )];
        let run = make_run(RunStatus::Running);
        let offset = FixedOffset::east_opt(0).unwrap();
        let lines = message_lines(&msgs, &run, &offset, None, 80);
        let text = line_text(&lines[0]);

        assert!(text.contains("Here is bold text and code."));
        assert!(!text.contains("**"));
        assert!(!text.contains('`'));
        assert!(
            lines[0].spans.iter().any(|span| span.content == "bold text"
                && span.style.add_modifier.contains(Modifier::BOLD)),
            "bold markdown should become a bold span: {:?}",
            lines[0].spans
        );
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content == "code" && span.style.fg == Some(Color::Cyan)),
            "inline code should be highlighted: {:?}",
            lines[0].spans
        );
    }

    #[test]
    fn agent_text_renders_markdown_lists_and_fenced_code() {
        let msgs = vec![make_msg(
            MessageKind::AgentText,
            "- first\n- second\n\n```rust\nlet answer = 42;\n```",
        )];
        let run = make_run(RunStatus::Running);
        let offset = FixedOffset::east_opt(0).unwrap();
        let lines = message_lines(&msgs, &run, &offset, None, 80);
        let texts = lines.iter().map(line_text).collect::<Vec<_>>();

        assert!(texts.iter().any(|line| line.contains("• first")));
        assert!(texts.iter().any(|line| line.contains("• second")));
        assert!(texts.iter().any(|line| line.contains("let answer = 42;")));
        assert!(
            texts.iter().all(|line| !line.contains("```")),
            "fence markers should not render: {texts:?}"
        );
    }

    #[test]
    fn message_lines_drops_legacy_working_label() {
        let msgs = vec![make_msg(MessageKind::Started, "agent started")];
        let run = make_run(RunStatus::Running);
        let offset = FixedOffset::east_opt(0).unwrap();
        let lines = message_lines(&msgs, &run, &offset, None, 60);
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
            assert!(
                !text.contains("working..."),
                "running tail must come from caller, not the legacy 'working...' line"
            );
        }
    }

    #[test]
    fn message_lines_omits_tail_after_end_message() {
        let msgs = vec![
            make_msg(MessageKind::Started, "agent started"),
            make_msg(MessageKind::End, "done"),
        ];
        let run = make_run(RunStatus::Done);
        let offset = FixedOffset::east_opt(0).unwrap();
        let lines = message_lines(&msgs, &run, &offset, Some(tail_line("LIVE-TAIL")), 60);
        for line in &lines {
            let text: String = line.spans.iter().map(|s| s.content.to_string()).collect();
            assert!(!text.contains("LIVE-TAIL"));
        }
    }

    #[test]
    fn chat_lines_scroll_indicators() {
        let mut msgs = Vec::new();
        for i in 0..20 {
            msgs.push(make_msg(MessageKind::Brief, &format!("message {i}")));
        }
        let run = make_run(RunStatus::Done);
        let offset = FixedOffset::east_opt(0).unwrap();
        let lines = chat_lines(&msgs, &run, 5, &offset, None, 60, 10);
        let first_text: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        let last_text: String = lines
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(first_text.contains("↑"), "should show above indicator");
        assert!(first_text.contains("5 more above"));
        assert!(last_text.contains("↓"), "should show below indicator");
        assert!(last_text.contains("7 more below"));
    }

    #[test]
    fn wrapped_lines_indent_matches_prefix() {
        let msg = Message {
            ts: Utc.with_ymd_and_hms(2026, 4, 24, 10, 30, 0).unwrap(),
            run_id: 1,
            kind: MessageKind::Brief,
            sender: crate::state::MessageSender::System,
            text: "this is a long message that should wrap to the next line properly".to_string(),
        };
        let run = make_run(RunStatus::Running);
        let offset = FixedOffset::east_opt(0).unwrap();
        // width 30 forces wrapping. Prefix = "10:30 ◐ " = 5+3=8 chars
        let lines = render_messages(&[msg], &run, &offset, 30);
        assert!(lines.len() >= 2, "should have wrapped lines");
        // Second line should be indented (starts with spaces)
        let second_text: String = lines[1]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(
            second_text.starts_with("        "),
            "wrapped line should indent to match prefix width (8 spaces)"
        );
    }

    #[test]
    fn chat_lines_allows_scrolling_to_bottom_with_indicators() {
        let mut msgs = Vec::new();
        for i in 0..11 {
            msgs.push(make_msg(MessageKind::Brief, &format!("message {i}")));
        }
        let run = make_run(RunStatus::Done);
        let offset = FixedOffset::east_opt(0).unwrap();
        // Height 5 means overflow; at bottom, we should be able to reach the last message.
        // Max offset should be `total - (height - 1)` when overflow.
        let lines = chat_lines(&msgs, &run, 999, &offset, None, 60, 5);
        let last_text: String = lines
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(
            last_text.contains("message 10"),
            "bottom view should include the last message"
        );
    }
}
