use chrono::{Datelike, FixedOffset, TimeZone, Timelike, Utc};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

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

struct RenderedLine {
    spans: Vec<Span<'static>>,
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

        let wrapped = wrap_text(&msg.text, content_width);
        let hints = kind_to_hints(msg.kind, run.status);

        if let Some((first, rest)) = wrapped.split_first() {
            let first_line =
                format_historical_message(&ts_str, sym.symbol, first, sym.color, hints);
            lines.push(RenderedLine {
                spans: first_line.spans,
            });

            let body_style = if hints.is_error {
                Style::default().fg(Color::Red)
            } else if hints.is_warning {
                Style::default().fg(Color::Yellow)
            } else if hints.is_summary {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::White)
            };
            for chunk in rest {
                lines.push(RenderedLine {
                    spans: vec![Span::styled(format!("{}{}", indent, chunk), body_style)],
                });
            }
        } else {
            let first_line = format_historical_message(&ts_str, sym.symbol, "", sym.color, hints);
            lines.push(RenderedLine {
                spans: first_line.spans,
            });
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

        let has_overflow = total > height;
        let max_offset = if has_overflow {
            total.saturating_sub(height.saturating_sub(1))
        } else {
            0
        };
        let offset = self.scroll_offset.min(max_offset);

        let show_above_indicator = offset > 0;
        let mut message_rows = height.saturating_sub(show_above_indicator as usize);
        let show_below_indicator = total > offset.saturating_add(message_rows);
        if show_below_indicator {
            message_rows = message_rows.saturating_sub(1);
        }

        let vis_end = (offset + message_rows).min(total);
        let above = offset;
        let below = total.saturating_sub(vis_end);

        let mut row = area.y;

        if show_above_indicator {
            let indicator = format!("  ↑ {} more above", above);
            let line = Line::from(Span::styled(
                indicator,
                Style::default().fg(Color::DarkGray),
            ));
            buf.set_line(area.x, row, &line, area.width);
            row += 1;
        }

        for line in &all_lines[offset..vis_end] {
            let line = line.clone();
            buf.set_line(area.x, row, &line, area.width);
            row += 1;
        }

        if show_below_indicator {
            let indicator = format!("  ↓ {} more below", below);
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

    let has_overflow = total > available_height;
    let max_offset = if has_overflow {
        total.saturating_sub(available_height.saturating_sub(1))
    } else {
        0
    };
    let offset = scroll_offset.min(max_offset);

    let show_above = offset > 0;
    let mut message_rows = available_height.saturating_sub(show_above as usize);
    let show_below = total > offset.saturating_add(message_rows);
    if show_below {
        message_rows = message_rows.saturating_sub(1);
    }

    let vis_end = (offset + message_rows).min(total);
    let above = offset;
    let below = total.saturating_sub(vis_end);

    let mut lines = Vec::new();

    if show_above {
        lines.push(Line::from(Span::styled(
            format!("  ↑ {} more above", above),
            Style::default().fg(Color::DarkGray),
        )));
    }

    for line in &all_lines[offset..vis_end] {
        lines.push(line.clone());
    }

    if show_below {
        lines.push(Line::from(Span::styled(
            format!("  ↓ {} more below", below),
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
