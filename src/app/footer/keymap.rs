use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::super::focus_caps::FocusCaps;
use super::super::{ModalKind, StageId};
use crate::state::Phase;

/// Key binding with optional capability requirement.
#[derive(Clone, Copy)]
struct KeyBinding {
    glyph: &'static str,
    action: &'static str,
    is_primary: bool,
    capability: Option<Capability>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Capability {
    Expand,
}

/// Colors for keymap styling.
const ENABLED_GLYPH: Color = Color::White;
const ENABLED_GLYPH_PRIMARY: Color = Color::Blue;
const ENABLED_ACTION: Color = Color::DarkGray;
const DISABLED_GLYPH: Color = Color::Rgb(80, 80, 80);
const DISABLED_ACTION: Color = Color::Rgb(80, 80, 80);
const RULE_COLOR: Color = Color::DarkGray;

/// Separator within a category.
const SEP_INNER: &str = " · ";
/// Separator between categories.
const SEP_CATEGORY: &str = "  ·  ";

fn is_capable(caps: FocusCaps, cap: Capability) -> bool {
    match cap {
        Capability::Expand => caps.can_expand,
    }
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
            capability: None,
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
    glyph: "q",
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum WidthTier {
    Full,
    DropSystemLabel,
    DropActionsLabels,
    DropNavLabels,
    FirstKeyOnly,
}

fn measure_full_width(
    nav: &[KeyBinding],
    actions: &[KeyBinding],
    _system: &[KeyBinding],
    show_labels: bool,
) -> usize {
    let mut len = 0;

    for (i, b) in nav.iter().enumerate() {
        if i > 0 {
            len += SEP_INNER.chars().count();
        }
        len += b.glyph.chars().count();
        if show_labels {
            len += 1 + b.action.chars().count();
        }
    }

    if !nav.is_empty() && !actions.is_empty() {
        len += SEP_CATEGORY.chars().count();
    }

    for (i, b) in actions.iter().enumerate() {
        if i > 0 {
            len += SEP_INNER.chars().count();
        }
        len += b.glyph.chars().count();
        if show_labels {
            len += 1 + b.action.chars().count();
        }
    }

    len
}

fn measure_system(system: &[KeyBinding], show_label: bool) -> usize {
    let mut len = 0;
    for (i, b) in system.iter().enumerate() {
        if i > 0 {
            len += SEP_INNER.chars().count();
        }
        len += b.glyph.chars().count();
        if show_label {
            len += 1 + b.action.chars().count();
        }
    }
    len
}

fn measure_simple_bindings(bindings: &[KeyBinding], show_labels: bool) -> usize {
    let mut len = 0;
    for (i, b) in bindings.iter().enumerate() {
        if i > 0 {
            len += SEP_INNER.chars().count();
        }
        len += b.glyph.chars().count();
        if show_labels {
            len += 1 + b.action.chars().count();
        }
    }
    len
}

fn select_width_tier(
    nav: &[KeyBinding],
    actions: &[KeyBinding],
    system: &[KeyBinding],
    width: u16,
) -> WidthTier {
    let w = width as usize;

    let left_full = measure_full_width(nav, actions, system, true);
    let sys_full = measure_system(system, true);
    let total_full = left_full + SEP_CATEGORY.chars().count() + sys_full;

    if total_full <= w {
        return WidthTier::Full;
    }

    let sys_no_label = measure_system(system, false);
    let total_drop_sys = left_full + SEP_CATEGORY.chars().count() + sys_no_label;
    if total_drop_sys <= w {
        return WidthTier::DropSystemLabel;
    }

    let _left_nav_labels = measure_full_width(nav, actions, system, true)
        - actions
            .iter()
            .map(|b| 1 + b.action.chars().count())
            .sum::<usize>();
    let nav_and_actions_no_act = {
        let mut len = 0;
        for (i, b) in nav.iter().enumerate() {
            if i > 0 {
                len += SEP_INNER.chars().count();
            }
            len += b.glyph.chars().count() + 1 + b.action.chars().count();
        }
        if !nav.is_empty() && !actions.is_empty() {
            len += SEP_CATEGORY.chars().count();
        }
        for (i, b) in actions.iter().enumerate() {
            if i > 0 {
                len += SEP_INNER.chars().count();
            }
            len += b.glyph.chars().count();
        }
        len
    };
    let total_drop_act = nav_and_actions_no_act + SEP_CATEGORY.chars().count() + sys_no_label;
    if total_drop_act <= w {
        return WidthTier::DropActionsLabels;
    }

    let nav_no_labels = {
        let mut len = 0;
        for (i, b) in nav.iter().enumerate() {
            if i > 0 {
                len += SEP_INNER.chars().count();
            }
            len += b.glyph.chars().count();
        }
        len
    };
    let actions_no_labels = {
        let mut len = 0;
        for (i, b) in actions.iter().enumerate() {
            if i > 0 {
                len += SEP_INNER.chars().count();
            }
            len += b.glyph.chars().count();
        }
        len
    };
    let total_no_nav_labels = nav_no_labels
        + (if !nav.is_empty() && !actions.is_empty() {
            SEP_CATEGORY.chars().count()
        } else {
            0
        })
        + actions_no_labels
        + SEP_CATEGORY.chars().count()
        + sys_no_label;
    if total_no_nav_labels <= w {
        return WidthTier::DropNavLabels;
    }

    WidthTier::FirstKeyOnly
}

fn select_simple_tier(bindings: &[KeyBinding], width: u16) -> WidthTier {
    let w = width as usize;
    let full = measure_simple_bindings(bindings, true);
    if full <= w {
        return WidthTier::Full;
    }

    let no_labels = measure_simple_bindings(bindings, false);
    if no_labels <= w {
        return WidthTier::DropNavLabels;
    }

    WidthTier::FirstKeyOnly
}

fn render_binding(binding: &KeyBinding, show_label: bool, enabled: bool) -> Vec<Span<'static>> {
    let (glyph_color, action_color) = if enabled {
        let gc = if binding.is_primary {
            ENABLED_GLYPH_PRIMARY
        } else {
            ENABLED_GLYPH
        };
        (gc, ENABLED_ACTION)
    } else {
        (DISABLED_GLYPH, DISABLED_ACTION)
    };

    let mut spans = vec![Span::styled(
        binding.glyph.to_string(),
        Style::default().fg(glyph_color),
    )];

    if show_label {
        spans.push(Span::styled(
            format!(" {}", binding.action),
            Style::default().fg(action_color),
        ));
    }

    spans
}

fn render_category(
    bindings: &[KeyBinding],
    caps: FocusCaps,
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
        let enabled = binding
            .capability
            .map(|c| is_capable(caps, c))
            .unwrap_or(true);
        spans.extend(render_binding(binding, show_labels, enabled));
    }

