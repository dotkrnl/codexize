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
    assert!(matches!(result, MatchResult::UniquePrefix { command, .. } if command.name == "quit"));
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
fn palette_state_open_with_buffer_preserves_text_and_cursor() {
    let mut state = PaletteState::default();
    state.open_with_buffer("cheap".to_string());
    assert!(state.open);
    assert_eq!(state.buffer, "cheap");
    assert_eq!(state.cursor, 5);
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
