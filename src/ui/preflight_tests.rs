use super::*;
use crate::state::test_fs_lock;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect, style::Modifier};

#[test]
fn preflight_exit_keys_request_normal_shutdown() {
    assert_eq!(
        classify_required_modal_key(KeyCode::Char('q')),
        ModalAction::Exit
    );
    assert_eq!(classify_required_modal_key(KeyCode::Esc), ModalAction::Exit);
    assert_eq!(
        classify_optional_modal_key(KeyCode::Char('q')),
        ModalAction::Skip
    );
    assert_eq!(classify_optional_modal_key(KeyCode::Esc), ModalAction::Skip);
}

fn render_preflight_buf(scenario: Scenario, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render_preflight_modal(frame, scenario))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn raw_line_text(buf: &Buffer, y: u16, width: u16) -> String {
    (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>()
}

fn expected_dialog_rect(width: u16, height: u16, content_h: usize) -> Rect {
    let max_w = width.saturating_sub(4).max(1);
    let dialog_w = max_w.min(80).max(max_w.min(40));
    let dialog_h = ((content_h + 5) as u16).min(height.saturating_sub(4));
    Rect::new(
        (width.saturating_sub(dialog_w)) / 2,
        (height.saturating_sub(dialog_h)) / 2,
        dialog_w,
        dialog_h,
    )
}

fn scenario_body_line_count(scenario: Scenario, width: u16, height: u16) -> usize {
    let area = Rect::new(0, 0, width, height);
    let (_, body_copy, _) = preflight_modal_content(scenario);
    preflight_body_lines(area, body_copy).len()
}

#[test]
fn preflight_modals_use_shared_visual_contract_for_each_scenario() {
    let _guard = test_fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    for scenario in [
        Scenario::NoGitEmpty,
        Scenario::NoGitHasFiles,
        Scenario::GitExistsNotIgnored,
        Scenario::CodexAcpMissing,
        Scenario::ClaudeAcpMissing,
    ] {
        let width = 100;
        let height = 30;
        let buf = render_preflight_buf(scenario, width, height);
        let dialog = expected_dialog_rect(
            width,
            height,
            scenario_body_line_count(scenario, width, height),
        );

        let corner = &buf[(dialog.x, dialog.y)];
        assert_eq!(corner.symbol(), "┌");
        assert_eq!(corner.fg, Color::Yellow);
        assert!(corner.modifier.contains(Modifier::BOLD));

        for y in dialog.y..dialog.y + dialog.height {
            for x in dialog.x..dialog.x + dialog.width {
                assert_eq!(buf[(x, y)].bg, Color::Black);
            }
        }

        assert!(
            raw_line_text(&buf, dialog.y + dialog.height - 3, width)
                .trim()
                .trim_matches('│')
                .trim()
                .is_empty(),
            "expected the reserved blank separator row above the preflight keymap"
        );

        let keymap_row = raw_line_text(&buf, dialog.y + dialog.height - 2, width);
        assert!(
            !keymap_row.trim().trim_matches('│').trim().is_empty(),
            "expected a visible keymap row for {scenario:?}"
        );

        for y in 0..height {
            for x in 0..width {
                if (dialog.x..dialog.x + dialog.width).contains(&x)
                    && (dialog.y..dialog.y + dialog.height).contains(&y)
                {
                    continue;
                }
                assert_ne!(
                    buf[(x, y)].bg,
                    Color::DarkGray,
                    "preflight should not draw the dashboard dim backdrop"
                );
            }
        }
    }
}

#[test]
fn preflight_modal_action_markers_keep_allowed_semantic_colors() {
    let _guard = test_fs_lock().lock().unwrap_or_else(|e| e.into_inner());
    let width = 100;
    let height = 30;
    let buf = render_preflight_buf(Scenario::GitExistsNotIgnored, width, height);
    let dialog = expected_dialog_rect(
        width,
        height,
        scenario_body_line_count(Scenario::GitExistsNotIgnored, width, height),
    );
    let keymap_y = dialog.y + dialog.height - 2;
    let keymap_text = raw_line_text(&buf, keymap_y, width);

    let y_col = keymap_text.find("[Y]").expect("affirmative marker");
    let y_x = keymap_text[..y_col].chars().count() as u16 + 1;
    assert_eq!(buf[(y_x, keymap_y)].fg, Color::Green);

    let n_col = keymap_text.find("[N]").expect("secondary marker");
    let n_x = keymap_text[..n_col].chars().count() as u16 + 1;
    assert!(
        matches!(buf[(n_x, keymap_y)].fg, Color::White | Color::Gray),
        "secondary marker should stay in shared body colors"
    );

    let q_col = keymap_text.find("[Q]").expect("quit marker");
    let q_x = keymap_text[..q_col].chars().count() as u16 + 1;
    assert_eq!(buf[(q_x, keymap_y)].fg, Color::Red);
}