    spans
}

fn select_modal_tier(actions: &[KeyBinding], system: &[KeyBinding], width: u16) -> WidthTier {
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

fn render_modal_keymap(actions: &[KeyBinding], system: &[KeyBinding], width: u16) -> Line<'static> {
    let tier = select_modal_tier(actions, system, width);

    let (act_labels, sys_label, first_only) = match tier {
        WidthTier::Full => (true, true, false),
        WidthTier::DropSystemLabel => (true, false, false),
        WidthTier::DropActionsLabels | WidthTier::DropNavLabels => (false, false, false),
        WidthTier::FirstKeyOnly => (false, false, true),
    };

    let left_spans = render_category(actions, FocusCaps::default(), act_labels, first_only);
    let sys_spans = render_category(system, FocusCaps::default(), sys_label, first_only);

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

fn render_simple_keymap(bindings: &[KeyBinding], width: u16) -> Line<'static> {
    let tier = select_simple_tier(bindings, width);
    let (show_labels, first_only) = match tier {
        WidthTier::Full | WidthTier::DropSystemLabel | WidthTier::DropActionsLabels => {
            (true, false)
        }
        WidthTier::DropNavLabels => (false, false),
        WidthTier::FirstKeyOnly => (false, true),
    };

    let spans = render_category(bindings, FocusCaps::default(), show_labels, first_only);
    Line::from(spans)
}

