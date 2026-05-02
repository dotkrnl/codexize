use super::*;
use crate::app::footer::keymap_view_model;

fn line_text(line: &Line) -> String {
    line.spans
        .iter()
        .map(|s| s.content.to_string())
        .collect::<String>()
}

fn has_dim_spans(line: &Line) -> bool {
    line.spans.iter().any(|s| s.style.fg == Some(DISABLED_DIM))
}

#[test]
fn default_phase_exact_string_wide() {
    let caps = FocusCaps {
        can_expand: true,
        can_edit: true,
        can_back: true,
        can_input: true,
        can_split: true,
    };
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 200);
    let text = line_text(&line);
    assert!(text.contains("↑↓ move"));
    assert!(text.contains("Space expand"));
    assert!(text.contains("PgUp/PgDn page"));
    assert!(text.contains("Enter show"));
    assert!(text.contains(": palette"));
    assert!(text.contains("Esc quit"));
}

// Pause modal exact strings
#[test]
fn spec_review_paused_exact_string() {
    let line = keymap(
        Phase::SpecReviewPaused,
        Some(ModalKind::SpecReviewPaused),
        FocusCaps::default(),
        false,
        false,
        200,
    );
    let text = line_text(&line);
    assert!(text.contains("Enter continue"));
    assert!(text.contains("n new reviewer"));
    assert!(text.contains("Esc quit"));
}

#[test]
fn plan_review_paused_exact_string() {
    let line = keymap(
        Phase::PlanReviewPaused,
        Some(ModalKind::PlanReviewPaused),
        FocusCaps::default(),
        false,
        false,
        200,
    );
    let text = line_text(&line);
    assert!(text.contains("Enter continue"));
    assert!(text.contains("n new reviewer"));
    assert!(text.contains("Esc quit"));
}

// Stage error modal
#[test]
fn stage_error_brainstorm_has_edit_idea() {
    let line = keymap(
        Phase::BrainstormRunning,
        Some(ModalKind::StageError(StageId::Brainstorm)),
        FocusCaps::default(),
        false,
        false,
        200,
    );
    let text = line_text(&line);
    assert!(text.contains("r retry"));
    assert!(text.contains("e edit idea"));
    assert!(text.contains("Esc quit"));
}

#[test]
fn stage_error_non_brainstorm_no_edit() {
    let line = keymap(
        Phase::SpecReviewRunning,
        Some(ModalKind::StageError(StageId::SpecReview)),
        FocusCaps::default(),
        false,
        false,
        200,
    );
    let text = line_text(&line);
    assert!(text.contains("r retry"));
    assert!(!text.contains("edit idea"));
    assert!(text.contains("Esc quit"));
}

// Skip-to-impl modal
#[test]
fn skip_to_impl_exact_string() {
    let line = keymap(
        Phase::SkipToImplPending,
        Some(ModalKind::SkipToImpl),
        FocusCaps::default(),
        false,
        false,
        200,
    );
    let text = line_text(&line);
    assert!(text.contains("y accept"));
    assert!(text.contains("n decline"));
    assert!(text.contains("Esc quit"));
}

// Guard modal
#[test]
fn guard_exact_string() {
    let line = keymap(
        Phase::GitGuardPending,
        Some(ModalKind::GitGuard),
        FocusCaps::default(),
        false,
        false,
        200,
    );
    let text = line_text(&line);
    assert!(text.contains("r reset"));
    assert!(text.contains("k keep"));
    assert!(text.contains("Esc quit"));
}

#[test]
fn quit_running_agent_modal_exact_string() {
    let line = keymap(
        Phase::BrainstormRunning,
        Some(ModalKind::QuitRunningAgent),
        FocusCaps::default(),
        false,
        false,
        200,
    );
    let text = line_text(&line);
    assert!(text.contains("Enter confirm"));
    assert!(text.contains("y confirm"));
    assert!(text.contains("Esc cancel"));
    assert!(text.contains("n cancel"));
}

// Input mode
#[test]
fn input_mode_exact_string() {
    let line = keymap(
        Phase::IdeaInput,
        None,
        FocusCaps::default(),
        true,
        false,
        200,
    );
    let text = line_text(&line);
    assert!(text.contains("Esc cancel"));
    assert!(text.contains("Enter submit"));
}

// Disabled bindings render glyph-only and never advertise the action label,
// and the omitted label must not count toward width-tier selection.
#[test]
fn disabled_binding_omits_action_label() {
    let caps = FocusCaps {
        can_expand: false,
        can_edit: true,
        can_back: true,
        can_input: true,
        can_split: true,
    };
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 200);
    let text = line_text(&line);
    assert!(
        text.contains("Space"),
        "Space glyph should still appear when disabled"
    );
    assert!(
        !text.contains("Space expand"),
        "disabled Space must not advertise its `expand` label, got: {text}"
    );
    // Other (enabled) labels still render.
    assert!(text.contains("↑↓ move"));
    assert!(text.contains("Enter show"));
}

