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

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
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
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn insert_and_move() {
        let mut b = String::new();
        let mut c = 0usize;
        for ch in "hello".chars() {
            apply(&mut b, &mut c, key(KeyCode::Char(ch)));
        }
        assert_eq!(b, "hello");
        assert_eq!(c, 5);
        apply(&mut b, &mut c, ctrl('a'));
        assert_eq!(c, 0);
        apply(&mut b, &mut c, ctrl('e'));
        assert_eq!(c, 5);
    }

    #[test]
    fn ctrl_u_kills_to_start() {
        let mut b = "hello world".to_string();
        let mut c = 6usize;
        apply(&mut b, &mut c, ctrl('u'));
        assert_eq!(b, "world");
        assert_eq!(c, 0);
    }

    #[test]
    fn ctrl_k_kills_to_end() {
        let mut b = "hello world".to_string();
        let mut c = 5usize;
        apply(&mut b, &mut c, ctrl('k'));
        assert_eq!(b, "hello");
        assert_eq!(c, 5);
    }

    #[test]
    fn ctrl_w_deletes_word() {
        let mut b = "hello world".to_string();
        let mut c = 11usize;
        apply(&mut b, &mut c, ctrl('w'));
        assert_eq!(b, "hello ");
        assert_eq!(c, 6);
    }

    #[test]
    fn ctrl_d_deletes_forward() {
        let mut b = "abc".to_string();
        let mut c = 1usize;
        apply(&mut b, &mut c, ctrl('d'));
        assert_eq!(b, "ac");
        assert_eq!(c, 1);
    }

    #[test]
    fn unicode_cursor() {
        let mut b = "日本語".to_string();
        let mut c = 3usize;
        apply(&mut b, &mut c, KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(b, "日本");
        assert_eq!(c, 2);
    }
}