fn render_default_keymap(caps: FocusCaps, width: u16) -> Line<'static> {
    let (nav, actions, system) = default_bindings();
    let tier = select_width_tier(&nav, &actions, &system, width);

    let (nav_labels, act_labels, sys_label, first_only) = match tier {
        WidthTier::Full => (true, true, true, false),
        WidthTier::DropSystemLabel => (true, true, false, false),
        WidthTier::DropActionsLabels => (true, false, false, false),
        WidthTier::DropNavLabels => (false, false, false, false),
        WidthTier::FirstKeyOnly => (false, false, false, true),
    };

    let left_spans = {
        let mut spans = Vec::new();
        let nav_spans = render_category(&nav, caps, nav_labels, first_only);
        spans.extend(nav_spans);

        if !first_only || !actions.is_empty() {
            spans.push(Span::styled(
                SEP_CATEGORY.to_string(),
                Style::default().fg(ENABLED_ACTION),
            ));
        }

        let act_spans = render_category(&actions, caps, act_labels, first_only);
        spans.extend(act_spans);
        spans
    };

    let sys_spans = render_category(&system, FocusCaps::default(), sys_label, first_only);

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
    if width == 0 {
        return Line::from(vec![]);
    }

    if input_mode {
        return render_simple_keymap(&input_bindings(), width);
    }

    if let Some(modal_kind) = modal {
        let (actions, system) = match modal_kind {
            ModalKind::SpecReviewPaused | ModalKind::PlanReviewPaused => pause_bindings(),
            ModalKind::SkipToImpl => skip_to_impl_bindings(),
            ModalKind::GitGuard => guard_bindings(),
            ModalKind::StageError(stage_id) => stage_error_bindings(stage_id),
        };
        return render_modal_keymap(&actions, &system, width);
    }

    render_default_keymap(caps, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|s| s.content.to_string())
            .collect::<String>()
    }

    fn has_dim_spans(line: &Line) -> bool {
        line.spans
            .iter()
            .any(|s| s.style.fg == Some(DISABLED_GLYPH) || s.style.fg == Some(DISABLED_ACTION))
    }

    #[test]
    fn default_phase_exact_string_wide() {
        let caps = FocusCaps {
            can_expand: true,
            can_edit: true,
            can_back: true,
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);
        let text = line_text(&line);
        assert!(text.contains("↑↓ move"));
        assert!(text.contains("Space expand"));
        assert!(text.contains("PgUp/PgDn page"));
        assert!(text.contains("Enter input"));
        assert!(text.contains(": palette"));
        assert!(text.contains("q quit"));
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
        assert!(text.contains("q quit"));
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
        assert!(text.contains("q quit"));
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
        assert!(text.contains("q quit"));
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
        assert!(text.contains("q quit"));
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
        assert!(text.contains("q quit"));
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
        assert!(text.contains("q quit"));
    }

    // Input mode
    #[test]
    fn input_mode_exact_string() {
        let line = keymap(Phase::IdeaInput, None, FocusCaps::default(), true, 200);
        let text = line_text(&line);
        assert!(text.contains("Esc cancel"));
        assert!(text.contains("Enter submit"));
    }

    // Dim-in-place tests
    #[test]
    fn dim_in_place_expand_disabled() {
        let caps = FocusCaps {
            can_expand: false,
            can_edit: true,
            can_back: true,
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
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);
        let text = line_text(&line);
        assert!(text.contains("Space"), "Space should not dropout");
        assert!(text.contains(":"), "palette hint should not dropout");
    }

    // Right-anchor stability
    #[test]
    fn q_quit_right_anchored_stable_across_phases() {
        let caps = FocusCaps::default();
        let width = 120u16;

        let line1 = keymap(Phase::IdeaInput, None, caps, false, width);
        let line2 = keymap(Phase::BrainstormRunning, None, caps, false, width);

        let text1 = line_text(&line1);
        let text2 = line_text(&line2);

        assert!(text1.ends_with("q quit"));
        assert!(text2.ends_with("q quit"));

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
        assert!(text_wide.contains("q quit"), "wide should have 'q quit'");

        let line_narrow = keymap(Phase::IdeaInput, None, caps, false, 80);
        let text_narrow = line_text(&line_narrow);
        let ends_with_q = text_narrow.trim_end().ends_with("q");
        assert!(
            text_narrow.contains("q quit") || ends_with_q,
            "narrow should have 'q' or 'q quit'"
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
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);
        let text = line_text(&line);
        assert!(text.contains("↑↓ move · Space expand · PgUp/PgDn page"));
        assert!(text.contains("Enter input · : palette"));
        assert!(text.ends_with("q quit"));
    }

    #[test]
    fn snapshot_width_120_default() {
        let caps = FocusCaps {
            can_expand: true,
            can_edit: true,
            can_back: true,
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 120);
        let text = line_text(&line);
        assert!(text.ends_with("q quit") || text.ends_with("q"));
    }

    #[test]
    fn snapshot_width_80_default() {
        let caps = FocusCaps::default();
        let line = keymap(Phase::IdeaInput, None, caps, false, 80);
        let text = line_text(&line);
        assert!(text.contains("q"), "should contain q");
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
    fn q_quit_right_anchored_stable_default_vs_pause_modal() {
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

        assert!(default_text.ends_with("q quit"));
        assert!(modal_text.ends_with("q quit"));
        assert_eq!(
            default_text.chars().count(),
            modal_text.chars().count(),
            "line lengths must be equal for stable right-anchor"
        );
    }

    #[test]
    fn q_quit_right_anchored_stable_default_vs_guard_modal() {
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

        assert!(default_text.ends_with("q quit"));
        assert!(modal_text.ends_with("q quit"));
        assert_eq!(
            default_text.chars().count(),
            modal_text.chars().count(),
            "line lengths must be equal for stable right-anchor"
        );
    }

    #[test]
    fn q_quit_right_anchored_stable_default_vs_skip_to_impl() {
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

        assert!(default_text.ends_with("q quit"));
        assert!(modal_text.ends_with("q quit"));
        assert_eq!(
            default_text.chars().count(),
            modal_text.chars().count(),
            "line lengths must be equal for stable right-anchor"
        );
    }

    #[test]
    fn q_quit_right_anchored_stable_default_vs_stage_error() {
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

        assert!(default_text.ends_with("q quit"));
        assert!(modal_text.ends_with("q quit"));
        assert_eq!(
            default_text.chars().count(),
            modal_text.chars().count(),
            "line lengths must be equal for stable right-anchor"
        );
    }

    #[test]
    fn modal_q_quit_right_anchor_with_fill() {
        let width = 200u16;
        let modal = keymap(
            Phase::SpecReviewPaused,
            Some(ModalKind::SpecReviewPaused),
            FocusCaps::default(),
            false,
            width,
        );
        let text = line_text(&modal);
        assert!(text.ends_with("q quit"));
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
        };
        let line = keymap(Phase::IdeaInput, None, caps, false, 200);

        let has_disabled_style = line
            .spans
            .iter()
            .any(|s| s.style.fg == Some(DISABLED_GLYPH) || s.style.fg == Some(DISABLED_ACTION));
        assert!(has_disabled_style, "should have disabled styling");

        let has_enabled_style = line.spans.iter().any(|s| {
            s.style.fg == Some(ENABLED_GLYPH)
                || s.style.fg == Some(ENABLED_GLYPH_PRIMARY)
                || s.style.fg == Some(ENABLED_ACTION)
        });
        assert!(has_enabled_style, "should have enabled styling too");
    }
}
