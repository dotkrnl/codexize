use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::keymap_view_model::{
    WidthTier, binding_enabled, render_binding, select_modal_tier, select_simple_tier,
    select_width_tier,
};
use crate::app::{ModalKind, StageId};
use crate::state::Phase;
use crate::ui::focus_caps::FocusCaps;

/// Key binding with optional capability requirement.
#[derive(Clone, Copy)]
pub(crate) struct KeyBinding {
    pub(crate) glyph: &'static str,
    pub(crate) action: &'static str,
    pub(crate) is_primary: bool,
    pub(crate) capability: Option<Capability>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Capability {
    Expand,
    Input,
    Split,
}

macro_rules! bindings {
    ($($binding:expr),* $(,)?) => {
        vec![$($binding),*]
    };
}

const fn key(glyph: &'static str, action: &'static str) -> KeyBinding {
    KeyBinding {
        glyph,
        action,
        is_primary: false,
        capability: None,
    }
}

const fn primary_key(glyph: &'static str, action: &'static str) -> KeyBinding {
    KeyBinding {
        glyph,
        action,
        is_primary: true,
        capability: None,
    }
}

const fn gated_key(
    glyph: &'static str,
    action: &'static str,
    capability: Capability,
) -> KeyBinding {
    KeyBinding {
        glyph,
        action,
        is_primary: false,
        capability: Some(capability),
    }
}

const fn primary_gated_key(
    glyph: &'static str,
    action: &'static str,
    capability: Capability,
) -> KeyBinding {
    KeyBinding {
        glyph,
        action,
        is_primary: true,
        capability: Some(capability),
    }
}

/// Colors for keymap styling.
pub(crate) const ENABLED_GLYPH: Color = Color::White;
pub(crate) const ENABLED_GLYPH_PRIMARY: Color = Color::Blue;
pub(crate) const ENABLED_ACTION: Color = Color::DarkGray;
/// Single shared dim color for capability-disabled bindings (glyph-only).
pub(crate) const DISABLED_DIM: Color = Color::Rgb(80, 80, 80);
pub(crate) const RULE_COLOR: Color = Color::DarkGray;

/// Separator within a category.
pub(crate) const SEP_INNER: &str = " · ";
/// Separator between categories.
pub(crate) const SEP_CATEGORY: &str = "  ·  ";

/// Default phase keymap: navigation · actions · system.
fn default_bindings() -> (Vec<KeyBinding>, Vec<KeyBinding>, Vec<KeyBinding>) {
    let nav = bindings![
        key("↑↓", "move"),
        gated_key("Space", "expand", Capability::Expand),
        key("PgUp/PgDn", "page"),
    ];
    let actions = bindings![
        primary_gated_key("Enter", "show", Capability::Split),
        key(":", "palette"),
    ];
    (nav, actions, system_bindings())
}

fn system_bindings() -> Vec<KeyBinding> {
    bindings![key("Esc", "quit")]
}

/// Pause modal: actions + system.
fn pause_bindings() -> (Vec<KeyBinding>, Vec<KeyBinding>) {
    (
        bindings![primary_key("Enter", "continue"), key("n", "new reviewer")],
        system_bindings(),
    )
}

/// Stage error modal: actions + system.
fn stage_error_bindings(stage_id: StageId) -> (Vec<KeyBinding>, Vec<KeyBinding>) {
    let mut actions = bindings![primary_key("r", "retry")];
    if stage_id == StageId::Brainstorm {
        actions.extend(bindings![key("e", "edit idea")]);
    }
    (actions, system_bindings())
}

/// Final validation blocked modal: actions + system.
fn final_validation_blocked_bindings() -> (Vec<KeyBinding>, Vec<KeyBinding>) {
    (
        bindings![primary_key("f", "force ship to done"), key("r", "recover")],
        system_bindings(),
    )
}

/// Skip-to-impl modal: actions + system.
fn skip_to_impl_bindings() -> (Vec<KeyBinding>, Vec<KeyBinding>) {
    (
        bindings![primary_key("y", "accept"), key("n", "decline")],
        system_bindings(),
    )
}

/// Guard modal: actions + system.
fn guard_bindings() -> (Vec<KeyBinding>, Vec<KeyBinding>) {
    (
        bindings![primary_key("r", "reset"), key("k", "keep")],
        system_bindings(),
    )
}

fn quit_running_agent_bindings() -> (Vec<KeyBinding>, Vec<KeyBinding>) {
    (
        bindings![primary_key("Enter", "confirm"), key("y", "confirm")],
        bindings![key("Esc", "cancel"), key("n", "cancel")],
    )
}

fn interactive_exit_prompt_bindings() -> (Vec<KeyBinding>, Vec<KeyBinding>) {
    (
        bindings![primary_key("Enter", "no requests")],
        bindings![key("Esc", "request")],
    )
}

/// Input mode bindings.
fn input_bindings() -> Vec<KeyBinding> {
    bindings![key("Esc", "cancel"), primary_key("Enter", "submit")]
}

fn split_input_bindings() -> Vec<KeyBinding> {
    bindings![key("Esc", "close"), primary_key("Enter", "submit")]
}

/// Split mode bindings (when not in input mode).
fn split_bindings() -> (Vec<KeyBinding>, Vec<KeyBinding>, Vec<KeyBinding>) {
    let nav = bindings![key("↑↓", "scroll"), key("PgUp/PgDn", "page")];
    let actions = bindings![key(":", "palette")];
    let system = bindings![key("Esc", "close")];
    (nav, actions, system)
}

fn render_category(
    bindings: &[KeyBinding],
    caps: &dyn Fn(Option<Capability>) -> bool,
    show_labels: bool,
    first_only: bool,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let items: Vec<_> = if first_only {
        bindings.iter().take(1).collect()
    } else {
        bindings.iter().collect()
    };

    for (i, binding) in items.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                SEP_INNER.to_string(),
                Style::default().fg(ENABLED_ACTION),
            ));
        }
        let enabled = binding_enabled(binding, caps);
        spans.extend(render_binding(binding, show_labels, enabled));
    }

    spans
}

