use super::*;

fn test_picker(input_buffer: &str, input_cursor: usize) -> SessionPicker {
    SessionPicker {
        entries: Vec::new(),
        selected: 0,
        viewport_top: 0,
        body_inner_height: 0,
        expanded: None,
        input_mode: true,
        input_buffer: input_buffer.to_string(),
        input_cursor,
        show_archived: false,
        confirm_modal: None,
        create_modes: crate::state::Modes::default(),
        palette: PaletteState::default(),
        status_line: StatusLine::new(),
    }
}

#[test]
fn test_idea_summary_truncates() {
    let long_text = "a".repeat(100);
    let summary = truncate_idea(&Some(long_text));
    assert!(summary.len() <= 83); // 80 + "..."
    assert!(summary.ends_with("..."));
}

#[test]
fn test_idea_summary_short() {
    let summary = truncate_idea(&Some("hello world".to_string()));
    assert_eq!(summary, "hello world");
}

#[test]
fn test_idea_summary_fallback() {
    let summary = truncate_idea(&None);
    assert_eq!(summary, "(no idea yet)");
}

#[test]
fn test_relative_time_seconds() {
    let now = SystemTime::now();
    let ago = now - Duration::from_secs(45);
    let formatted = format_relative_time(ago, now);
    assert_eq!(formatted, "45s ago");
}

#[test]
fn test_relative_time_minutes() {
    let now = SystemTime::now();
    let ago = now - Duration::from_secs(150);
    let formatted = format_relative_time(ago, now);
    assert_eq!(formatted, "2m ago");
}

#[test]
fn test_relative_time_hours() {
    let now = SystemTime::now();
    let ago = now - Duration::from_secs(7200);
    let formatted = format_relative_time(ago, now);
    assert_eq!(formatted, "2h ago");
}

#[test]
fn test_relative_time_days() {
    let now = SystemTime::now();
    let ago = now - Duration::from_secs(86400 * 3);
    let formatted = format_relative_time(ago, now);
    assert_eq!(formatted, "3d ago");
}

#[test]
fn test_generate_session_id() {
    let id = generate_session_id();
    assert_eq!(id.len(), 25); // YYYYMMDD-HHMMSS-NNNNNNNNN
    let parts: Vec<&str> = id.split('-').collect();
    assert_eq!(parts.len(), 3);
    assert_eq!(parts[0].len(), 8);
    assert_eq!(parts[1].len(), 6);
    assert_eq!(parts[2].len(), 9);
    assert!(parts[2].chars().all(|c| c.is_ascii_digit()));
}

