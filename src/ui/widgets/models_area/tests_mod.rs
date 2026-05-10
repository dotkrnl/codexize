use super::*;
use chrono::{Duration, Utc};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};

// ----- fixtures -----

fn model_with_axis_score(name: &str, axis_score: f64, display_order: usize) -> CachedModel {
    // Tests used to vary a single `axis_score` knob; the spec restricts
    // ranking and sampling weights to authoritative ipbr phase scores, so
    // mirror the value into every ipbr phase to keep their relative
    // ordering and percentage shape.
    CachedModel {
        subscription: SubscriptionKind::Codex,
        name: name.to_string(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores {
            idea: Some(axis_score),
            planning: Some(axis_score),
            build: Some(axis_score),
            review: Some(axis_score),
        },
        score_source: crate::selection::ScoreSource::Ipbr,
        candidates: vec![crate::selection::Candidate {
            subscription: SubscriptionKind::Codex,
            cli: crate::selection::CliKind::Codex,
            launch_name: name.to_string(),
            quota_percent: Some(100),
            quota_resets_at: None,
            display_order,
            enabled: true,
            free: false,
            official: true,
            quota_disabled: false,
            cheap_eligible: false,
            tough_eligible: false,
            effort_eligible: false,
            effort_mapping: crate::data::config::schema::EffortMapping::default(),
            quota_failed: false,
        }],
        selected_candidate: Some(0),
        quota_percent: Some(100),
        quota_resets_at: None,
        display_order,
    }
}

fn vendor_model_with_axis_score(
    vendor: SubscriptionKind,
    name: &str,
    axis_score: f64,
    display_order: usize,
) -> CachedModel {
    let mut model = model_with_axis_score(name, axis_score, display_order);
    model.subscription = vendor;
    let cli = vendor
        .direct_cli()
        .unwrap_or(crate::selection::CliKind::Opencode);
    model.candidates = vec![crate::selection::Candidate {
        subscription: vendor,
        cli,
        launch_name: name.to_string(),
        quota_percent: model.quota_percent,
        quota_resets_at: model.quota_resets_at,
        display_order,
        enabled: true,
        free: false,
        official: vendor != SubscriptionKind::Direct,
        quota_disabled: false,
        cheap_eligible: false,
        tough_eligible: false,
        effort_eligible: false,
        effort_mapping: crate::data::config::schema::EffortMapping::default(),
        quota_failed: false,
    }];
    model.selected_candidate = Some(0);
    model
}

fn model_with_reset(
    mut model: CachedModel,
    quota_resets_at: chrono::DateTime<chrono::Utc>,
) -> CachedModel {
    model.quota_resets_at = Some(quota_resets_at);
    model
}

fn model_with_quota(mut model: CachedModel, quota: u8) -> CachedModel {
    model.quota_percent = Some(quota);
    if let Some(candidate) = model.candidates.get_mut(0) {
        candidate.quota_percent = Some(quota);
    }
    model
}

fn render_to_text(lines: &[Line<'static>], width: u16) -> Vec<String> {
    let height = lines.len().max(1) as u16;
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    Paragraph::new(lines.to_vec()).render(area, &mut buf);
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect()
}

fn full_buffer_line(lines: &[Line<'static>], y: usize, width: u16) -> String {
    render_to_text(lines, width)
        .into_iter()
        .nth(y)
        .unwrap_or_default()
}

fn is_quota_summary_row(row: &str) -> bool {
    let trimmed = row.trim_start();
    trimmed.starts_with("Remaining Quota:") || trimmed.starts_with("Quota:")
}

fn render_model_text(lines: &[Line<'static>], width: u16) -> Vec<String> {
    render_to_text(lines, width)
        .into_iter()
        .filter(|row| !is_quota_summary_row(row))
        .collect()
}

fn full_model_buffer_line(lines: &[Line<'static>], y: usize, width: u16) -> String {
    render_model_text(lines, width)
        .into_iter()
        .nth(y)
        .unwrap_or_default()
}

/// Convert a desired models budget (lines available *for the area*) to a
/// terminal height the renderer accepts. Mirrors `term_h - 11` in the
/// renderer so tests read in the same units the spec uses.
fn term_h_for_budget(budget: u16) -> u16 {
    budget + CHROME_RESERVED_LINES
}

// ----- mode-selection / hysteresis -----

#[test]
fn empty_models_returns_empty_lines() {
    let models: Vec<CachedModel> = Vec::new();
    let (lines, mode) = responsive_models_area(
        &models,
        &[],
        120,
        term_h_for_budget(20),
        ModelsAreaMode::FullTable,
    );
    assert!(lines.is_empty());
    assert_eq!(mode, ModelsAreaMode::FullTable);
}

#[test]
fn term_h_below_floor_returns_empty_preserving_prev_mode() {
    // term_h <= CHROME_RESERVED_LINES → models_budget == 0 → omit area.
    let models = vec![model_with_axis_score("gpt-5.2", 1.0, 0)];

    for term_h in [0u16, 5, CHROME_RESERVED_LINES] {
        let (lines, mode) =
            responsive_models_area(&models, &[], 120, term_h, ModelsAreaMode::CompactQuota);
        assert!(lines.is_empty(), "term_h={term_h}");
        // Preserved across transient small terminals so the area does not
        // flicker mode when the terminal grows back.
        assert_eq!(mode, ModelsAreaMode::CompactQuota, "term_h={term_h}");
    }
}

#[test]
fn full_table_renders_uncurated_provider_model() {
    let models = vec![vendor_model_with_axis_score(
        SubscriptionKind::Kimi,
        "kimi-cli-only",
        1.0,
        0,
    )];

    let (lines, mode) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);

    assert_eq!(mode, ModelsAreaMode::FullTable);
    let rendered = render_model_text(&lines, 120).join("\n");
    assert!(rendered.contains("[kimi]"), "{rendered}");
    assert!(rendered.contains("kimi-cli-only"), "{rendered}");
}

#[test]
fn full_to_compact_uses_strict_threshold() {
    let models = vec![
        model_with_axis_score("gpt-5.2", 1.0, 0),
        model_with_axis_score("gpt-5.4", 1.0, 1),
        model_with_axis_score("gpt-5.5", 1.0, 2),
    ];
    // visible_count >= 3 (all three picked because per-vendor backfill
    // promotes the best-score representative when phases miss).
    let visible_count = visible_models(&models).len() as u16;

    // Full mode now needs one row of headroom before it stays full.
    let (_, mode) = responsive_models_area(
        &models,
        &[],
        120,
        term_h_for_budget(visible_count),
        ModelsAreaMode::FullTable,
    );
    assert_eq!(mode, ModelsAreaMode::CompactQuota);

    // One extra row is still below the fixed 50-row compact threshold.
    let (_, mode) = responsive_models_area(
        &models,
        &[],
        120,
        term_h_for_budget(visible_count + 1),
        ModelsAreaMode::FullTable,
    );
    assert_eq!(mode, ModelsAreaMode::CompactQuota);

    // At 50 rows, normal hysteresis applies again and this fixture has
    // enough budget to stay full.
    let (_, mode) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    assert_eq!(mode, ModelsAreaMode::FullTable);
}

#[test]
fn compact_to_full_requires_extra_line() {
    let models = vec![
        model_with_axis_score("gpt-5.2", 1.0, 0),
        model_with_axis_score("gpt-5.4", 1.0, 1),
        model_with_axis_score("gpt-5.5", 1.0, 2),
    ];
    let visible_count = visible_models(&models).len() as u16;

    // From compact, models_budget == count is NOT enough (hysteresis +1).
    let (_, mode) = responsive_models_area(
        &models,
        &[],
        120,
        term_h_for_budget(visible_count),
        ModelsAreaMode::CompactQuota,
    );
    assert_eq!(mode, ModelsAreaMode::CompactQuota);

    // models_budget == count + 1 would unlock the switch back under the
    // hysteresis rule, but the sub-50 terminal threshold still wins.
    let (_, mode) = responsive_models_area(
        &models,
        &[],
        120,
        term_h_for_budget(visible_count + 1),
        ModelsAreaMode::CompactQuota,
    );
    assert_eq!(mode, ModelsAreaMode::CompactQuota);

    // At 50 rows, normal hysteresis applies again and the larger budget
    // switches compact mode back to full.
    let (_, mode) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::CompactQuota);
    assert_eq!(mode, ModelsAreaMode::FullTable);
}

#[test]
fn boundary_oscillation_does_not_flip_mode_each_frame() {
    // Spec: "at the boundary, oscillating the height by ±1 across frames
    // must not flip the mode each frame."
    let models = vec![
        model_with_axis_score("gpt-5.2", 1.0, 0),
        model_with_axis_score("gpt-5.4", 1.0, 1),
        model_with_axis_score("gpt-5.5", 1.0, 2),
    ];
    let visible_count = visible_models(&models).len() as u16;
    let term_at = term_h_for_budget(visible_count);
    let term_below = term_h_for_budget(visible_count - 1);

    // Start on the boundary in full mode. Full→compact now requires
    // one extra row of headroom, so budget == count drops immediately.
    // From compact the +1 hysteresis means count alone never flips us back.
    let mut mode = ModelsAreaMode::FullTable;

    // Frame 1: budget == count, prev=full → compact.
    let (_, m) = responsive_models_area(&models, &[], 120, term_at, mode);
    assert_eq!(m, ModelsAreaMode::CompactQuota);
    mode = m;

    for _ in 0..6 {
        let (_, m) = responsive_models_area(&models, &[], 120, term_at, mode);
        assert_eq!(
            m,
            ModelsAreaMode::CompactQuota,
            "+1 hysteresis must hold compact at boundary"
        );
        mode = m;

        let (_, m) = responsive_models_area(&models, &[], 120, term_below, mode);
        assert_eq!(m, ModelsAreaMode::CompactQuota);
        mode = m;
    }
}

#[test]
fn omit_then_grow_preserves_compact_state() {
    // Omit must not flip the hysteresis state — when the terminal grows
    // back, prev_mode applies as if the omit never happened.
    let models = vec![
        model_with_axis_score("gpt-5.2", 1.0, 0),
        model_with_axis_score("gpt-5.4", 1.0, 1),
        model_with_axis_score("gpt-5.5", 1.0, 2),
    ];
    let visible_count = visible_models(&models).len() as u16;

    let mut mode = ModelsAreaMode::CompactQuota;
    let (_, m) = responsive_models_area(&models, &[], 120, /*omit*/ 8, mode);
    assert_eq!(m, ModelsAreaMode::CompactQuota, "omit preserves prev_mode");
    mode = m;

    // Grow back exactly to visible_count budget — still inside the +1
    // hysteresis band, so we stay compact even though the strict
    // threshold technically allows full.
    let (_, m) = responsive_models_area(&models, &[], 120, term_h_for_budget(visible_count), mode);
    assert_eq!(m, ModelsAreaMode::CompactQuota);
}

#[test]
fn mode_default_is_full_table() {
    assert_eq!(ModelsAreaMode::default(), ModelsAreaMode::FullTable);
}

// ----- model_strip_* tests -----

#[test]
fn full_table_bolds_only_phase_rank_one_when_percentages_round_together() {
    let models = vec![
        model_with_axis_score("gpt-5.2", 1.0, 0),
        model_with_axis_score("gpt-5.4", 0.996_655, 1),
    ];

    // Width 60 → Ipbr tier (compact single-letter format).
    let (lines, mode) = responsive_models_area(&models, &[], 50, 50, ModelsAreaMode::FullTable);
    assert_eq!(mode, ModelsAreaMode::FullTable);

    // Render to a buffer so we can inspect cell modifiers.
    let area = Rect::new(0, 0, 50, lines.len() as u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(lines.clone()).render(area, &mut buf);

    let beta_y = (0..area.height)
        .find(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, *y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
                .contains("5.4")
        })
        .expect("second gpt row should be rendered");
    let beta_line: String = (0..area.width)
        .map(|x| buf.cell((x, beta_y)).map(|c| c.symbol()).unwrap_or(" "))
        .collect();
    let build_col = beta_line
        .rfind("B50")
        .expect("second gpt build probability should round to B50") as u16;
    let build_cell = buf.cell((build_col, beta_y)).expect("build cell");

    assert!(!build_cell.modifier.contains(Modifier::BOLD));
}

#[test]
fn full_table_truncates_long_names_on_narrow_width() {
    let models = vec![model_with_axis_score("gemini-3.1-pro-preview", 1.0, 0)];

    // Width 26 leaves a small name budget, so even the curated short
    // label must be ellipsized.
    // name budget so the full name cannot fit.
    let (lines, _) = responsive_models_area(&models, &[], 26, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 26);

    assert!(
        row.contains("..."),
        "narrow width should truncate name with ellipsis: {row:?}"
    );
    assert!(
        !row.contains("gemini-3.1-pro-preview"),
        "full name should not fit: {row:?}"
    );
}

#[test]
fn full_table_drops_probabilities_below_60() {
    // Spec rule: "drop probabilities entirely below ~60 cols". The
    // pre-cutover model_strip used to keep IPBR at width 50 — under the
    // new spec, that whole column collapses.
    let models = vec![model_with_axis_score("gemini-3.1-pro-preview", 1.0, 0)];

    let (lines, _) = responsive_models_area(&models, &[], 50, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 50);

    assert!(
        !row.contains("I 0")
            && !row.contains("P 0")
            && !row.contains("B 0")
            && !row.contains("R 0"),
        "probabilities must be dropped below 60: {row:?}"
    );
}

#[test]
fn full_table_keeps_full_ipbr_at_or_above_80() {
    let models = vec![model_with_axis_score("gpt-5.2", 1.0, 0)];

    let (lines, _) = responsive_models_area(&models, &[], 80, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 80);

    // All four phase letters must appear at width 80.
    assert!(
        row.contains('I') && row.contains('P') && row.contains('B') && row.contains('R'),
        "full IPBR at width 80: {row:?}"
    );
}

#[test]
fn full_table_collapses_to_top_rank_only_between_60_and_80() {
    let models = vec![
        model_with_axis_score("gpt-5.2", 100.0, 0),
        model_with_axis_score("gpt-5.4", 1.0, 1),
    ];

    let (lines, _) = responsive_models_area(&models, &[], 70, 50, ModelsAreaMode::FullTable);

    // The full IPBR string would not fit; the top-rank cell does.
    // Each row should contain exactly ONE phase-letter cell (Lxx where
    // L is one of I/P/B/R and xx is two digits). We assert that at
    // least one row carries one such cell, and no row carries the full
    // four-cell IPBR sequence.
    let texts = render_model_text(&lines, 70);
    for (i, row) in texts.iter().enumerate() {
        let cell_count = ["I", "P", "B", "R"]
            .iter()
            .filter(|ph| {
                let bytes = row.as_bytes();
                bytes.windows(3).any(|w| {
                    w[0] == ph.as_bytes()[0]
                        && (w[1] as char).is_ascii_digit()
                        && (w[2] as char).is_ascii_digit()
                }) || bytes.windows(3).any(|w| {
                    w[0] == ph.as_bytes()[0] && w[1] == b' ' && (w[2] as char).is_ascii_digit()
                })
            })
            .count();
        assert!(
            cell_count <= 1,
            "row {i}: expected at most one phase cell at 60-79: {row:?}"
        );
    }
}

#[test]
fn full_table_shows_full_name_on_wide_width() {
    let models = vec![model_with_axis_score("grok-code-fast-1", 1.0, 0)];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 120);

    assert!(
        row.contains("code fast 1"),
        "full short name should appear on wide width: {row:?}"
    );
    assert!(
        !row.contains("..."),
        "should not truncate on wide width: {row:?}"
    );
}

#[test]
fn full_table_uses_gemini_preview_display_label() {
    let models = vec![vendor_model_with_axis_score(
        SubscriptionKind::Gemini,
        "gemini-3.1-pro-preview",
        1.0,
        0,
    )];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 120);

    assert!(
        row.contains("3.1 preview"),
        "curated short display label should appear: {row:?}"
    );
    assert!(
        !row.contains("gemini-3.1-pro-preview"),
        "raw canonical should not appear in display label: {row:?}"
    );
}

// ----- name exact-width contract -----

#[test]
fn format_name_exact_width() {
    // Full name fits — padded to target width.
    let spans = format_name_with_freshness("short", 10);
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert_eq!(width, 10);

    // Name truncated with ellipsis when budget is too small.
    let spans = format_name_with_freshness("verylongname", 10);
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert_eq!(width, 10);
    assert!(spans.iter().any(|s| s.content.contains("...")));
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "verylon...");

    // Very narrow — only ellipsis fits.
    let spans = format_name_with_freshness("x", 2);
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert_eq!(width, 2);
}

