pub const EXPLICIT_SCORE_FALLBACKS: &[(&str, &str)] = &[
    ("gemini-3.1-pro-preview", "gemini-3-pro-preview"),
    ("gemini-3-flash-preview", "gemini-2.5-flash"),
];

pub const GEMINI_KNOWN_QUOTA_MODELS: &[&str] = &[
    "gemini-3.1-pro-preview",
    "gemini-3-pro-preview",
    "gemini-3-flash-preview",
    "gemini-2.5-pro",
    "gemini-2.5-flash",
];

const EXPLICIT_DISPLAY_NAMES: &[(&str, &str)] = &[
    ("gemini-3.1-pro-preview", "3.1-pro"),
    ("gemini-3-pro-preview", "3-pro"),
    ("gemini-3-flash-preview", "3-flash"),
];

pub fn display_name(canonical: &str) -> String {
    explicit_display_name(canonical)
        .unwrap_or(canonical)
        .to_string()
}

pub fn display_name_for_vendor(canonical: &str, vendor_prefix: &str) -> String {
    if let Some(display) = explicit_display_name(canonical) {
        display.to_string()
    } else {
        canonical
            .strip_prefix(vendor_prefix)
            .unwrap_or(canonical)
            .to_string()
    }
}

pub fn run_label_name(canonical: &str) -> String {
    if let Some(display) = explicit_display_name(canonical) {
        display.to_string()
    } else {
        canonical
            .strip_prefix("claude-")
            .unwrap_or(canonical)
            .to_string()
    }
}

fn explicit_display_name(canonical: &str) -> Option<&'static str> {
    EXPLICIT_DISPLAY_NAMES
        .iter()
        .find(|(name, _)| *name == canonical)
        .map(|(_, display)| *display)
}

#[cfg(test)]
#[path = "model_names_tests.rs"]
mod tests;