fn render_modal_keymap(
    actions: &[KeyBinding],
    system: &[KeyBinding],
    caps: &dyn Fn(Option<Capability>) -> bool,
    width: u16,
) -> Line<'static> {
    let tier = select_modal_tier(actions, system, width);

    let (act_labels, sys_label, first_only) = match tier {
        WidthTier::Full => (true, true, false),
        WidthTier::DropSystemLabel => (true, false, false),
        WidthTier::DropActionsLabels | WidthTier::DropNavLabels => (false, false, false),
        WidthTier::FirstKeyOnly => (false, false, true),
    };

    let left_spans = render_category(actions, caps, act_labels, first_only);
    let sys_spans = render_category(system, caps, sys_label, first_only);

    let left_len: usize = left_spans.iter().map(|s| s.content.chars().count()).sum();
    let sys_len: usize = sys_spans.iter().map(|s| s.content.chars().count()).sum();
    let sep_len = SEP_CATEGORY.chars().count();

    let fill_needed = (width as usize).saturating_sub(left_len + sep_len + sys_len);

    let mut spans = left_spans;

    if fill_needed > 0 {
        spans.push(Span::styled(
            format!("{}{}", SEP_CATEGORY, " ".repeat(fill_needed)),
            Style::default().fg(RULE_COLOR),
        ));
    } else {
        spans.push(Span::styled(
            SEP_CATEGORY.to_string(),
            Style::default().fg(ENABLED_ACTION),
        ));
    }

    spans.extend(sys_spans);
    Line::from(spans)
}

fn render_simple_keymap(
    bindings: &[KeyBinding],
    caps: &dyn Fn(Option<Capability>) -> bool,
    width: u16,
) -> Line<'static> {
    let tier = select_simple_tier(bindings, width);
    let (show_labels, first_only) = match tier {
        WidthTier::Full | WidthTier::DropSystemLabel | WidthTier::DropActionsLabels => {
            (true, false)
        }
        WidthTier::DropNavLabels => (false, false),
        WidthTier::FirstKeyOnly => (false, true),
    };

    let spans = render_category(bindings, caps, show_labels, first_only);
    Line::from(spans)
}

