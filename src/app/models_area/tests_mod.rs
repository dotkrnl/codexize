use super::*;
use crate::selection::ranking::build_version_index;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget};

// ----- fixtures -----

fn model_with_axis_score(name: &str, axis_score: f64, display_order: usize) -> CachedModel {
    CachedModel {
        vendor: VendorKind::Codex,
        name: name.to_string(),
        overall_score: axis_score,
        current_score: 99.0,
        standard_error: 0.0,
        axes: vec![
            ("codequality".to_string(), axis_score),
            ("correctness".to_string(), axis_score),
            ("debugging".to_string(), axis_score),
            ("safety".to_string(), axis_score),
            ("complexity".to_string(), axis_score),
            ("edgecases".to_string(), axis_score),
            ("contextawareness".to_string(), axis_score),
            ("taskcompletion".to_string(), axis_score),
            ("stability".to_string(), axis_score),
        ],
        axis_provenance: std::collections::BTreeMap::new(),
        quota_percent: Some(100),
        display_order,
        fallback_from: None,
    }
}

fn vendor_model_with_axis_score(
    vendor: VendorKind,
    name: &str,
    axis_score: f64,
    display_order: usize,
) -> CachedModel {
    let mut model = model_with_axis_score(name, axis_score, display_order);
    model.vendor = vendor;
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
    let versions = build_version_index(&models);
    let (lines, mode) = responsive_models_area(
        &models,
        &versions,
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
    let models = vec![model_with_axis_score("gpt-alpha", 1.0, 0)];
    let versions = build_version_index(&models);

    for term_h in [0u16, 5, CHROME_RESERVED_LINES] {
        let (lines, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            120,
            term_h,
            ModelsAreaMode::CompactQuota,
        );
        assert!(lines.is_empty(), "term_h={term_h}");
        // Preserved across transient small terminals so the area does not
        // flicker mode when the terminal grows back.
        assert_eq!(mode, ModelsAreaMode::CompactQuota, "term_h={term_h}");
    }
}

#[test]
fn full_to_compact_uses_strict_threshold() {
    let models = vec![
        model_with_axis_score("gpt-a", 1.0, 0),
        model_with_axis_score("gpt-b", 1.0, 1),
        model_with_axis_score("gpt-c", 1.0, 2),
    ];
    let versions = build_version_index(&models);
    // visible_count >= 3 (all three picked because per-vendor backfill
    // promotes the best-score representative when phases miss).
    let visible_count = visible_models(&models, &versions).len() as u16;

    // Full mode now needs one row of headroom before it stays full.
    let (_, mode) = responsive_models_area(
        &models,
        &versions,
        &[],
        120,
        term_h_for_budget(visible_count),
        ModelsAreaMode::FullTable,
    );
    assert_eq!(mode, ModelsAreaMode::CompactQuota);

    // One extra row is still below the fixed 50-row compact threshold.
    let (_, mode) = responsive_models_area(
        &models,
        &versions,
        &[],
        120,
        term_h_for_budget(visible_count + 1),
        ModelsAreaMode::FullTable,
    );
    assert_eq!(mode, ModelsAreaMode::CompactQuota);

    // At 50 rows, normal hysteresis applies again and this fixture has
    // enough budget to stay full.
    let (_, mode) =
        responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
    assert_eq!(mode, ModelsAreaMode::FullTable);
}

#[test]
fn compact_to_full_requires_extra_line() {
    let models = vec![
        model_with_axis_score("gpt-a", 1.0, 0),
        model_with_axis_score("gpt-b", 1.0, 1),
        model_with_axis_score("gpt-c", 1.0, 2),
    ];
    let versions = build_version_index(&models);
    let visible_count = visible_models(&models, &versions).len() as u16;

    // From compact, models_budget == count is NOT enough (hysteresis +1).
    let (_, mode) = responsive_models_area(
        &models,
        &versions,
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
        &versions,
        &[],
        120,
        term_h_for_budget(visible_count + 1),
        ModelsAreaMode::CompactQuota,
    );
    assert_eq!(mode, ModelsAreaMode::CompactQuota);

    // At 50 rows, normal hysteresis applies again and the larger budget
    // switches compact mode back to full.
    let (_, mode) = responsive_models_area(
        &models,
        &versions,
        &[],
        120,
        50,
        ModelsAreaMode::CompactQuota,
    );
    assert_eq!(mode, ModelsAreaMode::FullTable);
}

#[test]
fn boundary_oscillation_does_not_flip_mode_each_frame() {
    // Spec: "at the boundary, oscillating the height by ±1 across frames
    // must not flip the mode each frame."
    let models = vec![
        model_with_axis_score("gpt-a", 1.0, 0),
        model_with_axis_score("gpt-b", 1.0, 1),
        model_with_axis_score("gpt-c", 1.0, 2),
    ];
    let versions = build_version_index(&models);
    let visible_count = visible_models(&models, &versions).len() as u16;
    let term_at = term_h_for_budget(visible_count);
    let term_below = term_h_for_budget(visible_count - 1);

    // Start on the boundary in full mode. Full→compact now requires
    // one extra row of headroom, so budget == count drops immediately.
    // From compact the +1 hysteresis means count alone never flips us back.
    let mut mode = ModelsAreaMode::FullTable;

    // Frame 1: budget == count, prev=full → compact.
    let (_, m) = responsive_models_area(&models, &versions, &[], 120, term_at, mode);
    assert_eq!(m, ModelsAreaMode::CompactQuota);
    mode = m;

    for _ in 0..6 {
        let (_, m) = responsive_models_area(&models, &versions, &[], 120, term_at, mode);
        assert_eq!(
            m,
            ModelsAreaMode::CompactQuota,
            "+1 hysteresis must hold compact at boundary"
        );
        mode = m;

        let (_, m) = responsive_models_area(&models, &versions, &[], 120, term_below, mode);
        assert_eq!(m, ModelsAreaMode::CompactQuota);
        mode = m;
    }
}

#[test]
fn omit_then_grow_preserves_compact_state() {
    // Omit must not flip the hysteresis state — when the terminal grows
    // back, prev_mode applies as if the omit never happened.
    let models = vec![
        model_with_axis_score("gpt-a", 1.0, 0),
        model_with_axis_score("gpt-b", 1.0, 1),
        model_with_axis_score("gpt-c", 1.0, 2),
    ];
    let versions = build_version_index(&models);
    let visible_count = visible_models(&models, &versions).len() as u16;

    let mut mode = ModelsAreaMode::CompactQuota;
    let (_, m) = responsive_models_area(&models, &versions, &[], 120, /*omit*/ 8, mode);
    assert_eq!(m, ModelsAreaMode::CompactQuota, "omit preserves prev_mode");
    mode = m;

    // Grow back exactly to visible_count budget — still inside the +1
    // hysteresis band, so we stay compact even though the strict
    // threshold technically allows full.
    let (_, m) = responsive_models_area(
        &models,
        &versions,
        &[],
        120,
        term_h_for_budget(visible_count),
        mode,
    );
    assert_eq!(m, ModelsAreaMode::CompactQuota);
}

#[test]
fn mode_default_is_full_table() {
    assert_eq!(ModelsAreaMode::default(), ModelsAreaMode::FullTable);
}

// ----- migrated model_strip_* tests -----

#[test]
fn full_table_bolds_only_phase_rank_one_when_percentages_round_together() {
    let models = vec![
        model_with_axis_score("gpt-alpha", 1.0, 0),
        model_with_axis_score("gpt-beta", 0.996_655, 1),
    ];
    let versions = build_version_index(&models);

    // Width 60 → Ipbr tier (compact single-letter format).
    let (lines, mode) =
        responsive_models_area(&models, &versions, &[], 50, 50, ModelsAreaMode::FullTable);
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
                .contains("beta")
        })
        .expect("beta row should be rendered");
    let beta_line: String = (0..area.width)
        .map(|x| buf.cell((x, beta_y)).map(|c| c.symbol()).unwrap_or(" "))
        .collect();
    let build_col = beta_line
        .rfind("B50")
        .expect("beta build probability should round to B50") as u16;
    let build_cell = buf.cell((build_col, beta_y)).expect("build cell");

    assert!(!build_cell.modifier.contains(Modifier::BOLD));
}

