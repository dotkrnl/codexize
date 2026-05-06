pub struct PaletteCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub help: &'static str,
    /// Shortcut hint to advertise next to this command in the discovery
    /// browser. Set to `None` for palette-only commands (e.g. `cheap`,
    /// `yolo`) and to `Some(key)` only when a real direct keybinding
    /// exists for the current context (e.g. `Esc` for quit, `n` for new).
    pub key_hint: Option<&'static str>,
}
pub enum MatchResult<'a> {
    Exact {
        command: &'a PaletteCommand,
        args: String,
    },
    UniquePrefix {
        command: &'a PaletteCommand,
        args: String,
    },
    Ambiguous {
        candidates: Vec<&'a str>,
        #[allow(dead_code)]
        ghost: &'a str,
    },
    Unknown {
        input: String,
    },
}
pub fn resolve<'a>(input: &str, commands: &'a [PaletteCommand]) -> MatchResult<'a> {
    let input = input.trim();
    if input.is_empty() {
        return MatchResult::Unknown {
            input: String::new(),
        };
    }
    let (cmd_part, args) = match input.split_once(' ') {
        Some((cmd, rest)) => (cmd, rest.to_string()),
        None => (input, String::new()),
    };
    // 1. Exact name match
    if let Some(cmd) = commands.iter().find(|c| c.name == cmd_part) {
        return MatchResult::Exact { command: cmd, args };
    }
    // 2. Exact alias match
    if let Some(cmd) = commands.iter().find(|c| c.aliases.contains(&cmd_part)) {
        return MatchResult::Exact { command: cmd, args };
    }
    // 3. Collect prefix matches on names and aliases
    let prefix_matches: Vec<&PaletteCommand> = commands
        .iter()
        .filter(|c| {
            c.name.starts_with(cmd_part) || c.aliases.iter().any(|a| a.starts_with(cmd_part))
        })
        .collect();
    if prefix_matches.len() == 1 {
        return MatchResult::UniquePrefix {
            command: prefix_matches[0],
            args,
        };
    }
    if prefix_matches.len() > 1 {
        let candidates: Vec<&str> = prefix_matches.iter().map(|c| c.name).collect();
        let ghost = prefix_matches[0].name;
        return MatchResult::Ambiguous { candidates, ghost };
    }
    MatchResult::Unknown {
        input: cmd_part.to_string(),
    }
}
/// Compute the ghost completion text for the current buffer, if any.
pub fn ghost_completion<'a>(input: &str, commands: &'a [PaletteCommand]) -> Option<&'a str> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let cmd_part = input.split_once(' ').map(|(c, _)| c).unwrap_or(input);
    // Don't show ghost for exact matches
    if commands
        .iter()
        .any(|c| c.name == cmd_part || c.aliases.contains(&cmd_part))
    {
        return None;
    }
    let prefix_matches: Vec<&PaletteCommand> = commands
        .iter()
        .filter(|c| {
            c.name.starts_with(cmd_part) || c.aliases.iter().any(|a| a.starts_with(cmd_part))
        })
        .collect();
    if prefix_matches.len() == 1 {
        return Some(prefix_matches[0].name);
    }
    if prefix_matches.len() > 1 {
        return Some(prefix_matches[0].name);
    }
    None
}
/// Return the subset of `commands` to display in the discovery browser for
/// the current input `buffer`.
///
/// An empty (or whitespace-only) buffer surfaces every command — the empty
/// state is the discoverable browser. Otherwise, match on case-insensitive
/// substring against name and aliases. Order is preserved so callers control
/// the semantic ordering of their command set.
pub fn filter<'a>(buffer: &str, commands: &'a [PaletteCommand]) -> Vec<&'a PaletteCommand> {
    let trimmed = buffer.trim();
    if trimmed.is_empty() {
        return commands.iter().collect();
    }
    let cmd_part = trimmed.split_once(' ').map(|(c, _)| c).unwrap_or(trimmed);
    let needle = cmd_part.to_ascii_lowercase();
    commands
        .iter()
        .filter(|c| {
            c.name.to_ascii_lowercase().contains(&needle)
                || c.aliases
                    .iter()
                    .any(|a| a.to_ascii_lowercase().contains(&needle))
        })
        .collect()
}
/// Plain-text rendering of a single suggestion row clamped to `width` cells.
///
/// Width budget order: name (always preserved), then description (truncated
/// with an ellipsis before the shortcut is dropped), then shortcut. Returns
/// the rendered string padded to exactly `width` cells.
pub fn suggestion_text(command: &PaletteCommand, width: u16) -> String {
    let name = command.name;
    let description = command.help;
    let shortcut = command.key_hint.unwrap_or("");
    let w = width as usize;
    if w == 0 {
        return String::new();
    }
    let name_len = name.chars().count();
    if name_len >= w {
        // Width is too narrow for anything beyond the name; preserve as much
        // of the name as fits without truncating mid-line.
        return name.chars().take(w).collect();
    }
    let mut out = String::from(name);
    let mut remaining = w - name_len;
    // Reserve a separator gap before description.
    let desc_gap = "  ";
    let shortcut_gap = "  ";
    let want_shortcut = !shortcut.is_empty();
    let shortcut_chars = shortcut.chars().count();
    let shortcut_block = if want_shortcut {
        shortcut_gap.chars().count() + shortcut_chars
    } else {
        0
    };
    if !description.is_empty() && remaining > desc_gap.chars().count() {
        let desc_budget = remaining
            .saturating_sub(desc_gap.chars().count())
            .saturating_sub(shortcut_block);
        if desc_budget > 0 {
            let desc_chars: Vec<char> = description.chars().collect();
            let truncated: String = if desc_chars.len() <= desc_budget {
                description.to_string()
            } else if desc_budget == 1 {
                "…".to_string()
            } else {
                let mut s: String = desc_chars[..desc_budget - 1].iter().collect();
                s.push('…');
                s
            };
            out.push_str(desc_gap);
            out.push_str(&truncated);
            remaining = remaining
                .saturating_sub(desc_gap.chars().count())
                .saturating_sub(truncated.chars().count());
        }
    }
    if want_shortcut && remaining >= shortcut_block {
        let pad = remaining - shortcut_block;
        if pad > 0 {
            out.push_str(&" ".repeat(pad));
        }
        out.push_str(shortcut_gap);
        out.push_str(shortcut);
    } else if remaining > 0 {
        out.push_str(&" ".repeat(remaining));
    }
    out
}
#[derive(Debug, Default)]
pub struct PaletteState {
    pub open: bool,
    pub buffer: String,
    pub cursor: usize,
}
impl PaletteState {
    pub fn open(&mut self) {
        self.open = true;
        self.buffer.clear();
        self.cursor = 0;
    }
    pub fn open_with_buffer(&mut self, buffer: String) {
        self.open = true;
        self.cursor = buffer.chars().count();
        self.buffer = buffer;
    }
    pub fn close(&mut self) {
        self.open = false;
        self.buffer.clear();
        self.cursor = 0;
    }
    pub fn accept_ghost(&mut self, ghost: &str) {
        self.buffer = ghost.to_string();
        self.cursor = self.buffer.len();
    }
}
#[cfg(test)]
#[path = "palette_tests.rs"]
mod tests;
