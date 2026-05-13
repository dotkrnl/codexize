//! Shared TOML formatting helpers for defaults emitter, sparse saver, and CLI/TUI formatters.

use std::collections::BTreeMap;

/// Quote a string value for TOML output, escaping `"`, `\`, and TOML
/// control characters as `\uXXXX`.
pub(crate) fn toml_quote(value: &str) -> String {
    let mut s = String::with_capacity(value.len() + 2);
    s.push('"');
    for ch in value.chars() {
        match ch {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                use std::fmt::Write;
                let _ = write!(s, "\\u{:04X}", c as u32);
            }
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

/// Format a `Vec<String>` as a TOML inline array of quoted strings.
pub(crate) fn format_string_array(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".to_string();
    }
    let mut s = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&toml_quote(item));
    }
    s.push(']');
    s
}

/// Format a `BTreeMap<String, String>` as a TOML inline table (`{ key = "value", ... }`).
pub(crate) fn format_inline_env(env: &BTreeMap<String, String>) -> String {
    if env.is_empty() {
        return "{}".to_string();
    }
    let mut s = String::from("{ ");
    for (i, (k, v)) in env.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&toml_quote(k));
        s.push_str(" = ");
        s.push_str(&toml_quote(v));
    }
    s.push_str(" }");
    s
}

#[cfg(test)]
mod tests {
    use super::toml_quote;

    #[test]
    fn toml_quote_escapes_del_control_character() {
        assert_eq!(toml_quote("a\u{7f}b"), "\"a\\u007Fb\"");
    }
}