#[test]
fn full_table_truncates_long_names_on_narrow_width() {
    let models = vec![model_with_axis_score(
        "gpt-very-long-model-name-that-will-overflow",
        1.0,
        0,
    )];
    let versions = build_version_index(&models);

    // Width 50 → tier 3 (no probabilities, 2-letter vendor) with a tight
    // name budget so the full name cannot fit.
    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 50, 50, ModelsAreaMode::FullTable);
    let row = full_buffer_line(&lines, 0, 50);

    assert!(
        row.contains("..."),
        "narrow width should truncate name with ellipsis: {row:?}"
    );
    assert!(
        !row.contains("very-long-model-name-that-will-overflow"),
        "full name should not fit: {row:?}"
    );
}

#[test]
fn full_table_drops_probabilities_below_60() {
    // Spec rule: "drop probabilities entirely below ~60 cols". The
    // pre-cutover model_strip used to keep IPBR at width 50 — under the
    // new spec, that whole column collapses.
    let models = vec![model_with_axis_score(
        "gpt-very-long-model-name-that-will-overflow",
        1.0,
        0,
    )];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 50, 50, ModelsAreaMode::FullTable);
    let row = full_buffer_line(&lines, 0, 50);

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
    let models = vec![model_with_axis_score("gpt-alpha", 1.0, 0)];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 80, 50, ModelsAreaMode::FullTable);
    let row = full_buffer_line(&lines, 0, 80);

    // All four phase letters must appear at width 80.
    assert!(
        row.contains('I') && row.contains('P') && row.contains('B') && row.contains('R'),
        "full IPBR at width 80: {row:?}"
    );
}

