use super::*;

#[tokio::test]
async fn load_quota_maps_for_empty_vendor_set_skips_all_probes() {
    let (maps, reset_maps, errors) = load_quota_maps_for_async([]).await;

    assert!(maps.is_empty());
    assert!(reset_maps.is_empty());
    assert!(errors.is_empty());
}

#[test]
fn kimi_quota_takes_min_across_windows() {
    let mapped = live_map_kimi(vec![
        LiveModel {
            name: "kimi-k1.6".to_string(),
            quota_percent: Some(80),
            quota_resets_at: None,
        },
        LiveModel {
            name: "kimi-k2".to_string(),
            quota_percent: Some(20),
            quota_resets_at: None,
        },
    ]);

    let key = providers::kimi::SHARED_QUOTA_KEY;
    assert_eq!(
        mapped.0.get(key),
        Some(&Some(20)),
        "should use the minimum quota across all windows"
    );
    assert_eq!(mapped.1.get(key), Some(&None));
}

#[test]
fn kimi_quota_returns_none_when_all_missing() {
    let mapped = live_map_kimi(vec![LiveModel {
        name: "kimi-k1.6".to_string(),
        quota_percent: None,
        quota_resets_at: None,
    }]);

    assert_eq!(
        mapped.0.get(providers::kimi::SHARED_QUOTA_KEY),
        Some(&None),
        "should return None when no quotas are available"
    );
}

#[test]
fn live_map_direct_injects_known_gemini_quota_names() {
    let mapped = live_map_direct(vec![LiveModel {
        name: "gemini-3-pro-preview".to_string(),
        quota_percent: Some(42),
        quota_resets_at: None,
    }]);

    for name in [
        "gemini-3.1-pro-preview",
        "gemini-3-pro-preview",
        "gemini-3-flash-preview",
        "gemini-2.5-pro",
        "gemini-2.5-flash",
    ] {
        assert_eq!(mapped.0.get(name), Some(&Some(42)), "{name} missing");
        assert_eq!(mapped.1.get(name), Some(&None), "{name} reset missing");
    }
}
