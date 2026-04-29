use ratatui::{style::Style, text::Span};

use super::keymap::{
    Capability, DISABLED_DIM, ENABLED_ACTION, ENABLED_GLYPH, ENABLED_GLYPH_PRIMARY, KeyBinding,
    SEP_CATEGORY, category_width, measure_system,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WidthTier {
    Full,
    DropSystemLabel,
    DropActionsLabels,
    DropNavLabels,
    FirstKeyOnly,
}

pub(super) fn measure_full_width(
    nav: &[KeyBinding],
    actions: &[KeyBinding],
    show_labels: bool,
    caps: &dyn Fn(Option<Capability>) -> bool,
) -> usize {
    let mut len = category_width(nav, show_labels, caps);
    if !nav.is_empty() && !actions.is_empty() {
        len += SEP_CATEGORY.chars().count();
    }
    len += category_width(actions, show_labels, caps);
    len
}

pub(super) fn measure_simple_bindings(bindings: &[KeyBinding], show_labels: bool) -> usize {
    let dummy_caps: &dyn Fn(Option<Capability>) -> bool = &|_| true;
    category_width(bindings, show_labels, dummy_caps)
}

pub(super) fn select_width_tier(
    nav: &[KeyBinding],
    actions: &[KeyBinding],
    system: &[KeyBinding],
    caps: &dyn Fn(Option<Capability>) -> bool,
    width: u16,
) -> WidthTier {
    let w = width as usize;

    let left_full = measure_full_width(nav, actions, true, caps);
    let sys_full = measure_system(system, true);
    if left_full + SEP_CATEGORY.chars().count() + sys_full <= w {
        return WidthTier::Full;
    }

    let sys_no_label = measure_system(system, false);
    if left_full + SEP_CATEGORY.chars().count() + sys_no_label <= w {
        return WidthTier::DropSystemLabel;
    }

    let nav_full = category_width(nav, true, caps);
    let actions_no_labels = category_width(actions, false, caps);
    let nav_actions_drop_act = nav_full
        + if !nav.is_empty() && !actions.is_empty() {
            SEP_CATEGORY.chars().count()
        } else {
            0
        }
        + actions_no_labels;
    if nav_actions_drop_act + SEP_CATEGORY.chars().count() + sys_no_label <= w {
        return WidthTier::DropActionsLabels;
    }

    let nav_no_labels = category_width(nav, false, caps);
    let total_no_nav_labels = nav_no_labels
        + if !nav.is_empty() && !actions.is_empty() {
            SEP_CATEGORY.chars().count()
        } else {
            0
        }
        + actions_no_labels
        + SEP_CATEGORY.chars().count()
        + sys_no_label;
    if total_no_nav_labels <= w {
        return WidthTier::DropNavLabels;
    }

    WidthTier::FirstKeyOnly
}

pub(super) fn select_simple_tier(bindings: &[KeyBinding], width: u16) -> WidthTier {
    let w = width as usize;
    if measure_simple_bindings(bindings, true) <= w {
        return WidthTier::Full;
    }
    if measure_simple_bindings(bindings, false) <= w {
        return WidthTier::DropNavLabels;
    }
    WidthTier::FirstKeyOnly
}

pub(super) fn render_binding(
    binding: &KeyBinding,
    show_label: bool,
    enabled: bool,
) -> Vec<Span<'static>> {
    if !enabled {
        return vec![Span::styled(
            binding.glyph.to_string(),
            Style::default().fg(DISABLED_DIM),
        )];
    }

    let glyph_color = if binding.is_primary {
        ENABLED_GLYPH_PRIMARY
    } else {
        ENABLED_GLYPH
    };

    let mut spans = vec![Span::styled(
        binding.glyph.to_string(),
        Style::default().fg(glyph_color),
    )];

    if show_label {
        spans.push(Span::styled(
            format!(" {}", binding.action),
            Style::default().fg(ENABLED_ACTION),
        ));
    }

    spans
}

pub(super) fn select_modal_tier(
    actions: &[KeyBinding],
    system: &[KeyBinding],
    width: u16,
) -> WidthTier {
    let w = width as usize;

    let act_full = measure_simple_bindings(actions, true);
    let sys_full = measure_system(system, true);
    let total = act_full + SEP_CATEGORY.chars().count() + sys_full;
    if total <= w {
        return WidthTier::Full;
    }

    let sys_no_label = measure_system(system, false);
    let total_drop_sys = act_full + SEP_CATEGORY.chars().count() + sys_no_label;
    if total_drop_sys <= w {
        return WidthTier::DropSystemLabel;
    }

    let act_no_labels = measure_simple_bindings(actions, false);
    let total_drop_act = act_no_labels + SEP_CATEGORY.chars().count() + sys_no_label;
    if total_drop_act <= w {
        return WidthTier::DropActionsLabels;
    }

    WidthTier::FirstKeyOnly
}

#[cfg(test)]
mod tests {
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
    fn measure_simple_bindings_drops_labels_when_requested() {
        let bindings = [binding("Esc", "quit", false, None)];
        assert!(
            measure_simple_bindings(&bindings, true) > measure_simple_bindings(&bindings, false)
        );
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
}