#[test]
fn full_table_collapses_to_top_rank_only_between_60_and_80() {
    let models = vec![
        model_with_axis_score("gpt-alpha", 100.0, 0),
        model_with_axis_score("gpt-beta", 1.0, 1),
    ];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 70, 50, ModelsAreaMode::FullTable);

    // The full IPBR string would not fit; the top-rank cell does.
    // Each row should contain exactly ONE phase-letter cell (Lxx where
    // L is one of I/P/B/R and xx is two digits). We assert that at
    // least one row carries one such cell, and no row carries the full
    // four-cell IPBR sequence.
    let texts = render_to_text(&lines, 70);
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
fn full_table_truncates_fallback_marker_text_on_narrow_width() {
    // Migrated from model_strip_truncates_fallback_marker_text_on_narrow_width.
    // Under the new spec, the freshness marker DEGRADES rather than
    // truncating: " (new)" → "*" → omitted before the name itself starts
    // ellipsis-truncating. So at narrow widths we expect either "*" or no
    // marker, not "name (...".
    let mut model = model_with_axis_score("gpt-opus-4-1", 1.0, 0);
    model.fallback_from = Some("gpt-4-1".to_string());
    let models = vec![model];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 48, 50, ModelsAreaMode::FullTable);
    let row = full_buffer_line(&lines, 0, 48);

    assert!(
        !row.contains("(...") && !row.contains("(n..."),
        "freshness marker must degrade, not partially truncate: {row:?}"
    );
    assert!(
        row.contains("opus-4-1"),
        "name itself should still appear when budget allows it: {row:?}"
    );
}