#[test]
fn disabled_binding_uses_single_dim_color() {
    let caps = FocusCaps {
        can_expand: false,
        can_edit: false,
        can_back: false,
        can_input: false,
        can_split: false,
    };
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 200);
    let dim_fgs: std::collections::BTreeSet<_> = line
        .spans
        .iter()
        .filter_map(|s| match s.style.fg {
            Some(c) if c == DISABLED_DIM => Some(format!("{:?}", c)),
            _ => None,
        })
        .collect();
    assert_eq!(
        dim_fgs.len(),
        1,
        "all disabled spans should share one dim color, got {dim_fgs:?}"
    );
}

#[test]
fn disabled_label_excluded_from_width_tier() {
    // At a width that only fits the line when Space's `expand` label is dropped,
    // the disabled-Space form must keep all enabled labels visible.
    let caps_disabled = FocusCaps {
        can_expand: false,
        can_edit: true,
        can_back: true,
        can_input: true,
        can_split: true,
    };
    let caps_enabled = FocusCaps {
        can_expand: true,
        can_edit: true,
        can_back: true,
        can_input: true,
        can_split: true,
    };
    let width = {
        let (nav, actions, system) = default_bindings();
        // Width that fits the disabled rendering exactly.
        let left = keymap_view_model::measure_full_width(&nav, &actions, true, &|cap: Option<
            Capability,
        >| {
            cap.map(|c| match c {
                Capability::Expand => caps_disabled.can_expand,
                Capability::Input => caps_disabled.can_input,
                Capability::Split => caps_disabled.can_split,
            })
            .unwrap_or(true)
        });
        let sys = keymap_view_model::measure_system(&system, true);
        (left + SEP_CATEGORY.chars().count() + sys) as u16
    };

    let line_dis = keymap(Phase::IdeaInput, None, caps_disabled, false, false, width);
    let text_dis = line_text(&line_dis);
    assert!(
        text_dis.contains("↑↓ move"),
        "disabled-form should still advertise enabled labels at this width: {text_dis}"
    );
    assert!(
        text_dis.contains("Enter show"),
        "disabled-form should keep `Enter show` at this width: {text_dis}"
    );
    assert!(
        !text_dis.contains("Space expand"),
        "Space label must remain hidden when disabled: {text_dis}"
    );

    // Sanity: with the same caps but the Space binding enabled, that exact
    // width is too narrow for `Space expand` to fit at the Full tier — so
    // tier selection must have observed the disabled label as 0-width.
    let line_en = keymap(Phase::IdeaInput, None, caps_enabled, false, false, width);
    let text_en = line_text(&line_en);
    // The enabled rendering at the same width may collapse to a narrower tier;
    // either way, its width must not exceed the budget.
    assert!(
        text_en.chars().count() as u16 <= width,
        "enabled rendering exceeded width budget: width={width} got={}",
        text_en.chars().count()
    );
}

// Dim-in-place tests
#[test]
fn dim_in_place_expand_disabled() {
    let caps = FocusCaps {
        can_expand: false,
        can_edit: true,
        can_back: true,
        can_input: true,
        can_split: true,
    };
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 200);
    assert!(
        has_dim_spans(&line),
        "should have dim spans for disabled expand"
    );
    let text = line_text(&line);
    assert!(text.contains("Space"), "Space should still appear");
}

#[test]
fn dim_in_place_expand_disabled_palette_present() {
    let caps = FocusCaps {
        can_expand: false,
        can_edit: true,
        can_back: true,
        can_input: true,
        can_split: true,
    };
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 200);
    assert!(
        has_dim_spans(&line),
        "should have dim spans for disabled expand"
    );
    let text = line_text(&line);
    assert!(text.contains(":"), "palette hint should appear");
}

#[test]
fn dim_in_place_all_disabled_palette_still_shows() {
    let caps = FocusCaps {
        can_expand: false,
        can_edit: false,
        can_back: false,
        can_input: false,
        can_split: false,
    };
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 200);
    let text = line_text(&line);
    assert!(text.contains("Space"), "Space should not dropout");
    assert!(text.contains(":"), "palette hint should not dropout");
}

#[test]
fn dim_in_place_input_disabled() {
    let caps = FocusCaps {
        can_expand: true,
        can_edit: true,
        can_back: true,
        can_input: false,
        can_split: false,
    };
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 200);
    assert!(
        has_dim_spans(&line),
        "should have dim spans for disabled input"
    );
    let text = line_text(&line);
    assert!(
        text.contains("Enter"),
        "Enter should still appear when input is disabled"
    );
}

