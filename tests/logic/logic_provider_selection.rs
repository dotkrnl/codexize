use codexize::data::config::schema::EffortMapping;
use codexize::logic::selection::{
    assemble::select_candidate_index,
    types::{Candidate, CliKind, SubscriptionKind},
};

fn sample_candidate(
    name: &str,
    subscription: SubscriptionKind,
    official: bool,
    free: bool,
) -> Candidate {
    Candidate {
        subscription,
        cli: CliKind::Claude,
        launch_name: name.to_string(),
        quota_percent: Some(100),
        quota_resets_at: None,
        display_order: 0,
        enabled: true,
        free,
        official,
        quota_disabled: false,
        cheap_eligible: true,
        tough_eligible: true,
        effort_eligible: true,
        effort_mapping: EffortMapping::default(),
        quota_failed: false,
    }
}

#[test]
fn provider_selection_ladder_prefers_free_over_official() {
    let mut official = sample_candidate("official", SubscriptionKind::Claude, true, false);
    official.quota_percent = Some(100);
    let mut free = sample_candidate("free", SubscriptionKind::OpencodeGo, false, true);
    free.quota_percent = Some(100);

    let candidates = vec![official, free];
    let selected = select_candidate_index(&candidates).expect("should pick one");
    assert_eq!(candidates[selected].launch_name, "free");
}

#[test]
fn provider_selection_ladder_prefers_official_with_good_quota_over_no_quota() {
    let mut official = sample_candidate("official", SubscriptionKind::Claude, true, false);
    official.quota_percent = Some(50);
    let mut no_quota = sample_candidate("no-quota", SubscriptionKind::OpencodeGo, false, false);
    no_quota.quota_disabled = true;

    let candidates = vec![official, no_quota];
    let selected = select_candidate_index(&candidates).expect("should pick one");
    assert_eq!(candidates[selected].launch_name, "official");
}

#[test]
fn provider_selection_ladder_prefers_no_quota_over_official_with_low_quota() {
    let mut official = sample_candidate("official", SubscriptionKind::Claude, true, false);
    official.quota_percent = Some(10);
    let mut no_quota = sample_candidate("no-quota", SubscriptionKind::OpencodeGo, false, false);
    no_quota.quota_disabled = true;

    let candidates = vec![official, no_quota];
    let selected = select_candidate_index(&candidates).expect("should pick one");
    // Now picks no-quota because official is throttled (<=20).
    assert_eq!(candidates[selected].launch_name, "no-quota");
}

#[test]
fn disabling_baked_provider_removes_it_from_selection() {
    let mut official = sample_candidate("official", SubscriptionKind::Claude, true, false);
    official.enabled = false;
    let mut addition = sample_candidate("addition", SubscriptionKind::OpencodeGo, false, false);
    addition.enabled = true;

    let candidates = vec![official, addition];
    let selected = select_candidate_index(&candidates).expect("should pick one");
    assert_eq!(candidates[selected].launch_name, "addition");
}
