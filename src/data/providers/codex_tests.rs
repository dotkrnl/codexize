use super::*;
use serde_json::json;

#[test]
fn record_rate_limit_keeps_reset_for_limiting_window() {
    let payload = json!({
        "rate_limit": {
            "primary_window": {
                "used_percent": 40.0,
                "resets_at": "2026-05-10T12:00:00Z"
            },
            "secondary_window": {
                "used_percent": 75.0,
                "resets_at": "2026-05-11T00:00:00Z"
            }
        }
    });
    let mut quotas = BTreeMap::<String, ModelQuota>::new();
    record_rate_limit(&mut quotas, "gpt-5.4", &payload["rate_limit"]);

    let quota = quotas.get("gpt-5.4").expect("quota recorded");
    assert_eq!(quota.remaining_min, Some(25.0));
    assert_eq!(
        quota.resets_at,
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-05-11T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        )
    );
}

#[test]
fn record_rate_limit_accepts_camel_case_reset_time() {
    let payload = json!({
        "primary_window": {
            "usedPercent": 10.0,
            "resetTime": "2026-05-10T12:00:00Z"
        }
    });
    let mut quotas = BTreeMap::<String, ModelQuota>::new();
    record_rate_limit(&mut quotas, "gpt-5.4", &payload);

    assert!(quotas["gpt-5.4"].resets_at.is_some());
}