fn with_temp_codexize_root<T>(f: impl FnOnce() -> T) -> T {
    let _guard = crate::state::test_fs_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::TempDir::new().unwrap();
    let prev = std::env::var_os("CODEXIZE_ROOT");
    // SAFETY: serialized via test_fs_lock; restored unconditionally.
    unsafe {
        std::env::set_var("CODEXIZE_ROOT", temp.path().join(".codexize"));
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    unsafe {
        match prev {
            Some(v) => std::env::set_var("CODEXIZE_ROOT", v),
            None => std::env::remove_var("CODEXIZE_ROOT"),
        }
    }
    outcome.unwrap()
}

#[test]
fn scan_sessions_returns_empty_when_root_is_brand_new() {
    with_temp_codexize_root(|| {
        // First call creates the sessions dir on demand and returns no entries.
        let entries = scan_sessions().unwrap();
        assert!(entries.is_empty());
        assert!(crate::state::codexize_root().join("sessions").exists());
    });
}

#[test]
fn scan_sessions_skips_directories_without_session_toml() {
    with_temp_codexize_root(|| {
        let _ = scan_sessions().unwrap();
        let stray = crate::state::codexize_root().join("sessions").join("stray");
        fs::create_dir_all(&stray).unwrap();
        // No session.toml inside; the entry must be ignored.
        let entries = scan_sessions().unwrap();
        assert!(
            entries.is_empty(),
            "stray dir without session.toml must be skipped"
        );
    });
}

#[test]
fn scan_sessions_returns_entries_sorted_by_recency() {
    with_temp_codexize_root(|| {
        // Stage two sessions; touching their session.toml gives the
        // newer one a more recent mtime so it sorts first.
        let mut older = SessionState::new("alpha".to_string());
        older.title = Some("alpha title".to_string());
        older.save().unwrap();
        // Sleep enough to ensure the next save's mtime strictly exceeds
        // alpha's (mtime resolution is filesystem-dependent).
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut newer = SessionState::new("beta".to_string());
        newer.title = Some("beta title".to_string());
        newer.save().unwrap();

        let entries = scan_sessions().unwrap();
        assert_eq!(entries.len(), 2, "both sessions must be discovered");
        assert_eq!(entries[0].session_id, "beta", "newest first");
        assert_eq!(entries[1].session_id, "alpha");
        assert_eq!(entries[0].idea_summary, "beta title");
    });
}

#[test]
fn new_session_seeds_create_modes() {
    with_temp_codexize_root(|| {
        let mut picker = SessionPicker::new_with_create_modes(crate::state::Modes {
            yolo: false,
            cheap: true,
        })
        .unwrap();
        picker.input_mode = true;
        picker.input_buffer = "ship cheap mode".to_string();
        picker.input_cursor = picker.input_buffer.chars().count();

        let action = picker
            .handle_input_key(KeyEvent::new(
                KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
        let KeyAction::SelectSession(selection) = action else {
            panic!("expected new session selection");
        };

        assert!(selection.created);
        let state = SessionState::load(&selection.session_id).expect("load new session");
        assert!(state.modes.cheap);
    });
}

#[test]
fn direct_n_key_enters_input_mode_outside_palette() {
    let mut picker = test_picker("", 0);
    picker.input_mode = false;

    assert!(matches!(
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char('n'),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap(),
        KeyAction::Continue
    ));
    assert!(picker.input_mode, "bare n should enter input mode");
    assert!(
        picker.input_buffer.is_empty(),
        "buffer should be empty on entry"
    );

    // Esc exits without creating a session
    assert!(matches!(
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap(),
        KeyAction::Continue
    ));
    assert!(!picker.input_mode, "Esc should exit input mode");
}

#[test]
fn direct_a_key_remains_palette_only_and_q_quits() {
    let mut picker = test_picker("", 0);
    picker.input_mode = false;

    picker
        .handle_key(KeyEvent::new(
            KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
    assert!(
        !picker.show_archived,
        "archive visibility action must be routed through the palette"
    );

    assert!(matches!(
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char('q'),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap(),
        KeyAction::Quit
    ));
}

#[test]
fn picker_quit_and_cancel_keys_have_escape_q_parity() {
    let mut picker = test_picker("", 0);
    picker.input_mode = false;

    assert!(matches!(
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char('q'),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap(),
        KeyAction::Quit
    ));

    picker.palette.open();
    assert!(matches!(
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char('q'),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap(),
        KeyAction::Continue
    ));
    assert!(!picker.palette.open, "q should close palette like Esc");

    picker.confirm_modal = Some(ConfirmKind::Archive);
    assert!(matches!(
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Char('q'),
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap(),
        KeyAction::Continue
    ));
    assert!(
        picker.confirm_modal.is_none(),
        "q should dismiss confirm modal like Esc"
    );
}

#[test]
fn input_mode_keeps_colon_literal() {
    let mut picker = test_picker("", 0);

    picker
        .handle_key(KeyEvent::new(
            KeyCode::Char(':'),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();

    assert_eq!(picker.input_buffer, ":");
}

#[test]
fn palette_overlay_empty_buffer_lists_commands_in_picker() {
    let mut picker = test_picker("", 0);
    picker.input_mode = false;
    picker.palette.open();

    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| picker.draw(frame)).unwrap();
    let buf = terminal.backend().buffer();
    let text = (0..24)
        .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    // Empty buffer lists every command with help text.
    assert!(text.contains("quit"), "lists quit");
    assert!(text.contains("Exit picker"));
    assert!(text.contains("new"));
    assert!(text.contains("Create a session"));
    assert!(text.contains("idea"));
    assert!(text.contains("show-archived"));

    // The picker `new` has a real direct key (`n`), so its shortcut renders.
    // `idea`/`archive`/`delete` are palette-only, no shortcut text.
    // Strip the surrounding `│` border characters before inspecting trailers.
    let strip_borders = |row: &str| -> String {
        row.trim_matches(|c: char| c == '│' || c == ' ')
            .trim_end()
            .to_string()
    };

    let new_row = text
        .lines()
        .find(|l| l.contains("Create a session"))
        .expect("new row present");
    assert!(
        strip_borders(new_row).ends_with('n'),
        "new advertises shortcut n: {new_row:?}"
    );

    // Idea has no direct key in the picker; the suggestion text must omit
    // the shortcut entirely. Inspect the rendered cell content directly to
    // avoid coupling to padding inside the bordered overlay.
    let commands = picker.palette_commands();
    let idea = commands
        .iter()
        .find(|c| c.name == "idea")
        .expect("idea command present");
    let idea_text = palette::suggestion_text(idea, 78);
    assert!(
        !idea_text.contains(" i\u{0}") && !idea_text.trim_end().ends_with('i'),
        "idea suggestion must not advertise a shortcut hint: {idea_text:?}"
    );
}

#[test]
fn palette_overlay_filters_and_resolves_alias_in_picker() {
    let mut picker = test_picker("", 0);
    picker.input_mode = false;
    picker.palette.open();
    picker.palette.buffer = "ar".to_string();
    picker.palette.cursor = 2;

    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| picker.draw(frame)).unwrap();
    let buf = terminal.backend().buffer();
    let text = (0..24)
        .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        text.contains("archive") || text.contains("show-archived"),
        "ar prefix surfaces archive-related commands: {text}"
    );
    // Tab still resolves to the ghost completion via shared palette helpers.
    let commands = picker.palette_commands();
    let ghost = palette::ghost_completion(&picker.palette.buffer, &commands);
    assert!(
        ghost.is_some(),
        "ghost autocomplete should still resolve from prefix"
    );
}

#[test]
fn palette_new_without_args_opens_input_modal() {
    let mut picker = test_picker("", 0);
    picker.input_mode = false;

    let action = picker.execute_palette_input("new").unwrap();
    assert!(matches!(action, KeyAction::Continue));
    assert!(picker.input_mode, "empty-args :new should open input modal");
    assert!(picker.input_buffer.is_empty());
}

#[test]
fn palette_new_with_args_creates_session_immediately() {
    with_temp_codexize_root(|| {
        let mut picker =
            SessionPicker::new_with_create_modes(crate::state::Modes::default()).unwrap();
        picker.input_mode = false;

        let action = picker.execute_palette_input("new ship cheap mode").unwrap();
        let KeyAction::SelectSession(selection) = action else {
            panic!("expected SelectSession action");
        };
        assert!(selection.created);

        let state = SessionState::load(&selection.session_id).expect("load new session");
        assert_eq!(state.idea_text.as_deref(), Some("ship cheap mode"));
        assert_eq!(state.current_phase, Phase::BrainstormRunning);
    });
}

#[test]
fn create_session_helper_persists_brainstorm_running_with_modes() {
    with_temp_codexize_root(|| {
        let session_id = create_session(
            "ship the dashboard",
            crate::state::Modes {
                yolo: true,
                cheap: true,
            },
        )
        .expect("create_session succeeds");

        let state = SessionState::load(&session_id).expect("load new session");
        assert_eq!(state.idea_text.as_deref(), Some("ship the dashboard"));
        assert_eq!(state.current_phase, Phase::BrainstormRunning);
        assert!(state.modes.yolo);
        assert!(state.modes.cheap);
    });
}

#[test]
fn create_session_helper_logs_session_created_and_mode_events() {
    with_temp_codexize_root(|| {
        let session_id = create_session(
            "log it",
            crate::state::Modes {
                yolo: true,
                cheap: false,
            },
        )
        .expect("create_session succeeds");

        // The events audit trail is a TOML file next to session.toml.
        // Reading the raw file keeps the test independent of any
        // structured-log accessor.
        let events_path = crate::state::session_dir(&session_id).join("events.toml");
        let log = std::fs::read_to_string(&events_path).expect("events.toml exists");
        assert!(log.contains("session created"), "log: {log}");
        assert!(log.contains("mode=yolo"), "yolo logged: {log}");
        assert!(!log.contains("mode=cheap"), "cheap not logged: {log}");
    });
}

#[test]
fn palette_idea_alias_creates_session_immediately() {
    with_temp_codexize_root(|| {
        let mut picker =
            SessionPicker::new_with_create_modes(crate::state::Modes::default()).unwrap();
        picker.input_mode = false;

        let action = picker
            .execute_palette_input("idea ship cheap mode")
            .unwrap();
        let KeyAction::SelectSession(selection) = action else {
            panic!("expected SelectSession action");
        };
        assert!(selection.created);

        let state = SessionState::load(&selection.session_id).expect("load new session");
        assert_eq!(state.idea_text.as_deref(), Some("ship cheap mode"));
        assert_eq!(state.current_phase, Phase::BrainstormRunning);
    });
}

#[test]
fn mode_badge_labels_include_cheap_marker() {
    let labels = mode_badge_labels(crate::state::Modes {
        yolo: false,
        cheap: true,
    });

    assert_eq!(labels, vec!["[CHEAP]"]);
}

#[test]
fn generate_session_id_distinguishes_rapid_calls() {
    // Two sessions kicked off back-to-back in the same wall-clock
    // second must produce distinct session-directory names — this used
    // to collide because the format only had second precision.
    let mut ids = std::collections::HashSet::new();
    for _ in 0..5 {
        ids.insert(generate_session_id());
    }
    assert_eq!(
        ids.len(),
        5,
        "five rapid session ids must be distinct, got {ids:?}"
    );
}

fn picker_with_entries(entries: Vec<SessionEntry>, selected: usize) -> SessionPicker {
    SessionPicker {
        entries,
        selected,
        viewport_top: 0,
        body_inner_height: 0,
        expanded: None,
        input_mode: false,
        input_buffer: String::new(),
        input_cursor: 0,
        show_archived: false,
        confirm_modal: None,
        create_modes: crate::state::Modes::default(),
        palette: PaletteState::default(),
        status_line: StatusLine::new(),
    }
}

fn dummy_entry(id: &str, summary: &str) -> SessionEntry {
    SessionEntry {
        session_id: id.to_string(),
        idea_summary: summary.to_string(),
        current_phase: Phase::IdeaInput,
        modes: crate::state::Modes::default(),
        last_modified: SystemTime::now(),
        archived: false,
    }
}

#[test]
fn selected_row_uses_marker_and_no_reversed_style() {
    let mut picker = picker_with_entries(
        vec![
            dummy_entry("alpha", "first idea"),
            dummy_entry("beta", "second idea"),
        ],
        1,
    );

    let backend = ratatui::backend::TestBackend::new(80, 8);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| picker.draw(frame)).unwrap();
    let buf = terminal.backend().buffer();

    // Locate the row carrying each idea and inspect its leading marker
    // and style. Row index is independent of the surrounding border.
    let mut alpha_row = None;
    let mut beta_row = None;
    for y in 0..8 {
        let row: String = (0..80).map(|x| buf[(x, y)].symbol()).collect();
        if row.contains("first idea") {
            alpha_row = Some(y);
        }
        if row.contains("second idea") {
            beta_row = Some(y);
        }
    }
    let alpha_y = alpha_row.expect("alpha row rendered");
    let beta_y = beta_row.expect("beta row rendered");

    // Borderless rows place the selection marker in the first cell.
    // Unselected rows keep that cell blank.
    let marker_cell = |y: u16| -> String { buf[(0, y)].symbol().to_string() };
    assert_eq!(marker_cell(alpha_y), " ", "unselected row stays blank");
    assert_eq!(marker_cell(beta_y), ">", "selected row shows > marker");

    // Selected row must not rely on reversed background. Scan every cell
    // on the selected row to confirm REVERSED is absent.
    for x in 0..80 {
        let style = buf[(x, beta_y)].style();
        assert!(
            !style.add_modifier.contains(Modifier::REVERSED),
            "selected row must not use Modifier::REVERSED at col {x}"
        );
    }
}

#[test]
fn selected_row_highlight_style_excludes_reversed() {
    // Even outside of rendering, the highlight style itself must be free
    // of REVERSED so any future render path inherits the same contract.
    let mut picker = picker_with_entries(vec![dummy_entry("alpha", "only idea")], 0);
    let backend = ratatui::backend::TestBackend::new(80, 8);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| picker.draw(frame)).unwrap();
    let buf = terminal.backend().buffer();
    for y in 0..8 {
        for x in 0..80 {
            assert!(
                !buf[(x, y)]
                    .style()
                    .add_modifier
                    .contains(Modifier::REVERSED),
                "no cell may render with REVERSED at ({x},{y})"
            );
        }
    }
}

#[test]
fn input_mode_renders_single_divider_above_input_sheet() {
    let mut picker = picker_with_entries(vec![dummy_entry("alpha", "only idea")], 0);
    picker.input_mode = true;

    let backend = ratatui::backend::TestBackend::new(80, 8);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| picker.draw(frame)).unwrap();
    let buf = terminal.backend().buffer();

    let divider_rows = (0..8)
        .filter(|&y| {
            let row: String = (0..80).map(|x| buf[(x, y)].symbol()).collect();
            row.chars().all(|c| c == '─')
        })
        .count();

    assert_eq!(
        divider_rows, 1,
        "exactly one divider row should render above the input sheet"
    );
}

#[test]
fn test_phase_badge_variants() {
    let (badge, color, prefix) = phase_badge(Phase::Done);
    assert_eq!(badge, "done");
    assert_eq!(color, Color::Green);
    assert_eq!(prefix, "✓");

    let (badge, _, prefix) = phase_badge(Phase::BlockedNeedsUser);
    assert_eq!(badge, "blocked");
    assert_eq!(prefix, "○");

    let (badge, _, _) = phase_badge(Phase::ImplementationRound(3));
    assert_eq!(badge, "coding r3");
}

#[test]
fn space_toggles_expansion_on_selected_session() {
    let mut picker = picker_with_entries(
        vec![
            dummy_entry("alpha", "first idea"),
            dummy_entry("beta", "second idea"),
        ],
        0,
    );

    assert_eq!(picker.expanded, None);

    picker
        .handle_key(KeyEvent::new(
            KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
    assert_eq!(picker.expanded, Some("alpha".to_string()));

    picker
        .handle_key(KeyEvent::new(
            KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
    assert_eq!(picker.expanded, None);
}

#[test]
fn navigation_collapses_expanded_session() {
    let mut picker = picker_with_entries(
        vec![
            dummy_entry("alpha", "first idea"),
            dummy_entry("beta", "second idea"),
            dummy_entry("gamma", "third idea"),
        ],
        1,
    );

    // Expand beta
    picker
        .handle_key(KeyEvent::new(
            KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
    assert_eq!(picker.expanded, Some("beta".to_string()));

    // Move up collapses
    picker
        .handle_key(KeyEvent::new(
            KeyCode::Up,
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
    assert_eq!(picker.selected, 0);
    assert_eq!(picker.expanded, None);

    // Expand alpha
    picker
        .handle_key(KeyEvent::new(
            KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
    assert_eq!(picker.expanded, Some("alpha".to_string()));

    // Move down collapses
    picker
        .handle_key(KeyEvent::new(
            KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
    assert_eq!(picker.selected, 1);
    assert_eq!(picker.expanded, None);
}

#[test]
fn pgup_pgdn_use_visible_body_step_and_collapse_expansion() {
    let mut picker = picker_with_entries(
        (0..20)
            .map(|i| dummy_entry(&format!("sess-{i}"), &format!("idea {i}")))
            .collect(),
        8,
    );

    // term_h = 10 => body_h = 7, so PageUp/PageDown should move by 6
    // sessions rather than a fixed constant.
    let backend = ratatui::backend::TestBackend::new(80, 10);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| picker.draw(frame)).unwrap();

    // Expand sess-8.
    picker
        .handle_key(KeyEvent::new(
            KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
    assert_eq!(picker.expanded, Some("sess-8".to_string()));

    // PageUp collapses and moves by body_h - 1 sessions.
    picker
        .handle_key(KeyEvent::new(
            KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
    assert_eq!(picker.expanded, None);
    assert_eq!(picker.selected, 2);

    // Re-expand and PageDown
    picker.selected = 8;
    picker
        .handle_key(KeyEvent::new(
            KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
    assert_eq!(picker.expanded, Some("sess-8".to_string()));

    picker
        .handle_key(KeyEvent::new(
            KeyCode::PageDown,
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();
    assert_eq!(picker.expanded, None);
    assert_eq!(picker.selected, 14);
}

#[test]
fn expanded_details_force_viewport_to_scroll_when_off_screen() {
    let entries: Vec<SessionEntry> = (0..8)
        .map(|i| dummy_entry(&format!("sess-{i}"), &format!("idea {i}")))
        .collect();
    let mut picker = picker_with_entries(entries, 0);

    // term_h = 10 => body_h ≈ 7 (10 - 1 - 1 - 1 footer), which fits 7 rows.
    let backend = ratatui::backend::TestBackend::new(80, 10);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();

    terminal.draw(|frame| picker.draw(frame)).unwrap();
    assert_eq!(picker.viewport_top, 0);

    // Expand sess-0 (adds 4 detail rows).
    picker
        .handle_key(KeyEvent::new(
            KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();

    // Now select sess-7. With sess-0 expanded we have 8 header + 4 detail = 12 rows.
    // body_h = 10 - 1 - 1 - 1 = 7 (no status line).
    // selected_top_idx for sess-7 = 7, selected_bottom_idx = 7.
    // 7 >= 0 + 7, so viewport_top should become 7 + 1 - 7 = 1.
    for _ in 0..7 {
        picker
            .handle_key(KeyEvent::new(
                KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ))
            .unwrap();
    }
    assert_eq!(picker.selected, 7);

    terminal.draw(|frame| picker.draw(frame)).unwrap();
    assert_eq!(
        picker.viewport_top, 1,
        "viewport_top should account for the expanded detail rows exactly"
    );
}

#[test]
fn expanded_session_renders_detail_lines() {
    let entries = vec![dummy_entry("alpha", "only idea")];
    let mut picker = picker_with_entries(entries, 0);

    let backend = ratatui::backend::TestBackend::new(80, 12);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();

    // Expand
    picker
        .handle_key(KeyEvent::new(
            KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE,
        ))
        .unwrap();

    terminal.draw(|frame| picker.draw(frame)).unwrap();
    let buf = terminal.backend().buffer();
    let text = (0..12)
        .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join(
            "
",
        );

    assert!(
        text.contains("Phase:"),
        "expanded session must show phase: {text}"
    );
    assert!(
        text.contains("Idea:"),
        "expanded session must show idea: {text}"
    );
    assert!(
        text.contains("Last agent:"),
        "expanded session must show last agent: {text}"
    );
    assert!(
        text.contains("Modified:"),
        "expanded session must show modified date: {text}"
    );
}

#[test]
fn degenerate_terminal_omits_expansion() {
    let entries = vec![dummy_entry("alpha", "only idea")];
    let mut picker = picker_with_entries(entries, 0);
    picker.expanded = Some("alpha".to_string());

    // term_h < 10 triggers degenerate mode
    let backend = ratatui::backend::TestBackend::new(80, 8);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();

    terminal.draw(|frame| picker.draw(frame)).unwrap();
    let buf = terminal.backend().buffer();
    let text = (0..8)
        .map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join(
            "
",
        );

    assert!(
        !text.contains("Phase:"),
        "degenerate terminal must omit detail expansion: {text}"
    );
}
