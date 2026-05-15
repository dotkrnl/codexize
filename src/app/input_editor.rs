fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(i, _)| i)
}
pub fn apply(buffer: &mut String, cursor: &mut usize, key: crate::app_runtime::UiKey) -> bool {
    let len = buffer.chars().count();
    if *cursor > len {
        *cursor = len;
    }
    let byte_cursor = char_to_byte(buffer, *cursor);
    match key.code {
        crate::app_runtime::UiKeyCode::Home => {
            *cursor = 0;
            true
        }
        crate::app_runtime::UiKeyCode::End => {
            *cursor = len;
            true
        }
        crate::app_runtime::UiKeyCode::Left => {
            if *cursor > 0 {
                *cursor -= 1;
            }
            true
        }
        crate::app_runtime::UiKeyCode::Right => {
            if *cursor < len {
                *cursor += 1;
            }
            true
        }
        crate::app_runtime::UiKeyCode::Delete => {
            delete_forward(buffer, cursor);
            true
        }
        crate::app_runtime::UiKeyCode::Backspace => {
            delete_backward(buffer, cursor);
            true
        }
        crate::app_runtime::UiKeyCode::Char('a') if key.ctrl => {
            *cursor = 0;
            true
        }
        crate::app_runtime::UiKeyCode::Char('e') if key.ctrl => {
            *cursor = len;
            true
        }
        crate::app_runtime::UiKeyCode::Char('b') if key.ctrl => {
            if *cursor > 0 {
                *cursor -= 1;
            }
            true
        }
        crate::app_runtime::UiKeyCode::Char('f') if key.ctrl => {
            if *cursor < len {
                *cursor += 1;
            }
            true
        }
        crate::app_runtime::UiKeyCode::Char('d') if key.ctrl => {
            delete_forward(buffer, cursor);
            true
        }
        crate::app_runtime::UiKeyCode::Char('h') if key.ctrl => {
            delete_backward(buffer, cursor);
            true
        }
        crate::app_runtime::UiKeyCode::Char('u') if key.ctrl => {
            buffer.replace_range(..byte_cursor, "");
            *cursor = 0;
            true
        }
        crate::app_runtime::UiKeyCode::Char('k') if key.ctrl => {
            buffer.truncate(byte_cursor);
            true
        }
        crate::app_runtime::UiKeyCode::Char('w') if key.ctrl => {
            delete_word_backward(buffer, cursor);
            true
        }
        crate::app_runtime::UiKeyCode::Char(c) if !key.ctrl && !key.alt => {
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
