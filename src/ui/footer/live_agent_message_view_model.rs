use ratatui::style::{Color, Style};
use ratatui::text::Span;
const GRADIENT_STOPS: &[(u8, u8, u8)] = &[
    (0xFF, 0x6B, 0x6B),
    (0xFF, 0xD1, 0x66),
    (0x06, 0xD6, 0xA0),
    (0x4C, 0xC9, 0xF0),
    (0x7B, 0x5B, 0xE0),
    (0xF0, 0x72, 0xB6),
];
const GRADIENT_STEP: usize = 4;
pub(crate) fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
pub(super) fn interpolate_rgb(
    from: (u8, u8, u8),
    to: (u8, u8, u8),
    step: usize,
    max_step: usize,
) -> Color {
    let interpolate_channel = |start: u8, end: u8| {
        let delta = end as i16 - start as i16;
        (start as i16 + (delta * step as i16) / max_step as i16) as u8
    };
    Color::Rgb(
        interpolate_channel(from.0, to.0),
        interpolate_channel(from.1, to.1),
        interpolate_channel(from.2, to.2),
    )
}
pub(super) fn gradient_spans(text: &str, phase: usize) -> Vec<Span<'static>> {
    if text.is_empty() {
        return Vec::new();
    }
    let cycle = GRADIENT_STOPS.len() * GRADIENT_STEP;
    let mut spans = Vec::with_capacity(text.chars().count());
    for (index, ch) in text.chars().enumerate() {
        let offset = (phase + index) % cycle;
        let start_index = offset / GRADIENT_STEP;
        let step = offset % GRADIENT_STEP;
        let end_index = (start_index + 1) % GRADIENT_STOPS.len();
        let color = interpolate_rgb(
            GRADIENT_STOPS[start_index],
            GRADIENT_STOPS[end_index],
            step,
            GRADIENT_STEP,
        );
        spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
    }
    spans
}
pub fn extract_short_title(text: &str) -> String {
    if let Some((title, _)) = text.split_once('|') {
        title.trim().to_string()
    } else {
        text.trim().to_string()
    }
}
#[cfg(test)]
#[path = "live_agent_message_view_model_tests.rs"]
mod tests;