#[test]
fn full_table_shows_full_name_on_wide_width() {
    let models = vec![model_with_axis_score("gpt-opus-4-5-20251101", 1.0, 0)];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
    let row = full_buffer_line(&lines, 0, 120);

    assert!(
        row.contains("opus-4-5-20251101"),
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
        VendorKind::Gemini,
        "gemini-3.1-pro-preview",
        1.0,
        0,
    )];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
    let row = full_buffer_line(&lines, 0, 120);

    assert!(
        row.contains("3.1-pro"),
        "short display label should appear: {row:?}"
    );
    assert!(
        !row.contains("3.1-pro-preview"),
        "preview suffix should not appear in display label: {row:?}"
    );
}

#[test]
fn full_table_shows_new_suffix_for_fallback_models_on_wide_width() {
    let mut model = model_with_axis_score("gpt-opus-4-5-20251101", 1.0, 0);
    model.fallback_from = Some("gpt-4-5".to_string());
    let models = vec![model];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
    let row = full_buffer_line(&lines, 0, 120);

    assert!(
        row.contains("opus-4-5-20251101 (new)"),
        "fallback model should show (new) suffix on wide width: {row:?}"
    );
}

#[test]
fn full_table_omits_provenance_labels() {
    let mut model = model_with_axis_score("gpt-alpha", 1.0, 0);
    model.axis_provenance = std::collections::BTreeMap::from([
        ("correctness".to_string(), "suite:hourly".to_string()),
        ("contextawareness".to_string(), "suite:tooling".to_string()),
    ]);
    let models = vec![model];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 200, 50, ModelsAreaMode::FullTable);
    let combined = render_to_text(&lines, 200).join("\n");

    assert!(
        !combined.contains("suite:hourly") && !combined.contains("suite:tooling"),
        "provenance labels must not render: {combined:?}"
    );
}

// ----- name + freshness exact-width contract -----

#[test]
fn format_name_with_freshness_exact_width() {
    // Full name fits — padded to target width.
    let spans = format_name_with_freshness("short", false, 10);
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert_eq!(width, 10);

    // Name + " (new)" fits.
    let spans = format_name_with_freshness("short", true, 15);
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert_eq!(width, 15);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "short (new)    ");

    // Freshness degrades to "*" when " (new)" no longer fits.
    let spans = format_name_with_freshness("gpt-4-turbo", true, 13);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert_eq!(width, 13);
    assert!(
        text.starts_with("gpt-4-turbo*"),
        "freshness should degrade to *: {text:?}"
    );

    // Freshness omitted entirely when even "*" no longer fits.
    let spans = format_name_with_freshness("gpt-4-turbo", true, 11);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert_eq!(width, 11);
    assert_eq!(text, "gpt-4-turbo", "freshness omitted, name fits exactly");

    // Name truncated with ellipsis (no marker since we are in plain mode).
    let spans = format_name_with_freshness("verylongname", false, 10);
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert_eq!(width, 10);
    assert!(spans.iter().any(|s| s.content.contains("...")));
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "verylon...");

    // Very narrow — only ellipsis fits.
    let spans = format_name_with_freshness("x", false, 2);
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert_eq!(width, 2);
}

#[test]
fn format_name_with_freshness_wide_display() {
    // "あ" is 2 wide.
    let spans = format_name_with_freshness("あああ", false, 5); // Total width 6, needs truncation.
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert_eq!(width, 5);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "あ...");

    // Budget 4: visible width 1. "あ" cannot fit, so truncated string is empty. Total width is 3 (just ellipsis).
    let spans = format_name_with_freshness("あああ", false, 4);
    let width: usize = spans.iter().map(|s| s.content.width()).sum();
    assert!(width <= 4);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "...");
}

// ----- compact-quota mode -----