// Right-anchor stability
#[test]
fn esc_quit_right_anchored_stable_across_phases() {
    let caps = FocusCaps::default();
    let width = 120u16;

    let line1 = keymap(Phase::IdeaInput, None, caps, false, false, width);
    let line2 = keymap(Phase::BrainstormRunning, None, caps, false, false, width);

    let text1 = line_text(&line1);
    let text2 = line_text(&line2);

    assert!(text1.ends_with("Esc quit"));
    assert!(text2.ends_with("Esc quit"));

    let len1 = text1.chars().count();
    let len2 = text2.chars().count();
    assert_eq!(len1, len2, "line lengths should be stable");
}

// Width tier collapse tests
#[test]
fn width_tier_drops_system_label_first() {
    let caps = FocusCaps::default();
    let line_wide = keymap(Phase::IdeaInput, None, caps, false, false, 200);
    let text_wide = line_text(&line_wide);
    assert!(text_wide.contains("Esc quit"), "wide should have 'q quit'");

    let line_narrow = keymap(Phase::IdeaInput, None, caps, false, false, 80);
    let text_narrow = line_text(&line_narrow);
    let ends_with_esc = text_narrow.trim_end().ends_with("Esc");
    assert!(
        text_narrow.contains("Esc quit") || ends_with_esc,
        "narrow should have 'Esc' or 'Esc quit'"
    );
}

#[test]
fn width_tier_ultra_narrow_still_renders() {
    let caps = FocusCaps::default();
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 20);
    let text = line_text(&line);
    assert!(
        !text.is_empty(),
        "should render something even ultra-narrow"
    );
}

#[test]
fn zero_width_empty() {
    let line = keymap(
        Phase::IdeaInput,
        None,
        FocusCaps::default(),
        false,
        false,
        0,
    );
    assert!(line.spans.is_empty());
}

// Snapshot tests at different widths
#[test]
fn snapshot_width_200_default() {
    let caps = FocusCaps {
        can_expand: true,
        can_edit: true,
        can_back: true,
        can_input: true,
        can_split: true,
    };
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 200);
    let text = line_text(&line);
    assert!(text.contains("↑↓ move · Space expand · PgUp/PgDn page"));
    assert!(text.contains("Enter show · : palette"));
    assert!(text.ends_with("Esc quit"));
}

#[test]
fn snapshot_width_120_default() {
    let caps = FocusCaps {
        can_expand: true,
        can_edit: true,
        can_back: true,
        can_input: true,
        can_split: true,
    };
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 120);
    let text = line_text(&line);
    assert!(text.ends_with("Esc quit") || text.ends_with("Esc"));
}

#[test]
fn snapshot_width_80_default() {
    let caps = FocusCaps::default();
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 80);
    let text = line_text(&line);
    assert!(text.contains("Esc"), "should contain Esc");
}

#[test]
fn snapshot_width_60_default() {
    let caps = FocusCaps::default();
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 60);
    let text = line_text(&line);
    assert!(text.contains("↑↓") || text.contains("Enter"));
}

#[test]
fn snapshot_width_40_default() {
    let caps = FocusCaps::default();
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 40);
    let text = line_text(&line);
    assert!(!text.is_empty());
}

#[test]
fn snapshot_width_30_default() {
    let caps = FocusCaps::default();
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 30);
    let text = line_text(&line);
    assert!(!text.is_empty());
}

// Modal snapshots
#[test]
fn snapshot_pause_modal_width_80() {
    let line = keymap(
        Phase::SpecReviewPaused,
        Some(ModalKind::SpecReviewPaused),
        FocusCaps::default(),
        false,
        false,
        80,
    );
    let text = line_text(&line);
    assert!(text.contains("Enter"));
    assert!(text.contains("n"));
    assert!(text.contains("q"));
}

#[test]
fn snapshot_guard_modal_width_80() {
    let line = keymap(
        Phase::GitGuardPending,
        Some(ModalKind::GitGuard),
        FocusCaps::default(),
        false,
        false,
        80,
    );
    let text = line_text(&line);
    assert!(text.contains("r"));
    assert!(text.contains("k"));
    assert!(text.contains("q"));
}

#[test]
fn snapshot_input_mode_width_80() {
    let line = keymap(
        Phase::IdeaInput,
        None,
        FocusCaps::default(),
        true,
        false,
        80,
    );
    let text = line_text(&line);
    assert!(text.contains("Esc"));
    assert!(text.contains("Enter"));
}

// Right-anchor stability across default/modal transitions
#[test]
fn esc_quit_right_anchored_stable_default_vs_pause_modal() {
    let width = 120u16;
    let default = keymap(
        Phase::IdeaInput,
        None,
        FocusCaps::default(),
        false,
        false,
        width,
    );
    let modal = keymap(
        Phase::SpecReviewPaused,
        Some(ModalKind::SpecReviewPaused),
        FocusCaps::default(),
        false,
        false,
        width,
    );
    let default_text = line_text(&default);
    let modal_text = line_text(&modal);

    assert!(default_text.ends_with("Esc quit"));
    assert!(modal_text.ends_with("Esc quit"));
    assert_eq!(
        default_text.chars().count(),
        modal_text.chars().count(),
        "line lengths must be equal for stable right-anchor"
    );
}

