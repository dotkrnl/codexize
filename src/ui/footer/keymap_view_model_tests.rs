use super::*;
use ratatui::style::Color;

fn binding(
    glyph: &'static str,
    action: &'static str,
    is_primary: bool,
    capability: Option<Capability>,
) -> KeyBinding {
    KeyBinding {
        glyph,
        action,
        is_primary,
        capability,
    }
}

#[test]
fn measure_full_width_counts_nav_and_actions() {
    let nav = [binding("↑↓", "move", false, None)];
    let actions = [binding("Enter", "open", true, Some(Capability::Input))];
    assert!(measure_full_width(&nav, &actions, true, &|_| true) > 0);
}

#[test]
fn binding_enabled_uses_capability_callback() {
    let expand_binding = binding("Space", "expand", false, Some(Capability::Expand));

    assert!(!binding_enabled(&expand_binding, &|_| false));
    assert!(binding_enabled(
        &binding("Esc", "quit", false, None),
        &|_| false
    ));
}

#[test]
fn binding_width_counts_enabled_label_only() {
    let binding = binding("Space", "expand", false, Some(Capability::Expand));

    assert_eq!(
        binding_width(&binding, true, &|_| true),
        "Space expand".len()
    );
    assert_eq!(binding_width(&binding, true, &|_| false), "Space".len());
}

#[test]
fn category_width_omits_disabled_labels() {
    let bindings = [binding("Space", "expand", false, Some(Capability::Expand))];

    assert_eq!(category_width(&bindings, true, &|_| false), "Space".len());
}

#[test]
fn measure_system_assumes_system_bindings_enabled() {
    let system = [binding("Esc", "quit", false, None)];

    assert_eq!(measure_system(&system, true), "Esc quit".len());
}

#[test]
fn measure_simple_bindings_drops_labels_when_requested() {
    let bindings = [binding("Esc", "quit", false, None)];
    assert!(measure_simple_bindings(&bindings, true) > measure_simple_bindings(&bindings, false));
}

#[test]
fn select_width_tier_collapses_when_width_tightens() {
    let nav = [binding("↑↓", "move", false, None)];
    let actions = [binding("Enter", "open", true, Some(Capability::Input))];
    let system = [binding("Esc", "quit", false, None)];
    assert_eq!(
        select_width_tier(&nav, &actions, &system, &|_| true, 80),
        WidthTier::Full
    );
    assert_eq!(
        select_width_tier(&nav, &actions, &system, &|_| true, 4),
        WidthTier::FirstKeyOnly
    );
}

#[test]
fn select_simple_tier_uses_label_free_middle_state() {
    let bindings = [binding("Esc", "quit", false, None)];
    assert_eq!(select_simple_tier(&bindings, 5), WidthTier::DropNavLabels);
}

#[test]
fn render_binding_dims_disabled_glyphs() {
    let spans = render_binding(
        &binding("Space", "expand", false, Some(Capability::Expand)),
        true,
        false,
    );
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].style.fg, Some(DISABLED_DIM));
}

#[test]
fn select_modal_tier_drops_to_first_key_only_for_tight_widths() {
    let actions = [binding("Enter", "confirm", true, None)];
    let system = [binding("Esc", "cancel", false, None)];
    assert_eq!(
        select_modal_tier(&actions, &system, 4),
        WidthTier::FirstKeyOnly
    );
}

#[test]
fn render_binding_keeps_primary_color_for_enabled_bindings() {
    let spans = render_binding(&binding("Enter", "confirm", true, None), true, true);
    assert_eq!(spans[0].style.fg, Some(ENABLED_GLYPH_PRIMARY));
    assert_eq!(spans[1].style.fg, Some(ENABLED_ACTION));
    assert_ne!(spans[0].style.fg, Some(Color::DarkGray));
}
