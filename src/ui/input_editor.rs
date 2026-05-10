//! Shared Unix-style readline-ish key handling for text input buffers.
//!
//! Tracks a cursor as a *char* index into the buffer. Callers own the
//! `String` and `usize` cursor; this module only mutates them in response
//! to key events. Returns `true` when a key is consumed, `false` to let
//! the caller handle it (e.g. Enter, Esc).
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
pub fn apply(buffer: &mut String, cursor: &mut usize, key: KeyEvent) -> bool {
    let len = buffer.chars().count();
    if *cursor > len {
        *cursor = len;
    }
    let byte_cursor = char_to_byte(buffer, *cursor);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Home => {
            *cursor = 0;
            true
        }
        KeyCode::End => {
            *cursor = len;
            true
        }
        KeyCode::Left => {
            if *cursor > 0 {
                *cursor -= 1;
            }
            true
        }
        KeyCode::Right => {
            if *cursor < len {
                *cursor += 1;
            }
            true
        }
        KeyCode::Delete => {
            delete_forward(buffer, cursor);
            true
        }
        KeyCode::Backspace => {
            delete_backward(buffer, cursor);
            true
        }
        KeyCode::Char('a') if ctrl => {
            *cursor = 0;
            true
        }
        KeyCode::Char('e') if ctrl => {
            *cursor = len;
            true
        }
        KeyCode::Char('b') if ctrl => {
            if *cursor > 0 {
                *cursor -= 1;
            }
            true
        }
        KeyCode::Char('f') if ctrl => {
            if *cursor < len {
                *cursor += 1;
            }
            true
        }
        KeyCode::Char('d') if ctrl => {
            delete_forward(buffer, cursor);
            true
        }
        KeyCode::Char('h') if ctrl => {
            delete_backward(buffer, cursor);
            true
        }
        KeyCode::Char('u') if ctrl => {
            buffer.replace_range(..byte_cursor, "");
            *cursor = 0;
            true
        }
        KeyCode::Char('k') if ctrl => {
            buffer.truncate(byte_cursor);
            true
        }
        KeyCode::Char('w') if ctrl => {
            delete_word_backward(buffer, cursor);
            true
        }
        KeyCode::Char(c) if !ctrl && !alt => {
            buffer.insert(byte_cursor, c);
            *cursor += 1;
            true
        }
        _ => false,
    }
}
pub fn insert_str(buffer: &mut String, cursor: &mut usize, s: &str) {
    let len = buffer.chars().count();
    if *cursor > len {
        *cursor = len;
    }
    let byte_cursor = char_to_byte(buffer, *cursor);
    let normalized: String = s.replace("\r\n", "\n").replace('\r', "\n");
    let inserted_chars = normalized.chars().count();
    buffer.insert_str(byte_cursor, &normalized);
    *cursor += inserted_chars;
}
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}
fn delete_backward(buffer: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let prev = char_to_byte(buffer, *cursor - 1);
    let here = char_to_byte(buffer, *cursor);
    buffer.replace_range(prev..here, "");
    *cursor -= 1;
}
fn delete_forward(buffer: &mut String, cursor: &mut usize) {
    let len = buffer.chars().count();
    if *cursor >= len {
        return;
    }
    let here = char_to_byte(buffer, *cursor);
    let next = char_to_byte(buffer, *cursor + 1);
    buffer.replace_range(here..next, "");
}
fn delete_word_backward(buffer: &mut String, cursor: &mut usize) {
    let chars: Vec<char> = buffer.chars().collect();
    let mut end_char = *cursor;
    while end_char > 0 && chars[end_char - 1].is_whitespace() {
        end_char -= 1;
    }
    while end_char > 0 && !chars[end_char - 1].is_whitespace() {
        end_char -= 1;
    }
    let start_byte = char_to_byte(buffer, end_char);
    let here = char_to_byte(buffer, *cursor);
    buffer.replace_range(start_byte..here, "");
    *cursor = end_char;
}
#[cfg(test)]
#[path = "input_editor_tests.rs"]
mod tests;