#[test]
fn esc_quit_right_anchored_stable_default_vs_guard_modal() {
    let width = 120u16;
    let default = keymap(
        Phase::IdeaInput,
        None,
        FocusCaps::default(),
        false,
        false,
        width,
    );
    let modal = keymap(
        Phase::GitGuardPending,
        Some(ModalKind::GitGuard),
        FocusCaps::default(),
        false,
        false,
        width,
    );
    let default_text = line_text(&default);
    let modal_text = line_text(&modal);

    assert!(default_text.ends_with("Esc quit"));
    assert!(modal_text.ends_with("Esc quit"));
    assert_eq!(
        default_text.chars().count(),
        modal_text.chars().count(),
        "line lengths must be equal for stable right-anchor"
    );
}

#[test]
fn esc_quit_right_anchored_stable_default_vs_skip_to_impl() {
    let width = 120u16;
    let default = keymap(
        Phase::IdeaInput,
        None,
        FocusCaps::default(),
        false,
        false,
        width,
    );
    let modal = keymap(
        Phase::SkipToImplPending,
        Some(ModalKind::SkipToImpl),
        FocusCaps::default(),
        false,
        false,
        width,
    );
    let default_text = line_text(&default);
    let modal_text = line_text(&modal);

    assert!(default_text.ends_with("Esc quit"));
    assert!(modal_text.ends_with("Esc quit"));
    assert_eq!(
        default_text.chars().count(),
        modal_text.chars().count(),
        "line lengths must be equal for stable right-anchor"
    );
}

#[test]
fn esc_quit_right_anchored_stable_default_vs_stage_error() {
    let width = 120u16;
    let default = keymap(
        Phase::IdeaInput,
        None,
        FocusCaps::default(),
        false,
        false,
        width,
    );
    let modal = keymap(
        Phase::BrainstormRunning,
        Some(ModalKind::StageError(StageId::Brainstorm)),
        FocusCaps::default(),
        false,
        false,
        width,
    );
    let default_text = line_text(&default);
    let modal_text = line_text(&modal);

    assert!(default_text.ends_with("Esc quit"));
    assert!(modal_text.ends_with("Esc quit"));
    assert_eq!(
        default_text.chars().count(),
        modal_text.chars().count(),
        "line lengths must be equal for stable right-anchor"
    );
}

#[test]
fn modal_esc_quit_right_anchor_with_fill() {
    let width = 200u16;
    let modal = keymap(
        Phase::SpecReviewPaused,
        Some(ModalKind::SpecReviewPaused),
        FocusCaps::default(),
        false,
        false,
        width,
    );
    let text = line_text(&modal);
    assert!(text.ends_with("Esc quit"));
    assert!(
        !text.contains('─'),
        "wide modal should use spaces between actions and system"
    );
}

// Verify dim styling is correct
#[test]
fn verify_dim_styling_colors() {
    let caps = FocusCaps {
        can_expand: false,
        can_edit: false,
        can_back: false,
        can_input: false,
        can_split: false,
    };
    let line = keymap(Phase::IdeaInput, None, caps, false, false, 200);

    let has_disabled_style = line.spans.iter().any(|s| s.style.fg == Some(DISABLED_DIM));
    assert!(has_disabled_style, "should have disabled styling");

    let has_enabled_style = line.spans.iter().any(|s| {
        s.style.fg == Some(ENABLED_GLYPH)
            || s.style.fg == Some(ENABLED_GLYPH_PRIMARY)
            || s.style.fg == Some(ENABLED_ACTION)
    });
    assert!(has_enabled_style, "should have enabled styling too");
}

#[test]
fn split_open_exact_string() {
    let caps = FocusCaps::default();
    let line = keymap(Phase::IdeaInput, None, caps, false, true, 200);
    let text = line_text(&line);
    assert!(text.contains("↑↓ scroll"));
    assert!(text.contains("PgUp/PgDn page"));
    assert!(text.contains(": palette"));
    assert!(text.contains("Esc close"));
    assert!(!text.contains("move"));
    assert!(!text.contains("quit"));
}

#[test]
fn split_owned_input_advertises_submit_and_close() {
    let caps = FocusCaps::default();
    let line = keymap(Phase::IdeaInput, None, caps, true, true, 200);
    let text = line_text(&line);

    assert!(text.contains("Enter submit"));
    assert!(text.contains("Esc close"));
    assert!(!text.contains("Esc cancel"));
    assert!(!text.contains("Esc quit"));
}