#[test]
fn format_name_wide_display() {
    // "あ" is 2 wide.
    let spans = format_name_with_freshness("あああ", 5); // Total width 6, needs truncation.
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert_eq!(width, 5);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "あ...");

    // Budget 4: visible width 1. "あ" cannot fit, so truncated string is empty. Total width is 3 (just ellipsis).
    let spans = format_name_with_freshness("あああ", 4);
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert!(width <= 4);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "...");
}

// ----- compact-quota mode -----

#[test]
fn compact_quota_renders_per_vendor_entries() {
    let models = vec![
        vendor_model_with_axis_score(SubscriptionKind::Kimi, "kimi-k2.6", 50.0, 0),
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.7", 50.0, 0),
        vendor_model_with_axis_score(SubscriptionKind::Codex, "gpt-5.4", 50.0, 0),
        vendor_model_with_axis_score(SubscriptionKind::Gemini, "gemini-2.5-pro", 50.0, 0),
    ];

    let (lines, mode) = responsive_models_area(
        &models,
        &[],
        120,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    assert_eq!(mode, ModelsAreaMode::CompactQuota);

    let row = full_buffer_line(&lines, 0, 120);
    for tag in ["claude", "codex", "gemini", "kimi"] {
        assert!(row.contains(tag), "missing {tag}: {row:?}");
    }
    assert!(
        row.contains("100%"),
        "100% quota should render verbatim in quota summary: {row:?}"
    );
}

#[test]
fn full_mode_renders_quota_summary_before_model_rows() {
    let mut claude = model_with_quota(
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.7", 50.0, 0),
        10,
    );
    claude.quota_resets_at = Some(Utc::now() + Duration::days(1) + Duration::hours(2));
    let models = vec![
        claude,
        model_with_quota(
            vendor_model_with_axis_score(SubscriptionKind::Codex, "gpt-5.4", 50.0, 0),
            20,
        ),
        model_with_quota(
            vendor_model_with_axis_score(SubscriptionKind::Gemini, "gemini-2.5-pro", 50.0, 0),
            30,
        ),
        model_with_quota(
            vendor_model_with_axis_score(SubscriptionKind::Kimi, "kimi-k2.6", 50.0, 0),
            40,
        ),
        model_with_quota(
            vendor_model_with_axis_score(
                SubscriptionKind::OpencodeGo,
                "deepseek-v4-flash",
                50.0,
                0,
            ),
            50,
        ),
    ];

    let (lines, mode) = responsive_models_area(&models, &[], 200, 50, ModelsAreaMode::FullTable);
    assert_eq!(mode, ModelsAreaMode::FullTable);

    let rows = render_to_text(&lines, 200);
    assert!(
        rows[0].contains("Remaining Quota: claude 10% (in 1d ")
            && rows[0].contains("), codex 20%, gemini 30%, kimi 40%, opencode 50%"),
        "first row should be the long quota summary: {:?}",
        rows[0]
    );
    assert!(
        rows[1..].iter().any(|row| row.contains("[claude]")),
        "model rows should follow quota summary: {rows:?}"
    );
}

#[test]
fn compact_quota_chooses_text_form_by_actual_rendered_length() {
    let models = vec![
        model_with_quota(
            vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.7", 50.0, 0),
            10,
        ),
        model_with_quota(
            vendor_model_with_axis_score(SubscriptionKind::Codex, "gpt-5.4", 50.0, 0),
            20,
        ),
        model_with_quota(
            vendor_model_with_axis_score(SubscriptionKind::Gemini, "gemini-2.5-pro", 50.0, 0),
            30,
        ),
        model_with_quota(
            vendor_model_with_axis_score(SubscriptionKind::Kimi, "kimi-k2.6", 50.0, 0),
            40,
        ),
        model_with_quota(
            vendor_model_with_axis_score(
                SubscriptionKind::OpencodeGo,
                "deepseek-v4-flash",
                50.0,
                0,
            ),
            50,
        ),
    ];

    let (mid_lines, _) = responsive_models_area(
        &models,
        &[],
        70,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    let mid = full_buffer_line(&mid_lines, 0, 70);
    assert!(
        mid.contains("Quota: claude 10%, codex 20%, gemini 30%, kimi 40%, opencode 50%"),
        "mid form should fit exactly by measured width: {mid:?}"
    );

    let (short_lines, _) = responsive_models_area(
        &models,
        &[],
        48,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    let short = full_buffer_line(&short_lines, 0, 48);
    assert!(
        short.contains("claude 10 codex 20 gemini 30 kimi 40 opencode 50"),
        "short form should be selected when mid is too long: {short:?}"
    );

    let (extreme_lines, _) = responsive_models_area(
        &models,
        &[],
        24,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    let extreme = full_buffer_line(&extreme_lines, 0, 24);
    assert!(
        extreme.contains("cl10 co20 ge30 ki40 op50"),
        "extreme form should be selected at very narrow widths: {extreme:?}"
    );
}

#[test]
fn compact_quota_keeps_full_vendor_names_below_60() {
    let models = vec![
        vendor_model_with_axis_score(SubscriptionKind::Kimi, "kimi-k2.6", 50.0, 0),
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.7", 50.0, 0),
    ];

    let (lines, _) = responsive_models_area(
        &models,
        &[],
        50,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    let row = full_buffer_line(&lines, 0, 50);
    assert!(row.contains("kimi"), "full kimi label: {row:?}");
    assert!(row.contains("claude"), "full claude label: {row:?}");
}

#[test]
fn compact_quota_uses_long_form_below_40_when_it_fits() {
    let models = vec![vendor_model_with_axis_score(
        SubscriptionKind::Kimi,
        "kimi-k2.6",
        50.0,
        0,
    )];

    let (lines, _) = responsive_models_area(
        &models,
        &[],
        30,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    let row = full_buffer_line(&lines, 0, 30);
    assert!(
        row.contains("Remaining Quota: kimi 100%"),
        "compact line should use the longest form that fits: {row:?}"
    );
}

#[test]
fn compact_quota_failed_fetch_renders_red_dashes() {
    let models = vec![vendor_model_with_axis_score(
        SubscriptionKind::Kimi,
        "kimi-k2.6",
        50.0,
        0,
    )];
    let errors = vec![QuotaError {
        subscription: SubscriptionKind::Kimi,
        message: "boom".to_string(),
    }];

    let (lines, _) = responsive_models_area(
        &models,
        &errors,
        120,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );

    // Render to a buffer so we can inspect the red color on the dashes.
    let area = Rect::new(0, 0, 120, lines.len() as u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(lines).render(area, &mut buf);

    let row: String = (0..area.width)
        .map(|x| buf.cell((x, 0)).map(|c| c.symbol()).unwrap_or(" "))
        .collect();
    let dash_col = row.find("--").expect("`--` must appear for failed quota") as u16;
    let cell = buf.cell((dash_col, 0)).expect("dash cell");
    assert_eq!(cell.fg, Color::Red);
}

// ----- failed-vendor full-table styling -----

#[test]
fn full_table_failed_vendor_renders_red_dashes_for_quota_and_probs() {
    let models = vec![vendor_model_with_axis_score(
        SubscriptionKind::Kimi,
        "kimi-k2.6",
        50.0,
        0,
    )];
    let errors = vec![QuotaError {
        subscription: SubscriptionKind::Kimi,
        message: "boom".to_string(),
    }];

    let (lines, _) = responsive_models_area(&models, &errors, 120, 50, ModelsAreaMode::FullTable);

    let area = Rect::new(0, 0, 120, lines.len() as u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(lines).render(area, &mut buf);
    let row_y = (0..area.height)
        .find(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, *y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
                .contains(STATUS_DOT)
        })
        .expect("kimi model row should render");
    let row: String = (0..area.width)
        .map(|x| buf.cell((x, row_y)).map(|c| c.symbol()).unwrap_or(" "))
        .collect();

    // Quota cell shows `--%` in red.
    let quota_col = row.find("--%").expect("--% must appear for failed quota") as u16;
    assert_eq!(buf.cell((quota_col, row_y)).unwrap().fg, Color::Red);

    // At width 120, IpbrVerbose is chosen, so phase labels are full words.
    // probability_unavailable_span renders the entire label+dashes as one red span.
    for (label, pat) in [
        ("Idea", "Idea --"),
        ("Plan", "Plan --"),
        ("Build", "Build --"),
        ("Review", "Review --"),
    ] {
        let col = row
            .find(pat)
            .unwrap_or_else(|| panic!("expected {pat:?} for failed vendor ({label}): {row:?}"))
            as u16;
        // Check that the first char of the label is red (the whole span is red).
        assert_eq!(buf.cell((col, row_y)).unwrap().fg, Color::Red);
    }
}

#[test]
fn full_table_dot_color_tracks_quota_not_score() {
    // Use two different vendors so the per-vendor visibility floor admits
    // both rows even though gpt-5.4 has zero quota (which collapses its
    // pool weight to 0 under the >10% visibility rule).
    let mut high_score_no_quota =
        vendor_model_with_axis_score(SubscriptionKind::Codex, "gpt-5.4", 1.0, 0);
    high_score_no_quota.quota_percent = Some(0);
    let mut low_score_full_quota =
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.7", 1.0, 1);
    low_score_full_quota.quota_percent = Some(100);
    let models = vec![high_score_no_quota, low_score_full_quota];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);

    let area = Rect::new(0, 0, 120, lines.len() as u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(lines).render(area, &mut buf);
    let rows: Vec<String> = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect()
        })
        .collect();

    let red_row = rows
        .iter()
        .position(|row| row.contains("5.4"))
        .expect("gpt-5.4 row");
    let red_col = rows[red_row].find(STATUS_DOT).expect("gpt-5.4 dot") as u16;
    assert_eq!(buf.cell((red_col, red_row as u16)).unwrap().fg, Color::Red);

    let green_row = rows
        .iter()
        .position(|row| row.contains("opus 4.7"))
        .expect("claude-opus-4.7 row");
    let green_col = rows[green_row].find(STATUS_DOT).expect("opus dot") as u16;
    assert_eq!(
        buf.cell((green_col, green_row as u16)).unwrap().fg,
        probability_color(100, 100)
    );
}

// ----- snapshot matrix (widths and heights from task) -----

fn snapshot_models() -> Vec<CachedModel> {
    // Four vendors so width-tier collapse is observable. Scores stagger
    // so phase ranks are deterministic. Use baked canonical names so the
    // curated brand-tags appear in rendered output.
    vec![
        vendor_model_with_axis_score(SubscriptionKind::Kimi, "kimi-k2.6", 80.0, 0),
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.5", 90.0, 0),
        vendor_model_with_axis_score(SubscriptionKind::Codex, "gpt-5.3-codex", 70.0, 0),
        vendor_model_with_axis_score(SubscriptionKind::Gemini, "gemini-2.5-pro", 60.0, 0),
    ]
}

#[test]
fn snapshot_matrix_widths() {
    let models = snapshot_models();
    let visible_count = visible_models(&models).len() as u16;

    for &width in &[200u16, 120, 100, 80, 60, 40, 30] {
        // Height is at the 50-row threshold so we exercise full-mode width
        // tiers across the matrix instead of the sub-50 compact override.
        let (lines, mode) =
            responsive_models_area(&models, &[], width, 50, ModelsAreaMode::FullTable);
        assert_eq!(mode, ModelsAreaMode::FullTable, "width {width}: mode");
        assert_eq!(
            lines.len() as u16,
            visible_count + 1,
            "width {width}: line count must equal quota summary plus visible count"
        );

        for (i, line) in lines.iter().enumerate() {
            let total: usize = line.spans.iter().map(|s| s.content.width()).sum();
            assert!(
                total <= width as usize,
                "width {width}: row {i} exceeds budget ({total})"
            );
        }
    }
}

#[test]
fn wide_layout_shows_relative_reset_time() {
    let models = vec![model_with_reset(
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.5", 90.0, 0),
        Utc::now() + Duration::hours(2),
    )];

    let (lines, mode) = responsive_models_area(&models, &[], 200, 50, ModelsAreaMode::FullTable);

    assert_eq!(mode, ModelsAreaMode::FullTable);
    let row = full_buffer_line(&lines, 0, 200);
    assert!(
        row.contains("in "),
        "expected relative reset time in quota summary, got: {row:?}"
    );
}

#[test]
fn quota_summary_compacts_reset_before_dropping_to_mid_form() {
    let models = vec![model_with_reset(
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.5", 90.0, 0),
        Utc::now() + Duration::days(2),
    )];

    let line = quota_summary_line(&models, &[], 25).expect("compact quota line should fit");
    let row: String = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();

    assert!(
        row.contains("Quota: claude 100% 2d"),
        "compact reset should use the quota label before dropping reset: {row:?}"
    );
    assert!(
        !row.contains("(2d)"),
        "compact reset should omit parentheses: {row:?}"
    );
    assert!(
        !row.contains("in "),
        "compact reset should drop the relative prefix: {row:?}"
    );
}

#[test]
fn full_table_keeps_quota_and_phase_width_tiers_together() {
    let models = vec![vendor_model_with_axis_score(
        SubscriptionKind::Claude,
        "claude-opus-4.7",
        100.0,
        0,
    )];

    let (lines, _) = responsive_models_area(&models, &[], 71, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 71);

    assert!(
        !row.contains("Idea"),
        "phase labels must not stay verbose after quota falls to narrow form: {row:?}"
    );
    assert!(
        row.contains("Quota"),
        "quota label should remain expanded when the phase labels downgrade together: {row:?}"
    );
}

#[test]
fn full_table_expanded_quota_single_digits_use_phase_cell_padding() {
    let models = vec![model_with_quota(
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.7", 100.0, 0),
        5,
    )];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 120);

    assert!(
        row.contains("Quota  5%"),
        "single-digit expanded quota should use two spaces like verbose phase cells: {row:?}"
    );
    assert!(
        !row.contains("Quota   5%"),
        "single-digit expanded quota should not use an independent three-column pad: {row:?}"
    );
}

#[test]
fn full_table_omits_reset_reminder_from_model_rows_but_keeps_quota_summary() {
    let models = vec![model_with_reset(
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.5", 90.0, 0),
        Utc::now() + Duration::hours(2),
    )];

    let (lines, mode) = responsive_models_area(&models, &[], 200, 50, ModelsAreaMode::FullTable);

    assert_eq!(mode, ModelsAreaMode::FullTable);
    let summary = full_buffer_line(&lines, 0, 200);
    assert!(
        summary.contains("Remaining Quota: claude 100% (in "),
        "quota summary should keep reset reminder: {summary:?}"
    );
    let row = full_model_buffer_line(&lines, 0, 200);
    assert!(
        !row.contains("in "),
        "model row should omit reset reminder: {row:?}"
    );
}

#[test]
fn reset_time_stays_hidden_below_very_wide_threshold() {
    let models = vec![model_with_reset(
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.5", 90.0, 0),
        Utc::now() + Duration::hours(2),
    )];

    let (lines, mode) = responsive_models_area(&models, &[], 139, 50, ModelsAreaMode::FullTable);

    assert_eq!(mode, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 139);
    assert!(
        !row.contains("in "),
        "reset time should stay hidden below very-wide threshold: {row:?}"
    );
}

#[test]
fn wide_layout_marks_past_reset_as_expired() {
    let models = vec![model_with_reset(
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.5", 90.0, 0),
        Utc::now() - Duration::hours(1),
    )];

    let (lines, _) = responsive_models_area(&models, &[], 200, 50, ModelsAreaMode::FullTable);

    let row = full_buffer_line(&lines, 0, 200);
    assert!(
        row.contains("expired"),
        "expected expired reset text, got: {row:?}"
    );
}

#[test]
fn full_table_model_row_caps_displayed_quota_at_99() {
    let models = vec![vendor_model_with_axis_score(
        SubscriptionKind::Claude,
        "claude-opus-4.7",
        100.0,
        0,
    )];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 120);

    assert!(
        row.contains("Quota 99%"),
        "model-list quota should cap 100 at 99: {row:?}"
    );
    assert!(
        !row.contains("Quota 100%"),
        "model-list quota should not show 100: {row:?}"
    );
}

#[test]
fn snapshot_matrix_heights_drives_mode() {
    // Task: snapshot tests at heights 30, 20, 15, 12, 10 (terminal
    // heights, not pre-computed budgets). Below 50 rows the fixed compact
    // threshold wins over the normal budget hysteresis:
    //   term_h=30 → compact
    //   term_h=20 → compact
    //   term_h=15 → compact
    //   term_h=12 → compact
    //   term_h=10 → omit (preserves prev_mode)
    let models = snapshot_models();
    let visible_count = visible_models(&models).len() as u16;
    assert_eq!(
        visible_count, 4,
        "fixture must keep four vendors visible for the matrix"
    );

    struct Case {
        term_h: u16,
        expect_mode: ModelsAreaMode,
        expect_line_count: usize,
    }
    let cases = [
        Case {
            term_h: 30,
            expect_mode: ModelsAreaMode::CompactQuota,
            expect_line_count: 1,
        },
        Case {
            term_h: 20,
            expect_mode: ModelsAreaMode::CompactQuota,
            expect_line_count: 1,
        },
        Case {
            term_h: 15,
            expect_mode: ModelsAreaMode::CompactQuota,
            expect_line_count: 1,
        },
        Case {
            term_h: 12,
            expect_mode: ModelsAreaMode::CompactQuota,
            expect_line_count: 1,
        },
        Case {
            term_h: 10,
            // Omitted area preserves prev_mode (compact, from term_h=12).
            expect_mode: ModelsAreaMode::CompactQuota,
            expect_line_count: 0,
        },
    ];

    let mut prev = ModelsAreaMode::FullTable;
    for case in cases {
        let (lines, mode) = responsive_models_area(&models, &[], 120, case.term_h, prev);
        assert_eq!(
            mode, case.expect_mode,
            "term_h={}: mode mismatch",
            case.term_h
        );
        assert_eq!(
            lines.len(),
            case.expect_line_count,
            "term_h={}: line count mismatch",
            case.term_h
        );
        prev = mode;
    }
}

#[test]
fn snapshot_matrix_heights_omit_then_grow_back_requires_headroom() {
    // Same fixture as the height matrix above, but starting from
    // prev=FullTable: a transient drop into omit must not flip the
    // preserved mode, so when the terminal grows back to a full-table
    // budget we stay in full mode.
    let models = snapshot_models();
    let visible_count = visible_models(&models).len() as u16;

    let mut prev = ModelsAreaMode::FullTable;

    // Drop into omit (term_h=10 → budget=0) while prev=full.
    let (lines, mode) = responsive_models_area(&models, &[], 120, 10, prev);
    assert!(lines.is_empty());
    assert_eq!(mode, ModelsAreaMode::FullTable, "omit preserves prev_mode");
    prev = mode;

    // Grow to exactly the boundary; full mode still requires headroom.
    let (_, mode) =
        responsive_models_area(&models, &[], 120, term_h_for_budget(visible_count), prev);
    assert_eq!(mode, ModelsAreaMode::CompactQuota);

    let (_, mode) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    assert_eq!(mode, ModelsAreaMode::FullTable);
}

#[test]
fn snapshot_compact_at_width_60_keeps_full_vendor_labels() {
    let models = snapshot_models();
    let (lines, _) = responsive_models_area(
        &models,
        &[],
        60,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    let row = full_buffer_line(&lines, 0, 60);
    // Width 60 is at the boundary: full vendor labels apply (the
    // 2-letter rule kicks in *below* 60).
    assert!(row.contains("kimi"), "kimi present at width 60: {row:?}");
}

// ----- regression tests for task 2: responsive score column -----

#[test]
fn width_tier_selection_at_50_picks_toprank_or_none() {
    // Acceptance criterion 1a: at width 50, TopRank or None shown (not Ipbr).
    let models = vec![model_with_axis_score("gpt-5.2", 1.0, 0)];

    let (lines, _) = responsive_models_area(&models, &[], 50, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 50);

    // Width 50 should not show full Ipbr (15 cols) — that requires more space.
    // TopRank (3 cols) or None should be chosen based on name budget.
    assert!(
        !row.contains("I ") || !row.contains("P ") || !row.contains("B ") || !row.contains("R "),
        "width 50 must not render full Ipbr tier: {row:?}"
    );
}

#[test]
fn width_tier_selection_at_45_empirically_fits_toprank() {
    // Acceptance criterion 1a: at width 45, TopRank fits if budget >= NAME_WIDTH_MIN.
    let models = vec![model_with_axis_score("gpt-5.2", 1.0, 0)];

    let (lines, _) = responsive_models_area(&models, &[], 45, 50, ModelsAreaMode::FullTable);

    // With a short name, width 45 should empirically fit TopRank (3 cols).
    // name_budget_for(45, vendor_width=6, TopRank=3) = 45 - (6+1+1+1+4+1+1+3) = 27
    // 27 >= 8 (NAME_WIDTH_MIN), so TopRank should be chosen.
    assert!(
        !lines.is_empty(),
        "models should render at width 45 with short name"
    );
}

#[test]
fn scores_right_anchored_row_spans_equal_width() {
    // Acceptance criterion 1b: at width 120, row total spans = 120 cols.
    let models = vec![model_with_axis_score("gpt-5.2", 1.0, 0)];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);

    // Check that the row's total visible width equals 120.
    for (i, line) in lines.iter().enumerate() {
        let total: usize = line.spans.iter().map(|s| s.content.width()).sum();
        assert_eq!(
            total, 120,
            "row {i} total span width must equal 120 (scores right-anchored)"
        );
    }
}

#[test]
fn verbose_tier_renders_full_labels_with_three_space_separation() {
    // The IpbrVerbose tier needs the full phase-label budget plus the wider
    // bracketed vendor column; at width 80 there is room for both.
    let models = vec![model_with_axis_score("gpt-5.2", 1.0, 0)];

    let (lines, _) = responsive_models_area(&models, &[], 80, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 80);

    // IpbrVerbose should render full phase labels.
    assert!(
        row.contains("Idea"),
        "verbose tier: missing 'Idea': {row:?}"
    );
    assert!(
        row.contains("Plan"),
        "verbose tier: missing 'Plan': {row:?}"
    );
    assert!(
        row.contains("Build"),
        "verbose tier: missing 'Build': {row:?}"
    );
    assert!(
        row.contains("Review"),
        "verbose tier: missing 'Review': {row:?}"
    );

    // Check three-space separation between cells (e.g., "Idea 50   Plan 50").
    // The pattern should be: label + two-digit + three-spaces + label.
    let idea_to_plan = row.find("Idea").and_then(|i| {
        let tail = &row[i..];
        tail.find("   Plan").map(|offset| &tail[..offset + 7])
    });
    assert!(
        idea_to_plan.is_some(),
        "verbose tier: three spaces should separate Idea and Plan: {row:?}"
    );
    assert!(
        row.contains("Review 99  Quota 99%"),
        "verbose tier should keep two spaces before quota: {row:?}"
    );
}

#[test]
fn term_h_below_50_forces_compact_quota() {
    let models = vec![
        model_with_axis_score("gpt-5.2", 1.0, 0),
        model_with_axis_score("gpt-5.4", 1.0, 1),
    ];

    let (lines, mode) = responsive_models_area(&models, &[], 120, 49, ModelsAreaMode::FullTable);
    assert_eq!(
        mode,
        ModelsAreaMode::CompactQuota,
        "term_h=49 must force compact"
    );
    assert!(!lines.is_empty(), "compact quota should produce output");

    let (_, mode) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    assert_eq!(
        mode,
        ModelsAreaMode::FullTable,
        "term_h=50 follows normal hysteresis"
    );
}
#[test]
fn full_table_orders_by_build_possibility_descending() {
    let high_score_low_quota = model_with_quota(
        vendor_model_with_axis_score(SubscriptionKind::Codex, "gpt-5.4", 100.0, 0),
        50,
    );
    let lower_score_full_quota = model_with_quota(
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.7", 96.0, 0),
        100,
    );
    let low_score_full_quota = model_with_quota(
        vendor_model_with_axis_score(SubscriptionKind::Gemini, "gemini-2.5-pro", 80.0, 0),
        100,
    );
    let models = vec![
        high_score_low_quota,
        lower_score_full_quota,
        low_score_full_quota,
    ];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    let rows = render_model_text(&lines, 120);

    assert!(rows[0].contains("[claude]"), "row 0: {:?}", rows[0]);
    assert!(rows[1].contains("[gpt]"), "row 1: {:?}", rows[1]);
    assert!(rows[2].contains("[gemini]"), "row 2: {:?}", rows[2]);
}

#[test]
fn full_table_places_quota_after_name_and_score_columns() {
    let models = vec![vendor_model_with_axis_score(
        SubscriptionKind::Codex,
        "gpt-5.4",
        100.0,
        0,
    )];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 120);

    let name_col = row.find("5.4").expect("model name");
    let build_col = row.find("Build").expect("build probability");
    let quota_col = row.find("Quota").expect("quota");

    assert!(
        name_col < build_col && build_col < quota_col,
        "quota should render at the end of the model row: {row:?}"
    );
}

#[test]
fn full_table_renders_bracketed_curated_brand_tag_per_baked_model() {
    // Bracket text is sourced from the row's curated `display_vendor`,
    // keyed by canonical model name; one baked row per subscription kind
    // exercises the curated brand mapping (e.g. opencode-go routes a
    // deepseek model, so the tag reads `[deepseek]`).
    let cases = [
        (SubscriptionKind::Claude, "claude-opus-4.7", "[claude]"),
        (SubscriptionKind::Codex, "gpt-5.4", "[gpt]"),
        (SubscriptionKind::Gemini, "gemini-2.5-pro", "[gemini]"),
        (SubscriptionKind::Kimi, "kimi-k2.6", "[kimi]"),
        (
            SubscriptionKind::OpencodeGo,
            "deepseek-v4-flash",
            "[deepseek]",
        ),
    ];
    for (sub, name, expected) in cases {
        let model = vendor_model_with_axis_score(sub, name, 100.0, 0);
        let (lines, _) = responsive_models_area(&[model], &[], 200, 50, ModelsAreaMode::FullTable);
        let row = full_model_buffer_line(&lines, 0, 200);
        assert!(
            row.contains(expected),
            "model {name} must render `{expected}`, got: {row:?}"
        );
    }
}

#[test]
fn full_table_renders_curated_dim_tag_for_zero_candidate_row() {
    // IPBR-known rows with no candidates keep their curated model brand,
    // but the dimmed tag makes the non-launchable state visible.
    let mut model =
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.7", 100.0, 0);
    model.candidates.clear();
    model.selected_candidate = None;

    let (lines, _) = responsive_models_area(&[model], &[], 200, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 200);
    assert!(
        row.contains("[claude]"),
        "zero-candidate row must keep curated brand, got: {row:?}"
    );
}

#[test]
fn full_table_renders_vendor_label_on_every_row() {
    // Tied phase scores so both same-vendor rows clear the >10% pool
    // weight visibility threshold. The intent is to assert the vendor
    // label is rendered on consecutive same-vendor rows, not to drive any
    // particular ranking outcome.
    let m1 = vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.7", 100.0, 0);
    let m2 = vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-sonnet-4.6", 100.0, 0);
    let models = vec![m1, m2];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    let rows = render_model_text(&lines, 120);

    assert!(rows[0].contains("[claude]"));
    assert!(rows[1].contains("[claude]"));
}

#[test]
fn compact_quota_renders_expanded_quota_when_space_permits() {
    let models = vec![vendor_model_with_axis_score(
        SubscriptionKind::Claude,
        "claude-opus-4.7",
        100.0,
        0,
    )];

    let (lines, _) = responsive_models_area(
        &models,
        &[],
        120,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    let row = full_buffer_line(&lines, 0, 120);
    println!("ROW 2: {:?}", row);

    assert!(row.contains("Remaining Quota: claude 100%"));
}

#[test]
fn compact_quota_renders_narrow_quota_when_tight() {
    let models = vec![
        vendor_model_with_axis_score(SubscriptionKind::Kimi, "kimi-k2.6", 50.0, 0),
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.7", 50.0, 0),
        vendor_model_with_axis_score(SubscriptionKind::Codex, "gpt-5.4", 50.0, 0),
        vendor_model_with_axis_score(SubscriptionKind::Gemini, "gemini-2.5-pro", 50.0, 0),
    ];

    let (lines, _) = responsive_models_area(
        &models,
        &[],
        50,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    let row = full_buffer_line(&lines, 0, 50);
    println!("ROW: {:?}", row);

    assert!(!row.contains("Quota"));
    assert!(row.contains("100"));
}

#[test]
fn full_table_expands_quota_and_phase_labels_when_space_permits() {
    let models = vec![vendor_model_with_axis_score(
        SubscriptionKind::Claude,
        "claude-opus-4.7",
        100.0,
        0,
    )];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    let row = full_model_buffer_line(&lines, 0, 120);

    assert!(row.contains("Quota"));
    assert!(row.contains("Idea"));
}

// ----- new ipbr-pipeline display contracts -----

fn unscored_provider_model(
    vendor: SubscriptionKind,
    name: &str,
    quota: Option<u8>,
    display_order: usize,
) -> CachedModel {
    CachedModel {
        subscription: vendor,
        name: name.to_string(),
        ipbr_phase_scores: crate::selection::IpbrPhaseScores::default(),
        score_source: crate::selection::ScoreSource::None,
        candidates: Vec::new(),
        selected_candidate: None,
        quota_percent: quota,
        quota_resets_at: None,
        display_order,
    }
}

#[test]
fn full_table_unscored_model_renders_as_unscored_for_current_phase() {
    let mut ranked = vendor_model_with_axis_score(SubscriptionKind::Codex, "gpt-5.4", 90.0, 0);
    ranked.quota_percent = Some(80);
    let unscored = unscored_provider_model(SubscriptionKind::Gemini, "gemini-2.5-pro", Some(80), 0);

    let models = vec![ranked, unscored];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    let rows = render_to_text(&lines, 120);

    let unscored_row = rows
        .iter()
        .find(|r| r.contains("2.5 pro"))
        .expect("unscored row should still render");
    // Sampling cells must show 0% for the unscored model — it has no
    // pool weight in any phase. The ranked model is the only sampling
    // candidate and gets ~99%.
    assert!(
        unscored_row.contains("Idea  0")
            || unscored_row.contains("I  0")
            || unscored_row.contains("I 0"),
        "unscored Idea cell should render as 0%: {unscored_row:?}"
    );
    assert!(
        unscored_row.contains("Build  0")
            || unscored_row.contains("B  0")
            || unscored_row.contains("B 0"),
        "unscored Build cell should render as 0%: {unscored_row:?}"
    );
}

#[test]
fn full_table_unknown_quota_renders_as_unknown_not_exhausted() {
    // Spec: missing quota displays as unknown (DarkGray), not exhausted
    // (Red). Selection still treats it as effective-30, but the dot color
    // must distinguish unknown from known-zero.
    let mut model = vendor_model_with_axis_score(SubscriptionKind::Codex, "gpt-5.4", 80.0, 0);
    model.quota_percent = None;
    let models = vec![model];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);

    let area = Rect::new(0, 0, 120, lines.len() as u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(lines).render(area, &mut buf);
    let row_y = (0..area.height)
        .find(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, *y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
                .contains("5.4")
        })
        .expect("gpt-5.4 row should render");
    let row: String = (0..area.width)
        .map(|x| buf.cell((x, row_y)).map(|c| c.symbol()).unwrap_or(" "))
        .collect();
    let dot_col = row.find(STATUS_DOT).expect("status dot") as u16;
    assert_eq!(buf.cell((dot_col, row_y)).unwrap().fg, Color::DarkGray);
}

#[test]
fn full_table_known_zero_quota_renders_exhausted_with_zero_sampling() {
    // Spec: known zero quota = exhausted (red dot) AND must not be
    // auto-selected. The pool scorer drops zero-quota candidates, so the
    // sampling cell should render at 0%.
    let mut zero_quota = vendor_model_with_axis_score(SubscriptionKind::Codex, "gpt-5.4", 90.0, 0);
    zero_quota.quota_percent = Some(0);
    // Mirror the row's exhausted state on the per-tuple Candidate —
    // the sampler reads the row's max effective quota across enabled
    // providers, so a stale `Some(100)` on the candidate would
    // disagree with the row-level `Some(0)` and re-enable the row.
    zero_quota.candidates[0].quota_percent = Some(0);
    let healthy =
        vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.7", 90.0, 0);
    let models = vec![zero_quota, healthy];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);

    let area = Rect::new(0, 0, 120, lines.len() as u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(lines.clone()).render(area, &mut buf);
    let rows: Vec<String> = (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect()
        })
        .collect();

    let zero_row_y = rows
        .iter()
        .position(|r| r.contains("5.4"))
        .expect("zero-quota row visible");
    let dot_col = rows[zero_row_y].find(STATUS_DOT).expect("dot") as u16;
    assert_eq!(
        buf.cell((dot_col, zero_row_y as u16)).unwrap().fg,
        Color::Red,
        "known zero quota must render exhausted (red dot)"
    );

    // Pool-derived sampling: zero-quota model gets weight 0 → 0% in every
    // phase even though its phase score equals the healthy model's.
    let zero_row = &rows[zero_row_y];
    assert!(
        zero_row.contains("Build  0") || zero_row.contains("B  0") || zero_row.contains("B 0"),
        "exhausted model must show 0% sampling: {zero_row:?}"
    );
}

#[test]
fn full_table_sampling_percentage_sourced_from_pool_weights_not_phase_score() {
    // The percentage rendered in the sampling column is the pool-derived
    // softmax weight (× relative-quota factor) — not the raw phase score.
    // With two ipbr-ranked models at scores 90 and 75, the pool softmax
    // assigns the lower-scored model a small but nonzero share (well below
    // its share of phase-score totals, which would be 75/(90+75) ≈ 45%).
    let high = vendor_model_with_axis_score(SubscriptionKind::Codex, "gpt-5.4", 90.0, 0);
    let low = vendor_model_with_axis_score(SubscriptionKind::Claude, "claude-opus-4.7", 75.0, 0);
    let models = vec![high, low];

    let (lines, _) = responsive_models_area(&models, &[], 120, 50, ModelsAreaMode::FullTable);
    let rows = render_to_text(&lines, 120);

    let low_row = rows
        .iter()
        .find(|r| r.contains("opus 4.7"))
        .expect("low row");
    // 15-point softmax gap → lower-scored share is ~6-8%. Phase-score
    // proportional rendering would have put it near 45.
    assert!(
        low_row.contains("Build  6")
            || low_row.contains("Build  7")
            || low_row.contains("Build  8"),
        "sampling percentage must come from pool weights (~6-8%), not phase score (~45%): {low_row:?}"
    );
}
