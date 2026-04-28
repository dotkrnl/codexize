pub struct PaletteCommand {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub help: &'static str,
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
        return MatchResult::Exact {
            command: cmd,
            args,
        };
    }

    // 2. Exact alias match
    if let Some(cmd) = commands
        .iter()
        .find(|c| c.aliases.iter().any(|a| *a == cmd_part))
    {
        return MatchResult::Exact {
            command: cmd,
            args,
        };
    }

    // 3. Collect prefix matches on names and aliases
    let prefix_matches: Vec<&PaletteCommand> = commands
        .iter()
        .filter(|c| {
            c.name.starts_with(cmd_part)
                || c.aliases.iter().any(|a| a.starts_with(cmd_part))
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
        .any(|c| c.name == cmd_part || c.aliases.iter().any(|a| *a == cmd_part))
    {
        return None;
    }

    let prefix_matches: Vec<&PaletteCommand> = commands
        .iter()
        .filter(|c| {
            c.name.starts_with(cmd_part)
                || c.aliases.iter().any(|a| a.starts_with(cmd_part))
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
            },
            PaletteCommand {
                name: "back",
                aliases: &["b"],
                help: "Go back",
            },
            PaletteCommand {
                name: "edit",
                aliases: &["e"],
                help: "Edit artifact",
            },
            PaletteCommand {
                name: "cheap",
                aliases: &[],
                help: "Toggle cheap mode",
            },
            PaletteCommand {
                name: "yolo",
                aliases: &[],
                help: "Toggle YOLO mode",
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
            },
            PaletteCommand {
                name: "food",
                aliases: &[],
                help: "",
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
            },
            PaletteCommand {
                name: "food",
                aliases: &[],
                help: "",
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
    fn palette_state_accept_ghost() {
        let mut state = PaletteState::default();
        state.open();
        state.buffer.push_str("qu");
        state.accept_ghost("quit");
        assert_eq!(state.buffer, "quit");
        assert_eq!(state.cursor, 4);
    }
}
