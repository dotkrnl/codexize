const DISPLAY: &[(&str, &str, &str)] = &[
    // (canonical, display_vendor, display_short)
    // Canonical keys mirror dashboard `entry.name` (lowercased ipbr
    // display_name) and the BakedRow.model field — predominantly dotted
    // where the upstream vendor uses a version dot.
    ("claude-opus-4.1", "claude", "opus 4.1"),
    ("claude-opus-4.5", "claude", "opus 4.5"),
    ("claude-opus-4.6", "claude", "opus 4.6"),
    ("claude-opus-4.7", "claude", "opus 4.7"),
    ("claude-sonnet-4", "claude", "sonnet 4"),
    ("claude-sonnet-4.5", "claude", "sonnet 4.5"),
    ("claude-sonnet-4.6", "claude", "sonnet 4.6"),
    ("gpt-5.2", "gpt", "5.2"),
    ("gpt-5.3-codex", "gpt", "5.3 codex"),
    ("gpt-5.4", "gpt", "5.4"),
    ("gpt-5.5", "gpt", "5.5"),
    ("gemini-2.5-flash", "gemini", "2.5 flash"),
    ("gemini-2.5-pro", "gemini", "2.5 pro"),
    ("gemini-3-flash", "gemini", "3 flash"),
    ("gemini-3-pro", "gemini", "3 pro"),
    ("gemini-3.1-pro-preview", "gemini", "3.1 pro"),
    ("kimi-k2-0905", "kimi", "k2 0905"),
    ("kimi-k2.5", "kimi", "2.5"),
    ("kimi-k2.6", "kimi", "2.6"),
    ("deepseek-v4-flash", "deepseek", "v4 flash"),
    ("deepseek-v4-pro", "deepseek", "v4 pro"),
    ("minimax-m2.5", "minimax", "m2.5"),
    ("minimax-m2.7", "minimax", "m2.7"),
    ("qwen3.5-plus", "qwen", "3.5 plus"),
    ("qwen3.6-plus", "qwen", "3.6 plus"),
    ("mimo-v2.5", "mimo", "v2.5"),
    ("mimo-v2.5-pro", "mimo", "v2.5 pro"),
    ("grok-4-latest", "grok", "4 latest"),
    ("grok-code-fast-1", "grok", "code fast 1"),
    ("glm-4.6", "glm", "4.6"),
    ("glm-4.7", "glm", "4.7"),
    ("glm-5", "glm", "5"),
    ("glm-5.1", "glm", "5.1"),
];

pub fn display_vendor(canonical: &str) -> Option<&'static str> {
    display_entry(canonical).map(|(_, v, _)| v)
}

pub fn display_short(canonical: &str) -> Option<&'static str> {
    display_entry(canonical).map(|(_, _, s)| s)
}

pub fn is_curated(canonical: &str) -> bool {
    display_entry(canonical).is_some()
}

pub fn run_label_name(canonical: &str) -> String {
    canonical
        .strip_prefix("claude-")
        .unwrap_or(canonical)
        .to_string()
}

fn display_entry(canonical: &str) -> Option<(&'static str, &'static str, &'static str)> {
    DISPLAY
        .iter()
        .copied()
        .find(|(candidate, _, _)| *candidate == canonical)
}
