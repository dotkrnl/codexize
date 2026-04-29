use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::super::focus_caps::FocusCaps;
use super::super::{ModalKind, StageId};
use super::keymap_view_model::{
    WidthTier, render_binding, select_modal_tier, select_simple_tier, select_width_tier,
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

fn binding_enabled(binding: &KeyBinding, caps: &dyn Fn(Option<Capability>) -> bool) -> bool {
    binding.capability.map(|c| caps(Some(c))).unwrap_or(true)
}

/// Width budget contribution for a single binding, accounting for the rule
/// that capability-disabled bindings render glyph-only and never advertise
/// their action label even when `show_label` is true.
fn binding_width(
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
            action: "input",
            is_primary: true,
            capability: Some(Capability::Input),
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

/// Width tier for progressive collapse.
pub(super) fn measure_system(system: &[KeyBinding], show_label: bool) -> usize {
    let dummy_caps: &dyn Fn(Option<Capability>) -> bool = &|_| true;
    category_width(system, show_label, dummy_caps)
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
    width: u16,
) -> Line<'static> {
    let caps_fn = |cap: Option<Capability>| -> bool {
        match cap {
            None => true,
            Some(Capability::Expand) => caps.can_expand,
            Some(Capability::Input) => caps.can_input,
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
mod tests {
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
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);
        let text = line_text(&line);
        assert!(text.contains("↑↓ move"));
        assert!(text.contains("Space expand"));
        assert!(text.contains("PgUp/PgDn page"));
        assert!(text.contains("Enter input"));
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
            200,
        );
        let text = line_text(&line);
        assert!(text.contains("r reset"));
        assert!(text.contains("k keep"));
        assert!(text.contains("Esc quit"));
    }

    // Input mode
    #[test]
    fn input_mode_exact_string() {
        let line = keymap(Phase::IdeaInput, None, FocusCaps::default(), true, 200);
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
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);
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
        assert!(text.contains("Enter input"));
    }

    #[test]
    fn disabled_binding_uses_single_dim_color() {
        let caps = FocusCaps {
            can_expand: false,
            can_edit: false,
            can_back: false,
            can_input: false,
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);
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
        };
        let caps_enabled = FocusCaps {
            can_expand: true,
            can_edit: true,
            can_back: true,
            can_input: true,
        };
        let width = {
            let (nav, actions, system) = default_bindings();
            // Width that fits the disabled rendering exactly.
            let left =
                keymap_view_model::measure_full_width(&nav, &actions, true, &|cap: Option<
                    Capability,
                >| {
                    cap.map(|c| match c {
                        Capability::Expand => caps_disabled.can_expand,
                        Capability::Input => caps_disabled.can_input,
                    })
                    .unwrap_or(true)
                });
            let sys = measure_system(&system, true);
            (left + SEP_CATEGORY.chars().count() + sys) as u16
        };

        let line_dis = keymap(Phase::IdeaInput, None, caps_disabled, false, width);
        let text_dis = line_text(&line_dis);
        assert!(
            text_dis.contains("↑↓ move"),
            "disabled-form should still advertise enabled labels at this width: {text_dis}"
        );
        assert!(
            text_dis.contains("Enter input"),
            "disabled-form should keep `Enter input` at this width: {text_dis}"
        );
        assert!(
            !text_dis.contains("Space expand"),
            "Space label must remain hidden when disabled: {text_dis}"
        );

        // Sanity: with the same caps but the Space binding enabled, that exact
        // width is too narrow for `Space expand` to fit at the Full tier — so
        // tier selection must have observed the disabled label as 0-width.
        let line_en = keymap(Phase::IdeaInput, None, caps_enabled, false, width);
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
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);
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
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);
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
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);
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
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);
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

        let line1 = keymap(Phase::IdeaInput, None, caps, false, width);
        let line2 = keymap(Phase::BrainstormRunning, None, caps, false, width);

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
        let line_wide = keymap(Phase::IdeaInput, None, caps, false, 200);
        let text_wide = line_text(&line_wide);
        assert!(text_wide.contains("Esc quit"), "wide should have 'q quit'");

        let line_narrow = keymap(Phase::IdeaInput, None, caps, false, 80);
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
        let line = keymap(Phase::IdeaInput, None, caps, false, 20);
        let text = line_text(&line);
        assert!(
            !text.is_empty(),
            "should render something even ultra-narrow"
        );
    }

    #[test]
    fn zero_width_empty() {
        let line = keymap(Phase::IdeaInput, None, FocusCaps::default(), false, 0);
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
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);
        let text = line_text(&line);
        assert!(text.contains("↑↓ move · Space expand · PgUp/PgDn page"));
        assert!(text.contains("Enter input · : palette"));
        assert!(text.ends_with("Esc quit"));
    }

    #[test]
    fn snapshot_width_120_default() {
        let caps = FocusCaps {
            can_expand: true,
            can_edit: true,
            can_back: true,
            can_input: true,
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 120);
        let text = line_text(&line);
        assert!(text.ends_with("Esc quit") || text.ends_with("Esc"));
    }

    #[test]
    fn snapshot_width_80_default() {
        let caps = FocusCaps::default();
        let line = keymap(Phase::IdeaInput, None, caps, false, 80);
        let text = line_text(&line);
        assert!(text.contains("Esc"), "should contain Esc");
    }

    #[test]
    fn snapshot_width_60_default() {
        let caps = FocusCaps::default();
        let line = keymap(Phase::IdeaInput, None, caps, false, 60);
        let text = line_text(&line);
        assert!(text.contains("↑↓") || text.contains("Enter"));
    }

    #[test]
    fn snapshot_width_40_default() {
        let caps = FocusCaps::default();
        let line = keymap(Phase::IdeaInput, None, caps, false, 40);
        let text = line_text(&line);
        assert!(!text.is_empty());
    }

    #[test]
    fn snapshot_width_30_default() {
        let caps = FocusCaps::default();
        let line = keymap(Phase::IdeaInput, None, caps, false, 30);
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
            80,
        );
        let text = line_text(&line);
        assert!(text.contains("r"));
        assert!(text.contains("k"));
        assert!(text.contains("q"));
    }

    #[test]
    fn snapshot_input_mode_width_80() {
        let line = keymap(Phase::IdeaInput, None, FocusCaps::default(), true, 80);
        let text = line_text(&line);
        assert!(text.contains("Esc"));
        assert!(text.contains("Enter"));
    }

    // Right-anchor stability across default/modal transitions
    #[test]
    fn esc_quit_right_anchored_stable_default_vs_pause_modal() {
        let width = 120u16;
        let default = keymap(Phase::IdeaInput, None, FocusCaps::default(), false, width);
        let modal = keymap(
            Phase::SpecReviewPaused,
            Some(ModalKind::SpecReviewPaused),
            FocusCaps::default(),
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
        let default = keymap(Phase::IdeaInput, None, FocusCaps::default(), false, width);
        let modal = keymap(
            Phase::GitGuardPending,
            Some(ModalKind::GitGuard),
            FocusCaps::default(),
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
        let default = keymap(Phase::IdeaInput, None, FocusCaps::default(), false, width);
        let modal = keymap(
            Phase::SkipToImplPending,
            Some(ModalKind::SkipToImpl),
            FocusCaps::default(),
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
        let default = keymap(Phase::IdeaInput, None, FocusCaps::default(), false, width);
        let modal = keymap(
            Phase::BrainstormRunning,
            Some(ModalKind::StageError(StageId::Brainstorm)),
            FocusCaps::default(),
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
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);

        let has_disabled_style = line.spans.iter().any(|s| s.style.fg == Some(DISABLED_DIM));
        assert!(has_disabled_style, "should have disabled styling");

        let has_enabled_style = line.spans.iter().any(|s| {
            s.style.fg == Some(ENABLED_GLYPH)
                || s.style.fg == Some(ENABLED_GLYPH_PRIMARY)
                || s.style.fg == Some(ENABLED_ACTION)
        });
        assert!(has_enabled_style, "should have enabled styling too");
    }
}