#[test]
fn compact_quota_renders_per_vendor_entries() {
    let models = vec![
        vendor_model_with_axis_score(VendorKind::Kimi, "kimi-1", 50.0, 0),
        vendor_model_with_axis_score(VendorKind::Claude, "claude-1", 50.0, 0),
        vendor_model_with_axis_score(VendorKind::Codex, "gpt-1", 50.0, 0),
        vendor_model_with_axis_score(VendorKind::Gemini, "gemini-1", 50.0, 0),
    ];
    let versions = build_version_index(&models);

    let (lines, mode) = responsive_models_area(
        &models,
        &versions,
        &[],
        120,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    assert_eq!(mode, ModelsAreaMode::CompactQuota);

    let row = full_buffer_line(&lines, 0, 120);
    for tag in ["kimi", "claude", "codex", "gemini"] {
        assert!(row.contains(tag), "missing {tag}: {row:?}");
    }
    assert!(
        row.contains("100%"),
        "100% quota should render verbatim: {row:?}"
    );
}

#[test]
fn compact_quota_keeps_full_vendor_names_below_60() {
    let models = vec![
        vendor_model_with_axis_score(VendorKind::Kimi, "kimi-1", 50.0, 0),
        vendor_model_with_axis_score(VendorKind::Claude, "claude-1", 50.0, 0),
    ];
    let versions = build_version_index(&models);

    let (lines, _) = responsive_models_area(
        &models,
        &versions,
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
fn compact_quota_omits_below_40() {
    let models = vec![vendor_model_with_axis_score(
        VendorKind::Kimi,
        "kimi-1",
        50.0,
        0,
    )];
    let versions = build_version_index(&models);

    let (lines, _) = responsive_models_area(
        &models,
        &versions,
        &[],
        30,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    assert!(lines.is_empty(), "compact line must omit below 40 cols");
}

#[test]
fn compact_quota_failed_fetch_renders_red_dashes() {
    let models = vec![vendor_model_with_axis_score(
        VendorKind::Kimi,
        "kimi-1",
        50.0,
        0,
    )];
    let versions = build_version_index(&models);
    let errors = vec![QuotaError {
        vendor: VendorKind::Kimi,
        message: "boom".to_string(),
    }];

    let (lines, _) = responsive_models_area(
        &models,
        &versions,
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
        VendorKind::Kimi,
        "kimi-1",
        50.0,
        0,
    )];
    let versions = build_version_index(&models);
    let errors = vec![QuotaError {
        vendor: VendorKind::Kimi,
        message: "boom".to_string(),
    }];

    let (lines, _) = responsive_models_area(
        &models,
        &versions,
        &errors,
        120,
        50,
        ModelsAreaMode::FullTable,
    );

    let area = Rect::new(0, 0, 120, lines.len() as u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(lines).render(area, &mut buf);
    let row: String = (0..area.width)
        .map(|x| buf.cell((x, 0)).map(|c| c.symbol()).unwrap_or(" "))
        .collect();

    // Quota cell shows `--%` in red.
    let quota_col = row.find("--%").expect("--% must appear for failed quota") as u16;
    assert_eq!(buf.cell((quota_col, 0)).unwrap().fg, Color::Red);

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
        assert_eq!(buf.cell((col, 0)).unwrap().fg, Color::Red);
    }
}

#[test]
fn full_table_dot_color_tracks_quota_not_score() {
    let mut high_score_no_quota = model_with_axis_score("gpt-alpha", 1.0, 0);
    high_score_no_quota.current_score = 99.0;
    high_score_no_quota.quota_percent = Some(0);
    let mut low_score_full_quota = model_with_axis_score("gpt-beta", 1.0, 1);
    low_score_full_quota.current_score = 1.0;
    low_score_full_quota.quota_percent = Some(100);
    let models = vec![high_score_no_quota, low_score_full_quota];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);

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
        .position(|row| row.contains("alpha"))
        .expect("alpha row");
    let red_col = rows[red_row].find(STATUS_DOT).expect("alpha dot") as u16;
    assert_eq!(buf.cell((red_col, red_row as u16)).unwrap().fg, Color::Red);

    let green_row = rows
        .iter()
        .position(|row| row.contains("beta"))
        .expect("beta row");
    let green_col = rows[green_row].find(STATUS_DOT).expect("beta dot") as u16;
    assert_eq!(
        buf.cell((green_col, green_row as u16)).unwrap().fg,
        probability_color(100, 100)
    );
}

// ----- snapshot matrix (widths and heights from task) -----

fn snapshot_models() -> Vec<CachedModel> {
    // Four vendors so width-tier collapse is observable. Scores stagger
    // so phase ranks are deterministic.
    vec![
        vendor_model_with_axis_score(VendorKind::Kimi, "kimi-k2-thinking", 80.0, 0),
        vendor_model_with_axis_score(VendorKind::Claude, "claude-opus-4-5", 90.0, 0),
        vendor_model_with_axis_score(VendorKind::Codex, "gpt-5-codex", 70.0, 0),
        vendor_model_with_axis_score(VendorKind::Gemini, "gemini-2.5-pro", 60.0, 0),
    ]
}

#[test]
fn snapshot_matrix_widths() {
    let models = snapshot_models();
    let versions = build_version_index(&models);
    let visible_count = visible_models(&models, &versions).len() as u16;

    for &width in &[200u16, 120, 100, 80, 60, 40, 30] {
        // Height is at the 50-row threshold so we exercise full-mode width
        // tiers across the matrix instead of the sub-50 compact override.
        let (lines, mode) = responsive_models_area(
            &models,
            &versions,
            &[],
            width,
            50,
            ModelsAreaMode::FullTable,
        );
        assert_eq!(mode, ModelsAreaMode::FullTable, "width {width}: mode");
        assert_eq!(
            lines.len() as u16,
            visible_count,
            "width {width}: line count must equal visible count"
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
    let versions = build_version_index(&models);
    let visible_count = visible_models(&models, &versions).len() as u16;
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
        let (lines, mode) = responsive_models_area(&models, &versions, &[], 120, case.term_h, prev);
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
    let versions = build_version_index(&models);
    let visible_count = visible_models(&models, &versions).len() as u16;

    let mut prev = ModelsAreaMode::FullTable;

    // Drop into omit (term_h=10 → budget=0) while prev=full.
    let (lines, mode) = responsive_models_area(&models, &versions, &[], 120, 10, prev);
    assert!(lines.is_empty());
    assert_eq!(mode, ModelsAreaMode::FullTable, "omit preserves prev_mode");
    prev = mode;

    // Grow to exactly the boundary; full mode still requires headroom.
    let (_, mode) = responsive_models_area(
        &models,
        &versions,
        &[],
        120,
        term_h_for_budget(visible_count),
        prev,
    );
    assert_eq!(mode, ModelsAreaMode::CompactQuota);

    let (_, mode) =
        responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
    assert_eq!(mode, ModelsAreaMode::FullTable);
}

#[test]
fn snapshot_compact_at_width_60_keeps_full_vendor_labels() {
    let models = snapshot_models();
    let versions = build_version_index(&models);
    let (lines, _) = responsive_models_area(
        &models,
        &versions,
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
    let models = vec![model_with_axis_score("gpt-alpha", 1.0, 0)];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 50, 50, ModelsAreaMode::FullTable);
    let row = full_buffer_line(&lines, 0, 50);

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
    let models = vec![model_with_axis_score("short", 1.0, 0)];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 45, 50, ModelsAreaMode::FullTable);

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
    let models = vec![model_with_axis_score("gpt-alpha", 1.0, 0)];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);

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
    // Acceptance criterion 1c: at width >= 63, IpbrVerbose tier chosen.
    let models = vec![model_with_axis_score("gpt-alpha", 1.0, 0)];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 63, 50, ModelsAreaMode::FullTable);
    let row = full_buffer_line(&lines, 0, 63);

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
}

#[test]
fn term_h_below_50_forces_compact_quota() {
    let models = vec![
        model_with_axis_score("gpt-a", 1.0, 0),
        model_with_axis_score("gpt-b", 1.0, 1),
    ];
    let versions = build_version_index(&models);

    let (lines, mode) =
        responsive_models_area(&models, &versions, &[], 120, 49, ModelsAreaMode::FullTable);
    assert_eq!(
        mode,
        ModelsAreaMode::CompactQuota,
        "term_h=49 must force compact"
    );
    assert!(!lines.is_empty(), "compact quota should produce output");

    let (_, mode) =
        responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
    assert_eq!(
        mode,
        ModelsAreaMode::FullTable,
        "term_h=50 follows normal hysteresis"
    );
}
#[test]
fn full_table_orders_by_build_score_descending() {
    let m1 = vendor_model_with_axis_score(VendorKind::Codex, "gpt-alpha", 0.5, 0);
    let m2 = vendor_model_with_axis_score(VendorKind::Claude, "claude-beta", 0.75, 0);
    let m3 = vendor_model_with_axis_score(VendorKind::Gemini, "gemini-gamma", 1.0, 0);
    let models = vec![m1, m2, m3];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
    let rows = render_to_text(&lines, 120);
    println!("ROWS 3: {:?}", rows);

    assert!(rows[0].contains("gemini"));
    assert!(rows[1].contains("claude"));
    assert!(rows[2].contains("codex"));
}

#[test]
fn full_table_renders_vendor_label_on_every_row() {
    let m1 = vendor_model_with_axis_score(VendorKind::Claude, "claude-alpha", 100.0, 0);
    let m2 = vendor_model_with_axis_score(VendorKind::Claude, "claude-beta", 50.0, 0);
    let models = vec![m1, m2];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
    let rows = render_to_text(&lines, 120);

    assert!(rows[0].contains("claude"));
    assert!(rows[1].contains("claude"));
}

#[test]
fn compact_quota_renders_expanded_quota_when_space_permits() {
    let models = vec![vendor_model_with_axis_score(
        VendorKind::Claude,
        "claude-alpha",
        100.0,
        0,
    )];
    let versions = build_version_index(&models);

    let (lines, _) = responsive_models_area(
        &models,
        &versions,
        &[],
        120,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    let row = full_buffer_line(&lines, 0, 120);
    println!("ROW 2: {:?}", row);

    assert!(row.contains("Quota 100%"));
}

#[test]
fn compact_quota_renders_narrow_quota_when_tight() {
    let models = vec![
        vendor_model_with_axis_score(VendorKind::Kimi, "kimi-1", 50.0, 0),
        vendor_model_with_axis_score(VendorKind::Claude, "claude-1", 50.0, 0),
        vendor_model_with_axis_score(VendorKind::Codex, "gpt-1", 50.0, 0),
        vendor_model_with_axis_score(VendorKind::Gemini, "gemini-1", 50.0, 0),
    ];
    let versions = build_version_index(&models);

    let (lines, _) = responsive_models_area(
        &models,
        &versions,
        &[],
        50,
        term_h_for_budget(1),
        ModelsAreaMode::CompactQuota,
    );
    let row = full_buffer_line(&lines, 0, 50);
    println!("ROW: {:?}", row);

    assert!(!row.contains("Quota"));
    assert!(row.contains("100%"));
}

#[test]
fn full_table_expands_quota_and_phase_labels_when_space_permits() {
    let models = vec![vendor_model_with_axis_score(
        VendorKind::Claude,
        "claude",
        100.0,
        0,
    )];
    let versions = build_version_index(&models);

    let (lines, _) =
        responsive_models_area(&models, &versions, &[], 120, 50, ModelsAreaMode::FullTable);
    let row = full_buffer_line(&lines, 0, 120);

    assert!(row.contains("Quota"));
    assert!(row.contains("Idea"));
}
