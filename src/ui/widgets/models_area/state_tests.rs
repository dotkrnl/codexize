use super::*;

#[test]
fn choose_mode_holds_full_until_budget_tightens() {
    assert_eq!(
        choose_mode(4, 5, ModelsAreaMode::FullTable),
        ModelsAreaMode::FullTable
    );
    assert_eq!(
        choose_mode(4, 4, ModelsAreaMode::FullTable),
        ModelsAreaMode::CompactQuota
    );
}

#[test]
fn name_budget_for_subtracts_fixed_columns() {
    assert_eq!(
        name_budget_for(45, 6, QuotaColumn::Narrow, ProbColumn::TopRank),
        27
    );
}

#[test]
fn verbose_expanded_columns_consume_reserved_width() {
    assert_eq!(
        name_budget_for(140, 6, QuotaColumn::Expanded, ProbColumn::IpbrVerbose),
        79
    );
}

#[test]
fn probability_helpers_clamp_and_dim_zero() {
    assert_eq!(probability_percent(0.0, 100.0), 0);
    assert_eq!(probability_percent(5.0, 4.0), 99);
    assert_eq!(probability_color(0, 100), Color::DarkGray);
}

