use crate::app::chat_widget_view_model::chat_scroll_window;
use crate::app::footer::{HistoricalStyleHints, capitalize_first, format_historical_message};
use crate::app::render_view_model::spinner_frame;
use crate::state::{Message, MessageKind, RunRecord, RunStatus};
use chrono::{Datelike, FixedOffset, TimeZone, Timelike, Utc};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};
pub struct ChatWidget<'a> {
    messages: &'a [Message],
    run: &'a RunRecord,
    scroll_offset: usize,
    local_offset: FixedOffset,
    running_tail: Option<Line<'static>>,
    spinner_tick: usize,
    animate_started: bool,
}
impl<'a> ChatWidget<'a> {
    pub fn new(
        messages: &'a [Message],
        run: &'a RunRecord,
        scroll_offset: usize,
        local_offset: FixedOffset,
        running_tail: Option<Line<'static>>,
        spinner_tick: usize,
        animate_started: bool,
    ) -> Self {
        Self {
            messages,
            run,
            scroll_offset,
            local_offset,
            running_tail,
            spinner_tick,
            animate_started,
        }
    }
}
struct SymbolStyle {
    symbol: &'static str,
    color: Color,
}
fn ss(symbol: &'static str, color: Color) -> SymbolStyle {
    SymbolStyle { symbol, color }
}
fn message_symbol(
    kind: MessageKind,
    run_status: RunStatus,
    animate_started: bool,
    spinner_tick: usize,
) -> SymbolStyle {
    match kind {
        MessageKind::Started => {
            if animate_started && run_status == RunStatus::Running {
                ss(spinner_frame(spinner_tick), Color::Blue)
            } else {
                ss("○", Color::DarkGray)
            }
        }
        MessageKind::Brief => ss("◐", Color::Cyan),
        MessageKind::UserInput => ss("›", Color::Magenta),
        MessageKind::AgentText => ss("▸", Color::White),
        MessageKind::AgentThought => ss("·", Color::DarkGray),
        MessageKind::Summary => ss("✓", Color::Green),
        MessageKind::SummaryWarn => ss("⚠", Color::Yellow),
        MessageKind::End => match run_status {
            RunStatus::Done => ss("●", Color::Green),
            RunStatus::FailedUnverified => ss("!", Color::Yellow),
            _ => ss("✗", Color::Red),
        },
    }
}
fn hint(is_summary: bool, is_warning: bool, is_dim: bool, is_error: bool) -> HistoricalStyleHints {
    HistoricalStyleHints {
        is_summary,
        is_warning,
        is_dim,
        is_error,
    }
}
fn kind_to_hints(kind: MessageKind, run_status: RunStatus) -> HistoricalStyleHints {
    match kind {
        MessageKind::Summary => hint(true, false, false, false),
        MessageKind::SummaryWarn => hint(false, true, false, false),
        MessageKind::AgentThought => hint(false, false, true, false),
        MessageKind::End => match run_status {
            RunStatus::Failed | RunStatus::FailedUnverified => hint(false, false, false, true),
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
use crate::ui::tui::{strip_ansi, wrap_lines_with_prefix, wrap_text};
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
fn flush_if_non_empty(
    lines: &mut Vec<Vec<Span<'static>>>,
    current: &mut Vec<Span<'static>>,
    width: usize,
) {
    if !current.is_empty() {
        push_wrapped_span_line(lines, current, width);
    }
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
                .map_or(remaining.len(), |(i, _)| i);
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
                flush_if_non_empty(&mut lines, &mut current, content_width);
            }
            Event::End(TagEnd::Paragraph) => {}
            Event::Start(Tag::Heading { .. }) => {
                flush_if_non_empty(&mut lines, &mut current, content_width);
                style_stack.push(heading_style);
            }
            Event::End(TagEnd::Heading(_)) => {
                flush_if_non_empty(&mut lines, &mut current, content_width);
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
                flush_if_non_empty(&mut lines, &mut current, content_width);
                let indent = "  ".repeat(list_depth.saturating_sub(1));
                current.push(Span::styled(format!("{indent}• "), base_style));
            }
            Event::End(TagEnd::Item) if !current.is_empty() => {
                flush_if_non_empty(&mut lines, &mut current, content_width);
            }
            Event::End(TagEnd::Item) => {}
            Event::Start(Tag::CodeBlock(_)) => {
                flush_if_non_empty(&mut lines, &mut current, content_width);
                in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                flush_if_non_empty(&mut lines, &mut current, content_width);
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
                flush_if_non_empty(&mut lines, &mut current, content_width);
                lines.push(vec![Span::styled("─".repeat(content_width), base_style)]);
            }
            _ => {}
        }
    }
    flush_if_non_empty(&mut lines, &mut current, content_width);
    if lines.is_empty() {
        wrap_text(text, content_width)
            .into_iter()
            .map(|line| vec![Span::styled(line, base_style)])
            .collect()
    } else {
        lines
    }
}
fn push_blank_line_if_needed(lines: &mut Vec<Line<'static>>) {
    if lines
        .last()
        .is_some_and(|line| !line.spans.iter().all(|span| span.content.is_empty()))
    {
        lines.push(Line::from(Vec::<Span<'static>>::new()));
    }
}
fn capitalize_first_span(spans: &[Span<'static>]) -> Vec<Span<'static>> {
    let mut capitalized = spans.to_vec();
    for span in &mut capitalized {
        let mut chars = span.content.chars();
        let Some(first) = chars.next() else {
            continue;
        };
        let text = first.to_uppercase().collect::<String>() + chars.as_str();
        span.content = text.into();
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
    spinner_tick: usize,
    animate_started: bool,
) -> Vec<Line<'static>> {
    let mut lines = render_messages(
        messages,
        run,
        local_offset,
        available_width,
        spinner_tick,
        animate_started,
    );
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
    spinner_tick: usize,
    animate_started: bool,
) -> Vec<Line<'static>> {
    let now_local = local_offset.from_utc_datetime(&Utc::now().naive_utc());
    let today_local = now_local.date_naive();
    let mut lines: Vec<Line<'static>> = Vec::new();
    for msg in messages {
        let ts_str = format_timestamp(&msg.ts, local_offset, today_local);
        let ts_w = ts_str.chars().count();
        let sym = message_symbol(msg.kind, run.status, animate_started, spinner_tick);
        let prefix_width = ts_w + 3; // " ○ "
        let ts_sym_prefix = || -> Vec<Span<'static>> {
            vec![
                Span::styled(format!("{ts_str} "), Style::default().fg(sym.color)),
                Span::styled(format!("{} ", sym.symbol), Style::default().fg(sym.color)),
            ]
        };
        let indent_prefix = || -> Vec<Span<'static>> { vec![Span::raw(" ".repeat(prefix_width))] };
        if msg.kind == MessageKind::Brief {
            let (title, details) = match msg.text.split_once('|') {
                Some((t, d)) => (t.trim().to_string(), d.trim().to_string()),
                None => (msg.text.trim().to_string(), String::new()),
            };
            lines.extend(wrap_lines_with_prefix(
                ts_sym_prefix(),
                prefix_width,
                &title,
                Style::default().add_modifier(Modifier::BOLD),
                available_width,
            ));
            if !details.is_empty() {
                lines.extend(wrap_lines_with_prefix(
                    indent_prefix(),
                    prefix_width,
                    &details,
                    Style::default().fg(Color::White),
                    available_width,
                ));
            }
            continue;
        }
        if msg.kind == MessageKind::UserInput {
            lines.extend(wrap_lines_with_prefix(
                ts_sym_prefix(),
                prefix_width,
                &msg.text,
                Style::default().fg(Color::Magenta),
                available_width,
            ));
            push_blank_line_if_needed(&mut lines);
            continue;
        }
        let hints = kind_to_hints(msg.kind, run.status);
        let body_style = body_style_from_hints(hints);
        let renders_markdown =
            matches!(msg.kind, MessageKind::AgentText | MessageKind::AgentThought);
        if !renders_markdown {
            // Plain (non-markdown) historical messages route through the
            // shared wrap helper so prefix/wrap behavior matches every
            // other transcript surface. Capitalize the first character to
            // preserve `format_historical_message`'s look.
            let capitalized = capitalize_first(&msg.text);
            lines.extend(wrap_lines_with_prefix(
                ts_sym_prefix(),
                prefix_width,
                &capitalized,
                body_style,
                available_width,
            ));
            continue;
        }
        // Markdown-aware path keeps its own renderer because each rendered
        // line carries multiple styled spans (code blocks, emphasis,
        // inline code) that the single-style wrap helper cannot reproduce.
        let content_width = available_width.saturating_sub(prefix_width).max(1);
        let indent = " ".repeat(prefix_width);
        let markdown_style = if hints.is_dim {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::White)
        };
        push_blank_line_if_needed(&mut lines);
        let markdown_lines = render_agent_markdown(&msg.text, content_width, markdown_style);
        if let Some((first, rest)) = markdown_lines.split_first() {
            let mut first_spans = ts_sym_prefix();
            if msg.kind == MessageKind::AgentThought {
                first_spans.extend(capitalize_first_span(first));
            } else {
                first_spans.extend(first.iter().cloned());
            }
            lines.push(Line::from(first_spans));
            for chunk in rest {
                let mut spans = vec![Span::raw(indent.clone())];
                spans.extend(chunk.iter().cloned());
                lines.push(Line::from(spans));
            }
        } else {
            lines.push(format_historical_message(
                &ts_str, sym.symbol, "", sym.color, hints,
            ));
        }
        push_blank_line_if_needed(&mut lines);
    }
    lines
}
fn body_style_from_hints(hints: HistoricalStyleHints) -> Style {
    let color = if hints.is_error {
        Color::Red
    } else if hints.is_warning {
        Color::Yellow
    } else if hints.is_summary {
        Color::Green
    } else if hints.is_dim {
        Color::DarkGray
    } else {
        Color::White
    };
    Style::default().fg(color)
}
impl Widget for ChatWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        for (i, line) in chat_lines(
            self.messages,
            self.run,
            self.scroll_offset,
            &self.local_offset,
            self.running_tail,
            area.width as usize,
            area.height as usize,
            self.spinner_tick,
            self.animate_started,
        )
        .into_iter()
        .enumerate()
        {
            buf.set_line(area.x, area.y + i as u16, &line, area.width);
        }
    }
}
#[allow(clippy::too_many_arguments)]
pub fn chat_lines(
    messages: &[Message],
    run: &RunRecord,
    scroll_offset: usize,
    local_offset: &FixedOffset,
    running_tail: Option<Line<'static>>,
    available_width: usize,
    available_height: usize,
    spinner_tick: usize,
    animate_started: bool,
) -> Vec<Line<'static>> {
    let all_lines = message_lines(
        messages,
        run,
        local_offset,
        running_tail,
        available_width,
        spinner_tick,
        animate_started,
    );
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
#[path = "tests_mod.rs"]
mod tests_mod;
