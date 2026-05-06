use super::*;

#[test]
fn message_chunks_become_text_events() {
    let event = translate_update(
        ClientUpdate::Text {
            kind: ClientTextKind::Message,
            text: "hello".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        },
        true,
    )
    .expect("text event");

    assert_eq!(
        event,
        AcpRuntimeEvent::Text(AcpTextEvent {
            text: "hello".to_string(),
            interactive: true,
            thought: false,
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        })
    );
}

#[test]
fn message_chunks_preserve_continue_boundary() {
    let event = translate_update(
        ClientUpdate::Text {
            kind: ClientTextKind::Message,
            text: " more".to_string(),
            boundary: AcpTextBoundary::Continue,
            identity: None,
        },
        true,
    )
    .expect("text event");

    assert_eq!(
        event,
        AcpRuntimeEvent::Text(AcpTextEvent {
            text: " more".to_string(),
            interactive: true,
            thought: false,
            boundary: AcpTextBoundary::Continue,
            identity: None,
        })
    );
}

#[test]
fn thought_chunks_are_ignored() {
    let event = translate_update(
        ClientUpdate::Text {
            kind: ClientTextKind::Thought,
            text: "internal".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        },
        true,
    );

    assert_eq!(
        event,
        Some(AcpRuntimeEvent::Text(AcpTextEvent {
            text: "internal".to_string(),
            interactive: true,
            thought: true,
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        }))
    );
}

#[test]
fn tool_call_text_becomes_thought_event_with_paragraph_break() {
    let event = translate_update(
        ClientUpdate::Text {
            kind: ClientTextKind::Tool,
            text: "tool: read(Cargo.toml)".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        },
        false,
    );

    assert_eq!(
        event,
        Some(AcpRuntimeEvent::Text(AcpTextEvent {
            text: "tool: read(Cargo.toml)\n\n".to_string(),
            interactive: false,
            thought: true,
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        }))
    );
}

#[test]
fn tool_call_text_translates_unchanged_for_result_blocks() {
    let event = translate_update(
        ClientUpdate::Text {
            kind: ClientTextKind::Tool,
            text: "result: completed, exit 0, output: ok".to_string(),
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        },
        true,
    );

    assert_eq!(
        event,
        Some(AcpRuntimeEvent::Text(AcpTextEvent {
            text: "result: completed, exit 0, output: ok\n\n".to_string(),
            interactive: true,
            thought: true,
            boundary: AcpTextBoundary::StartNewMessage,
            identity: None,
        }))
    );
}

#[test]
fn unknown_updates_are_ignored() {
    let event = translate_update(
        ClientUpdate::Unknown {
            kind: "future_update".to_string(),
        },
        false,
    );

    assert!(event.is_none());
}

#[test]
fn text_accumulator_flushes_bounded_prompt_turn_blocks() {
    let mut accumulator = AcpTextAccumulator::with_max_chars(10);

    assert!(accumulator.push("hello").is_none());
    assert_eq!(accumulator.push(" world"), Some("hello worl".to_string()));
    assert_eq!(
        accumulator.finish_prompt_turn(),
        Some("d".to_string()),
        "overflow text remains in the current prompt-turn block"
    );
}

#[test]
fn text_accumulator_ignores_empty_chunks() {
    let mut accumulator = AcpTextAccumulator::with_max_chars(8);

    assert!(accumulator.push("").is_none());
    assert!(accumulator.finish_prompt_turn().is_none());
}

#[test]
fn text_accumulator_keeps_partial_text_live_and_splits_paragraphs() {
    let mut accumulator = AcpTextAccumulator::with_max_chars(80);

    assert!(accumulator.push("thinking").is_none());
    assert_eq!(accumulator.current_text(), Some("thinking"));

    assert_eq!(
        accumulator.push(" aloud\n\nnext thought"),
        Some("thinking aloud".to_string())
    );
    assert_eq!(accumulator.current_text(), Some("next thought"));
    assert_eq!(
        accumulator.finish_prompt_turn(),
        Some("next thought".to_string())
    );
}

#[test]
fn finish_prompt_turn_drains_queued_ready_blocks_before_live_buffer() {
    // `finish_prompt_turn` returns one block per call so callers can persist
    // each finalized block before the live remainder. Drain order: ready
    // blocks first (in arrival order), then the live buffer.
    let mut accumulator = AcpTextAccumulator::with_max_chars(80);

    assert_eq!(
        accumulator.push("first paragraph\n\nsecond paragraph\n\nlive"),
        Some("first paragraph".to_string())
    );
    assert_eq!(
        accumulator.finish_prompt_turn(),
        Some("second paragraph".to_string())
    );
    assert_eq!(accumulator.finish_prompt_turn(), Some("live".to_string()));
    assert!(accumulator.finish_prompt_turn().is_none());
}
