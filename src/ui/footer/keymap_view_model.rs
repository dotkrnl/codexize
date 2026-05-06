use super::keymap::{
    Capability, DISABLED_DIM, ENABLED_ACTION, ENABLED_GLYPH, ENABLED_GLYPH_PRIMARY, KeyBinding,
    SEP_CATEGORY, SEP_INNER,
};
use ratatui::{style::Style, text::Span};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WidthTier {
    Full,
    DropSystemLabel,
    DropActionsLabels,
    DropNavLabels,
    FirstKeyOnly,
}
pub(super) fn binding_enabled(
    binding: &KeyBinding,
    caps: &dyn Fn(Option<Capability>) -> bool,
) -> bool {
    binding.capability.map(|c| caps(Some(c))).unwrap_or(true)
}
pub(super) fn binding_width(
    binding: &KeyBinding,
    show_label: bool,
    caps: &dyn Fn(Option<Capability>) -> bool,
) -> usize {
    let mut len = binding.glyph.chars().count();
    if show_label && binding_enabled(binding, caps) {
        len += 1 + binding.action.chars().count();
    }
    len
}
pub(super) fn category_width(
    bindings: &[KeyBinding],
    show_labels: bool,
    caps: &dyn Fn(Option<Capability>) -> bool,
) -> usize {
    let mut len = 0;
    for (i, b) in bindings.iter().enumerate() {
        if i > 0 {
            len += SEP_INNER.chars().count();
        }
        len += binding_width(b, show_labels, caps);
    }
    len
}
pub(super) fn measure_system(system: &[KeyBinding], show_label: bool) -> usize {
    let dummy_caps: &dyn Fn(Option<Capability>) -> bool = &|_| true;
    category_width(system, show_label, dummy_caps)
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
#[path = "keymap_view_model_tests.rs"]
mod tests;
