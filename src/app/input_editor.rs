fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}
pub fn apply(buffer: &mut String, cursor: &mut usize, key: crate::app::keys::UiKey) -> bool {
    let len = buffer.chars().count();
    if *cursor > len {
        *cursor = len;
    }
    let byte_cursor = char_to_byte(buffer, *cursor);
    match key.code {
        crate::app::keys::UiKeyCode::Home => {
            *cursor = 0;
            true
        }
        crate::app::keys::UiKeyCode::End => {
            *cursor = len;
            true
        }
        crate::app::keys::UiKeyCode::Left => {
            if *cursor > 0 {
                *cursor -= 1;
            }
            true
        }
        crate::app::keys::UiKeyCode::Right => {
            if *cursor < len {
                *cursor += 1;
            }
            true
        }
        crate::app::keys::UiKeyCode::Delete => {
            delete_forward(buffer, cursor);
            true
        }
        crate::app::keys::UiKeyCode::Backspace => {
            delete_backward(buffer, cursor);
            true
        }
        crate::app::keys::UiKeyCode::Char('a') if key.ctrl => {
            *cursor = 0;
            true
        }
        crate::app::keys::UiKeyCode::Char('e') if key.ctrl => {
            *cursor = len;
            true
        }
        crate::app::keys::UiKeyCode::Char('b') if key.ctrl => {
            if *cursor > 0 {
                *cursor -= 1;
            }
            true
        }
        crate::app::keys::UiKeyCode::Char('f') if key.ctrl => {
            if *cursor < len {
                *cursor += 1;
            }
            true
        }
        crate::app::keys::UiKeyCode::Char('d') if key.ctrl => {
            delete_forward(buffer, cursor);
            true
        }
        crate::app::keys::UiKeyCode::Char('h') if key.ctrl => {
            delete_backward(buffer, cursor);
            true
        }
        crate::app::keys::UiKeyCode::Char('u') if key.ctrl => {
            buffer.replace_range(..byte_cursor, "");
            *cursor = 0;
            true
        }
        crate::app::keys::UiKeyCode::Char('k') if key.ctrl => {
            buffer.truncate(byte_cursor);
            true
        }
        crate::app::keys::UiKeyCode::Char('w') if key.ctrl => {
            delete_word_backward(buffer, cursor);
            true
        }
        crate::app::keys::UiKeyCode::Char(c) if !key.ctrl && !key.alt => {
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

fn delete_word_forward(buffer: &mut String, cursor: &mut usize) {
    let chars: Vec<char> = buffer.chars().collect();
    let total = chars.len();
    let mut end_char = *cursor;
    while end_char < total && chars[end_char].is_whitespace() {
        end_char += 1;
    }
    while end_char < total && !chars[end_char].is_whitespace() {
        end_char += 1;
    }
    let here = char_to_byte(buffer, *cursor);
    let end_byte = char_to_byte(buffer, end_char);
    buffer.replace_range(here..end_byte, "");
}

/// Apply a typed [`InputCommand`] to a buffer/cursor pair. Returns true
/// when the command mutated the buffer or moved the cursor.
pub fn apply_input_command(
    buffer: &mut String,
    cursor: &mut usize,
    cmd: &crate::app_runtime::InputCommand,
) -> bool {
    use crate::app_runtime::{CursorMove, InputCommand};
    let len = buffer.chars().count();
    if *cursor > len {
        *cursor = len;
    }
    match cmd {
        InputCommand::InsertText(text) => {
            insert_str(buffer, cursor, text);
            true
        }
        InputCommand::ReplaceBuffer(text) => {
            *buffer = text.clone();
            *cursor = buffer.chars().count();
            true
        }
        InputCommand::Backspace => {
            delete_backward(buffer, cursor);
            true
        }
        InputCommand::DeleteForward => {
            delete_forward(buffer, cursor);
            true
        }
        InputCommand::DeleteWordBack => {
            delete_word_backward(buffer, cursor);
            true
        }
        InputCommand::DeleteWordForward => {
            delete_word_forward(buffer, cursor);
            true
        }
        InputCommand::MoveCursor(mv) => match mv {
            CursorMove::Left => {
                if *cursor > 0 {
                    *cursor -= 1;
                }
                true
            }
            CursorMove::Right => {
                if *cursor < len {
                    *cursor += 1;
                }
                true
            }
            CursorMove::Home | CursorMove::LineUp => {
                *cursor = 0;
                true
            }
            CursorMove::End | CursorMove::LineDown => {
                *cursor = len;
                true
            }
            CursorMove::WordLeft => {
                let chars: Vec<char> = buffer.chars().collect();
                let mut idx = *cursor;
                while idx > 0 && chars[idx - 1].is_whitespace() {
                    idx -= 1;
                }
                while idx > 0 && !chars[idx - 1].is_whitespace() {
                    idx -= 1;
                }
                *cursor = idx;
                true
            }
            CursorMove::WordRight => {
                let chars: Vec<char> = buffer.chars().collect();
                let mut idx = *cursor;
                while idx < chars.len() && !chars[idx].is_whitespace() {
                    idx += 1;
                }
                while idx < chars.len() && chars[idx].is_whitespace() {
                    idx += 1;
                }
                *cursor = idx;
                true
            }
        },
        // Submit/Cancel are flow-control; surfaces route these through
        // their own typed handler instead of editing the buffer.
        InputCommand::Submit | InputCommand::Cancel => false,
    }
}
