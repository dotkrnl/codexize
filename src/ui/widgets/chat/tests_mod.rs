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
    let s = message_symbol(MessageKind::Started, RunStatus::Running, false, 0);
    assert_eq!(s.symbol, "○");
    assert_eq!(s.color, Color::DarkGray);
}

#[test]
fn symbol_started_animates_when_running_and_enabled() {
    let s = message_symbol(MessageKind::Started, RunStatus::Running, true, 0);
    assert_ne!(s.symbol, "○");
    assert_eq!(s.symbol, spinner_frame(0));
    assert_eq!(s.color, Color::Blue);
}

#[test]
fn symbol_started_static_when_animation_disabled() {
    let s = message_symbol(MessageKind::Started, RunStatus::Running, false, 5);
    assert_eq!(s.symbol, "○");
    assert_eq!(s.color, Color::DarkGray);
}

#[test]
fn symbol_started_static_when_not_running() {
    // Animation toggle is harmless once the run finished — symbol stays static.
    let s = message_symbol(MessageKind::Started, RunStatus::Done, true, 3);
    assert_eq!(s.symbol, "○");
    assert_eq!(s.color, Color::DarkGray);
}

#[test]
fn symbol_brief() {
    let s = message_symbol(MessageKind::Brief, RunStatus::Running, false, 0);
    assert_eq!(s.symbol, "◐");
    assert_eq!(s.color, Color::Cyan);
}

#[test]
fn symbol_user_input() {
    let s = message_symbol(MessageKind::UserInput, RunStatus::Running, false, 0);
    assert_eq!(s.symbol, "›");
    assert_eq!(s.color, Color::Magenta);
}

#[test]
fn symbol_end_done() {
    let s = message_symbol(MessageKind::End, RunStatus::Done, false, 0);
    assert_eq!(s.symbol, "●");
    assert_eq!(s.color, Color::Green);
}

#[test]
fn symbol_end_failed() {
    let s = message_symbol(MessageKind::End, RunStatus::Failed, false, 0);
    assert_eq!(s.symbol, "✗");
    assert_eq!(s.color, Color::Red);
}

#[test]
fn symbol_end_failed_unverified() {
    let s = message_symbol(MessageKind::End, RunStatus::FailedUnverified, false, 0);
    assert_eq!(s.symbol, "!");
    assert_eq!(s.color, Color::Yellow);
}

#[test]
fn message_lines_animates_started_when_enabled() {
    let msgs = vec![make_msg(MessageKind::Started, "agent started")];
    let run = make_run(RunStatus::Running);
    let offset = FixedOffset::east_opt(0).unwrap();
    let lines = message_lines(&msgs, &run, &offset, None, 60, 2, true);
    let symbol_span = &lines[0].spans[1];
    assert_eq!(
        symbol_span.content.trim(),
        spinner_frame(2),
        "Started symbol should animate when animate_started=true and run is Running"
    );
    assert_eq!(symbol_span.style.fg, Some(Color::Blue));
}

#[test]
fn message_lines_keeps_static_started_when_animation_disabled() {
    let msgs = vec![make_msg(MessageKind::Started, "agent started")];
    let run = make_run(RunStatus::Running);
    let offset = FixedOffset::east_opt(0).unwrap();
    let lines = message_lines(&msgs, &run, &offset, None, 60, 7, false);
    let symbol_span = &lines[0].spans[1];
    assert_eq!(
        symbol_span.content.trim(),
        "○",
        "Started symbol should stay static when animate_started=false"
    );
    assert_eq!(symbol_span.style.fg, Some(Color::DarkGray));
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
    let lines = message_lines(
        &msgs,
        &run,
        &offset,
        Some(tail_line("LIVE-TAIL")),
        60,
        0,
        false,
    );
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
    let lines = message_lines(&msgs, &run, &offset, None, 80, 0, false);
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
            .any(|span| span.content == "please continue" && span.style.fg == Some(Color::Magenta)),
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
    let lines = message_lines(&msgs, &run, &offset, None, 80, 0, false);

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
    let lines = message_lines(&msgs, &run, &offset, None, 80, 0, false);
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
    let lines = message_lines(&msgs, &run, &offset, None, 80, 0, false);
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
fn user_input_echo_gets_trailing_blank_line() {
    let msgs = vec![make_msg(MessageKind::UserInput, "please continue")];
    let run = make_run(RunStatus::Running);
    let offset = FixedOffset::east_opt(0).unwrap();
    let lines = message_lines(&msgs, &run, &offset, None, 80, 0, false);
    let texts = lines.iter().map(line_text).collect::<Vec<_>>();

    assert!(texts[0].contains("› please continue"));
    assert_eq!(texts[1], "", "user echo should be followed by spacing");
}

#[test]
fn noninteractive_acp_agent_outputs_get_padding() {
    let msgs = vec![
        make_msg(MessageKind::AgentThought, "thinking"),
        make_msg(MessageKind::AgentText, "answer"),
    ];
    let run = make_run(RunStatus::Running);
    let offset = FixedOffset::east_opt(0).unwrap();
    let lines = message_lines(&msgs, &run, &offset, None, 80, 0, false);
    let texts = lines.iter().map(line_text).collect::<Vec<_>>();

    assert!(texts[0].contains("· Thinking"), "{texts:?}");
    assert_eq!(texts[1], "");
    assert!(texts[2].contains("▸ answer"), "{texts:?}");
    assert_eq!(texts[3], "");
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
    let lines = message_lines(&msgs, &run, &offset, None, 80, 0, false);
    let text = line_text(&lines[0]);

    assert!(text.contains("Here is bold text and code."));
    assert!(!text.contains("**"));
    assert!(!text.contains('`'));
    assert!(
        lines[0]
            .spans
            .iter()
            .any(|span| span.content == "bold text"
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
    let lines = message_lines(&msgs, &run, &offset, None, 80, 0, false);
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
    let lines = message_lines(&msgs, &run, &offset, None, 60, 0, false);
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
    let lines = message_lines(
        &msgs,
        &run,
        &offset,
        Some(tail_line("LIVE-TAIL")),
        60,
        0,
        false,
    );
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
    let lines = chat_lines(&msgs, &run, 5, &offset, None, 60, 10, 0, false);
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
    let lines = render_messages(&[msg], &run, &offset, 30, 0, false);
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
    let lines = chat_lines(&msgs, &run, 999, &offset, None, 60, 5, 0, false);
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