fn render_default_keymap(
    nav: &[KeyBinding],
    actions: &[KeyBinding],
    system: &[KeyBinding],
    caps: &dyn Fn(Option<Capability>) -> bool,
    width: u16,
) -> Line<'static> {
    let tier = select_width_tier(nav, actions, system, caps, width);

    let (nav_labels, act_labels, sys_label, first_only) = match tier {
        WidthTier::Full => (true, true, true, false),
        WidthTier::DropSystemLabel => (true, true, false, false),
        WidthTier::DropActionsLabels => (true, false, false, false),
        WidthTier::DropNavLabels => (false, false, false, false),
        WidthTier::FirstKeyOnly => (false, false, false, true),
    };

    let left_spans = {
        let mut spans = Vec::new();
        let nav_spans = render_category(nav, caps, nav_labels, first_only);
        spans.extend(nav_spans);

        if !first_only || !actions.is_empty() {
            spans.push(Span::styled(
                SEP_CATEGORY.to_string(),
                Style::default().fg(ENABLED_ACTION),
            ));
        }

        let act_spans = render_category(actions, caps, act_labels, first_only);
        spans.extend(act_spans);
        spans
    };

    let sys_spans = render_category(system, &|_| true, sys_label, first_only);

    let left_len: usize = left_spans.iter().map(|s| s.content.chars().count()).sum();
    let sys_len: usize = sys_spans.iter().map(|s| s.content.chars().count()).sum();
    let sep_len = SEP_CATEGORY.chars().count();

    let fill_needed = (width as usize).saturating_sub(left_len + sep_len + sys_len);

    let mut spans = left_spans;

    if fill_needed > 0 {
        spans.push(Span::styled(
            format!("{}{}", SEP_CATEGORY, " ".repeat(fill_needed)),
            Style::default().fg(RULE_COLOR),
        ));
    } else {
        spans.push(Span::styled(
            SEP_CATEGORY.to_string(),
            Style::default().fg(ENABLED_ACTION),
        ));
    }

    spans.extend(sys_spans);
    Line::from(spans)
}

/// Renders a context-aware keymap line.
///
/// Pure function that produces the exact keymap for a given phase, modal,
/// focus capabilities, and terminal width.
///
/// # Arguments
/// * `phase` - Current pipeline phase.
/// * `modal` - Active modal (overrides phase keymap).
/// * `caps` - Capabilities of the currently focused row.
/// * `input_mode` - Whether input mode is active.
/// * `width` - Available terminal width in columns.
pub fn keymap(
    _phase: Phase,
    modal: Option<ModalKind>,
    caps: FocusCaps,
    input_mode: bool,
    split_open: bool,
    width: u16,
) -> Line<'static> {
    let caps_fn = |cap: Option<Capability>| -> bool {
        match cap {
            None => true,
            Some(Capability::Expand) => caps.can_expand,
            Some(Capability::Input) => caps.can_input,
            Some(Capability::Split) => caps.can_split,
        }
    };

    if width == 0 {
        return Line::from(vec![]);
    }

    if input_mode && split_open {
        return render_keymap_line(&[&split_input_bindings()], &caps_fn, width);
    }

    if input_mode {
        return render_keymap_line(&[&input_bindings()], &caps_fn, width);
    }

    if let Some(modal_kind) = modal {
        let (actions, system) = match modal_kind {
            ModalKind::SpecReviewPaused | ModalKind::PlanReviewPaused => pause_bindings(),
            ModalKind::SkipToImpl => skip_to_impl_bindings(),
            ModalKind::GitGuard => guard_bindings(),
            ModalKind::QuitRunningAgent => quit_running_agent_bindings(),
            ModalKind::InteractiveExitPrompt => interactive_exit_prompt_bindings(),
            ModalKind::StageError(stage_id) => stage_error_bindings(stage_id),
            ModalKind::FinalValidationBlocked => final_validation_blocked_bindings(),
        };
        return render_keymap_line(&[&actions, &system], &caps_fn, width);
    }

    if split_open {
        let (nav, actions, system) = split_bindings();
        return render_keymap_line(&[&nav, &actions, &system], &caps_fn, width);
    }

    let (nav, actions, system) = default_bindings();
    render_keymap_line(&[&nav, &actions, &system], &caps_fn, width)
}

/// Renders a context-aware keymap line.
pub fn render_keymap_line(
    categories: &[&[KeyBinding]],
    caps: &dyn Fn(Option<Capability>) -> bool,
    width: u16,
) -> Line<'static> {
    match categories.len() {
        1 => render_simple_keymap(categories[0], caps, width),
        2 => render_modal_keymap(categories[0], categories[1], caps, width),
        3 => render_default_keymap(categories[0], categories[1], categories[2], caps, width),
        _ => Line::from(vec![]),
    }
}

#[cfg(test)]
mod tests_mod;
