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
mod tests {
    use super::*;

    fn test_commands() -> Vec<PaletteCommand> {
        vec![
            PaletteCommand {
                name: "quit",
                aliases: &["q"],
                help: "Exit the TUI",
                key_hint: Some("Esc"),
            },
            PaletteCommand {
                name: "back",
                aliases: &["b"],
                help: "Go back",
                key_hint: None,
            },
            PaletteCommand {
                name: "edit",
                aliases: &["e"],
                help: "Edit artifact",
                key_hint: None,
            },
            PaletteCommand {
                name: "cheap",
                aliases: &[],
                help: "Toggle cheap mode",
                key_hint: None,
            },
            PaletteCommand {
                name: "yolo",
                aliases: &[],
                help: "Toggle YOLO mode",
                key_hint: None,
            },
        ]
    }

    #[test]
    fn exact_name_match() {
        let cmds = test_commands();
        let result = resolve("quit", &cmds);
        assert!(matches!(result, MatchResult::Exact { command, .. } if command.name == "quit"));
    }

    #[test]
    fn exact_alias_match() {
        let cmds = test_commands();
        let result = resolve("q", &cmds);
        assert!(matches!(result, MatchResult::Exact { command, .. } if command.name == "quit"));
    }

    #[test]
    fn exact_name_with_args() {
        let cmds = test_commands();
        let result = resolve("cheap on", &cmds);
        match result {
            MatchResult::Exact { command, args } => {
                assert_eq!(command.name, "cheap");
                assert_eq!(args, "on");
            }
            _ => panic!("expected exact match"),
        }
    }

    #[test]
    fn unique_prefix_match() {
        let cmds = test_commands();
        let result = resolve("qu", &cmds);
        assert!(
            matches!(result, MatchResult::UniquePrefix { command, .. } if command.name == "quit")
        );
    }

    #[test]
    fn ambiguous_prefix_match() {
        let cmds = vec![
            PaletteCommand {
                name: "foo",
                aliases: &[],
                help: "",
                key_hint: None,
            },
            PaletteCommand {
                name: "food",
                aliases: &[],
                help: "",
                key_hint: None,
            },
        ];
        let result = resolve("fo", &cmds);
        match result {
            MatchResult::Ambiguous {
                candidates, ghost, ..
            } => {
                assert!(candidates.contains(&"foo"));
                assert!(candidates.contains(&"food"));
                assert_eq!(ghost, "foo");
            }
            _ => panic!("expected ambiguous match"),
        }
    }

    #[test]
    fn unknown_command() {
        let cmds = test_commands();
        let result = resolve("xyz", &cmds);
        assert!(matches!(result, MatchResult::Unknown { input } if input == "xyz"));
    }

    #[test]
    fn empty_input_is_unknown() {
        let cmds = test_commands();
        let result = resolve("", &cmds);
        assert!(matches!(result, MatchResult::Unknown { .. }));
    }

    #[test]
    fn whitespace_only_is_unknown() {
        let cmds = test_commands();
        let result = resolve("   ", &cmds);
        assert!(matches!(result, MatchResult::Unknown { .. }));
    }

    #[test]
    fn ghost_for_unique_prefix() {
        let cmds = test_commands();
        assert_eq!(ghost_completion("qu", &cmds), Some("quit"));
    }

    #[test]
    fn ghost_for_ambiguous_prefix() {
        let cmds = vec![
            PaletteCommand {
                name: "foo",
                aliases: &[],
                help: "",
                key_hint: None,
            },
            PaletteCommand {
                name: "food",
                aliases: &[],
                help: "",
                key_hint: None,
            },
        ];
        assert_eq!(ghost_completion("fo", &cmds), Some("foo"));
    }

    #[test]
    fn no_ghost_for_exact_match() {
        let cmds = test_commands();
        assert_eq!(ghost_completion("quit", &cmds), None);
    }

    #[test]
    fn no_ghost_for_exact_alias() {
        let cmds = test_commands();
        assert_eq!(ghost_completion("q", &cmds), None);
    }

    #[test]
    fn no_ghost_for_no_match() {
        let cmds = test_commands();
        assert_eq!(ghost_completion("xyz", &cmds), None);
    }

    #[test]
    fn palette_state_open_close_cycle() {
        let mut state = PaletteState::default();
        assert!(!state.open);

        state.open();
        assert!(state.open);
        assert!(state.buffer.is_empty());

        state.buffer.push_str("qu");
        state.cursor = 2;

        state.close();
        assert!(!state.open);
        assert!(state.buffer.is_empty());
        assert_eq!(state.cursor, 0);
    }

