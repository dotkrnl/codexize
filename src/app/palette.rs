pub(crate) struct PaletteCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub help: &'static str,
    pub key_hint: Option<&'static str>,
}
pub(crate) enum MatchResult<'a> {
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
    },
    Unknown {
        input: String,
    },
}
pub(crate) fn resolve<'a>(input: &str, commands: &'a [PaletteCommand]) -> MatchResult<'a> {
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
    if let Some(cmd) = commands.iter().find(|c| c.name == cmd_part) {
        return MatchResult::Exact { command: cmd, args };
    }
    if let Some(cmd) = commands.iter().find(|c| c.aliases.contains(&cmd_part)) {
        return MatchResult::Exact { command: cmd, args };
    }
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
        return MatchResult::Ambiguous { candidates };
    }
    MatchResult::Unknown {
        input: cmd_part.to_string(),
    }
}
pub(crate) fn ghost_completion<'a>(input: &str, commands: &'a [PaletteCommand]) -> Option<&'a str> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let cmd_part = input.split_once(' ').map_or(input, |(c, _)| c);
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
        Some(prefix_matches[0].name)
    } else {
        None
    }
}
pub(crate) fn filter<'a>(buffer: &str, commands: &'a [PaletteCommand]) -> Vec<&'a PaletteCommand> {
    let trimmed = buffer.trim();
    if trimmed.is_empty() {
        return commands.iter().collect();
    }
    let cmd_part = trimmed.split_once(' ').map_or(trimmed, |(c, _)| c);
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
pub(crate) fn suggestion_text(command: &PaletteCommand, width: u16) -> String {
    let name = command.name;
    let description = command.help;
    let shortcut = command.key_hint.unwrap_or("");
    let w = width as usize;
    if w == 0 {
        return String::new();
    }
    let name_len = name.chars().count();
    if name_len >= w {
        return name.chars().take(w).collect();
    }
    let mut out = String::from(name);
    let mut remaining = w - name_len;
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
pub(crate) struct PaletteState {
    pub open: bool,
    pub buffer: String,
    pub cursor: usize,
}
impl PaletteState {
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.buffer.clear();
        self.cursor = 0;
    }
    pub(crate) fn open_with_buffer(&mut self, buffer: String) {
        self.open = true;
        self.cursor = buffer.chars().count();
        self.buffer = buffer;
    }
    pub(crate) fn accept_ghost(&mut self, ghost: &str) {
        self.buffer = ghost.to_string();
        self.cursor = self.buffer.chars().count();
        self.open = false;
    }
    pub(crate) fn close(&mut self) {
        self.open = false;
        self.buffer.clear();
        self.cursor = 0;
    }
}
