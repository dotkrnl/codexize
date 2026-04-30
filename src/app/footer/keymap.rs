use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::super::focus_caps::FocusCaps;
use super::super::{ModalKind, StageId};
use super::keymap_view_model::{
    WidthTier, binding_enabled, render_binding, select_modal_tier, select_simple_tier,
    select_width_tier,
};
use crate::state::Phase;

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
    let nav = vec![
        KeyBinding {
            glyph: "↑↓",
            action: "move",
            is_primary: false,
            capability: None,
        },
        KeyBinding {
            glyph: "Space",
            action: "expand",
            is_primary: false,
            capability: Some(Capability::Expand),
        },
        KeyBinding {
            glyph: "PgUp/PgDn",
            action: "page",
            is_primary: false,
            capability: None,
        },
    ];
    let actions = vec![
        KeyBinding {
            glyph: "Enter",
            action: "show",
            is_primary: true,
            capability: Some(Capability::Split),
        },
        KeyBinding {
            glyph: ":",
            action: "palette",
            is_primary: false,
            capability: None,
        },
    ];
    (nav, actions, system_bindings())
}

const SYSTEM_QUIT: KeyBinding = KeyBinding {
    glyph: "Esc",
    action: "quit",
    is_primary: false,
    capability: None,
};

fn system_bindings() -> Vec<KeyBinding> {
    vec![SYSTEM_QUIT]
}

/// Pause modal: actions + system.
fn pause_bindings() -> (Vec<KeyBinding>, Vec<KeyBinding>) {
    (
        vec![
            KeyBinding {
                glyph: "Enter",
                action: "continue",
                is_primary: true,
                capability: None,
            },
            KeyBinding {
                glyph: "n",
                action: "new reviewer",
                is_primary: false,
                capability: None,
            },
        ],
        system_bindings(),
    )
}

/// Stage error modal: actions + system.
fn stage_error_bindings(stage_id: StageId) -> (Vec<KeyBinding>, Vec<KeyBinding>) {
    let mut actions = vec![KeyBinding {
        glyph: "r",
        action: "retry",
        is_primary: true,
        capability: None,
    }];
    if stage_id == StageId::Brainstorm {
        actions.push(KeyBinding {
            glyph: "e",
            action: "edit idea",
            is_primary: false,
            capability: None,
        });
    }
    (actions, system_bindings())
}

/// Skip-to-impl modal: actions + system.
fn skip_to_impl_bindings() -> (Vec<KeyBinding>, Vec<KeyBinding>) {
    (
        vec![
            KeyBinding {
                glyph: "y",
                action: "accept",
                is_primary: true,
                capability: None,
            },
            KeyBinding {
                glyph: "n",
                action: "decline",
                is_primary: false,
                capability: None,
            },
        ],
        system_bindings(),
    )
}

/// Guard modal: actions + system.
fn guard_bindings() -> (Vec<KeyBinding>, Vec<KeyBinding>) {
    (
        vec![
            KeyBinding {
                glyph: "r",
                action: "reset",
                is_primary: true,
                capability: None,
            },
            KeyBinding {
                glyph: "k",
                action: "keep",
                is_primary: false,
                capability: None,
            },
        ],
        system_bindings(),
    )
}

/// Input mode bindings.
fn input_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding {
            glyph: "Esc",
            action: "cancel",
            is_primary: false,
            capability: None,
        },
        KeyBinding {
            glyph: "Enter",
            action: "submit",
            is_primary: true,
            capability: None,
        },
    ]
}

/// Split mode bindings (when not in input mode).
fn split_bindings() -> (Vec<KeyBinding>, Vec<KeyBinding>, Vec<KeyBinding>) {
    let nav = vec![
        KeyBinding {
            glyph: "↑↓",
            action: "scroll",
            is_primary: false,
            capability: None,
        },
        KeyBinding {
            glyph: "PgUp/PgDn",
            action: "page",
            is_primary: false,
            capability: None,
        },
    ];
    let actions = vec![KeyBinding {
        glyph: ":",
        action: "palette",
        is_primary: false,
        capability: None,
    }];
    let system = vec![KeyBinding {
        glyph: "Esc",
        action: "close",
        is_primary: false,
        capability: None,
    }];
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

    if input_mode {
        return render_keymap_line(&[&input_bindings()], &caps_fn, width);
    }

    if let Some(modal_kind) = modal {
        let (actions, system) = match modal_kind {
            ModalKind::SpecReviewPaused | ModalKind::PlanReviewPaused => pause_bindings(),
            ModalKind::SkipToImpl => skip_to_impl_bindings(),
            ModalKind::GitGuard => guard_bindings(),
            ModalKind::StageError(stage_id) => stage_error_bindings(stage_id),
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