    #[test]
    fn filter_empty_buffer_returns_all_in_order() {
        let cmds = test_commands();
        let result = filter("", &cmds);
        assert_eq!(result.len(), cmds.len());
        let names: Vec<_> = result.iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["quit", "back", "edit", "cheap", "yolo"]);
    }

    #[test]
    fn filter_matches_name_prefix() {
        let cmds = test_commands();
        let names: Vec<_> = filter("qu", &cmds).iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["quit"]);
    }

    #[test]
    fn filter_matches_alias() {
        let cmds = test_commands();
        let names: Vec<_> = filter("e", &cmds).iter().map(|c| c.name).collect();
        // Either name or alias substring containing "e": back? no. edit? alias e, name has 'e'.
        // cheap has 'e', yolo no. quit no. back no.
        assert!(names.contains(&"edit"));
        assert!(names.contains(&"cheap"));
    }

    #[test]
    fn filter_is_case_insensitive() {
        let cmds = test_commands();
        let names: Vec<_> = filter("QUIT", &cmds).iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["quit"]);
    }

    #[test]
    fn filter_no_match_is_empty() {
        let cmds = test_commands();
        assert!(filter("zzz", &cmds).is_empty());
    }

    #[test]
    fn suggestion_text_full_width_includes_name_help_and_shortcut() {
        let cmd = PaletteCommand {
            name: "quit",
            aliases: &["q"],
            help: "Exit the TUI",
            key_hint: Some("Esc"),
        };
        let text = suggestion_text(&cmd, 40);
        assert!(text.starts_with("quit"));
        assert!(text.contains("Exit the TUI"));
        assert!(text.trim_end().ends_with("Esc"));
        assert_eq!(text.chars().count(), 40);
    }

    #[test]
    fn suggestion_text_omits_shortcut_when_key_hint_is_none() {
        let cmd = PaletteCommand {
            name: "yolo",
            aliases: &[],
            help: "Toggle YOLO mode",
            key_hint: None,
        };
        let text = suggestion_text(&cmd, 40);
        assert!(text.starts_with("yolo"));
        assert!(text.contains("Toggle YOLO mode"));
        assert_eq!(text.chars().count(), 40);
    }

    #[test]
    fn suggestion_text_truncates_description_before_shortcut() {
        // Width tight enough to force description truncation while shortcut survives.
        let cmd = PaletteCommand {
            name: "quit",
            aliases: &[],
            help: "Exit the TUI immediately and discard state",
            key_hint: Some("Esc"),
        };
        let width: u16 = 25;
        let text = suggestion_text(&cmd, width);
        assert!(text.starts_with("quit"), "name preserved: {text:?}");
        assert!(text.trim_end().ends_with("Esc"), "shortcut kept: {text:?}");
        assert!(text.contains('…'), "description truncated: {text:?}");
        assert_eq!(text.chars().count(), width as usize);
    }

    #[test]
    fn suggestion_text_drops_shortcut_when_too_narrow() {
        // Width that does not fit gap+description+gap+shortcut, so shortcut is dropped
        // but the command name is still preserved.
        let cmd = PaletteCommand {
            name: "quit",
            aliases: &[],
            help: "Exit",
            key_hint: Some("Esc"),
        };
        let text = suggestion_text(&cmd, 6); // "quit" (4) + 2 padding
        assert!(text.starts_with("quit"));
        assert!(!text.contains("Esc"), "shortcut dropped: {text:?}");
        assert_eq!(text.chars().count(), 6);
    }

    #[test]
    fn suggestion_text_preserves_name_at_extreme_narrow() {
        let cmd = PaletteCommand {
            name: "show-archived",
            aliases: &[],
            help: "Toggle archived sessions",
            key_hint: None,
        };
        let text = suggestion_text(&cmd, 4);
        // Name truncates from the right, but the command identity remains visible.
        assert_eq!(text.chars().count(), 4);
        assert!(text.starts_with("show"));
    }

    #[test]
    fn palette_state_accept_ghost() {
        let mut state = PaletteState::default();
        state.open();
        state.buffer.push_str("qu");
        state.accept_ghost("quit");
        assert_eq!(state.buffer, "quit");
        assert_eq!(state.cursor, 4);
    }
}
