use super::*;
use serde_json::json;

#[test]
fn min_across_windows() {
    let payload = json!({
        "five_hour": { "utilization": 17.0, "resets_at": "2026-04-26T12:00:00Z" },
        "seven_day": { "utilization": 32.0, "resets_at": "2026-04-30T00:00:00Z" },
        "seven_day_sonnet": { "utilization": 7.0, "resets_at": "2026-04-30T00:00:00Z" }
    });
    let models = live_models_from_payload(&payload).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "claude-shared");
    // min(100-17, 100-32, 100-7) = min(83, 68, 93) = 68
    assert_eq!(models[0].quota_percent, Some(68));
    assert_eq!(
        models[0].quota_resets_at,
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-04-30T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        )
    );
}

#[test]
fn extra_usage_excluded() {
    let payload = json!({
        "five_hour": { "utilization": 10.0, "resets_at": "..." },
        "extra_usage": {
            "is_enabled": true,
            "monthly_limit": 5000,
            "used_credits": 3175.0,
            "utilization": 63.5,
            "currency": "USD"
        }
    });
    let models = live_models_from_payload(&payload).unwrap();
    assert_eq!(models[0].quota_percent, Some(90));
}

#[test]
fn null_values_skipped() {
    let payload = json!({
        "five_hour": { "utilization": 20.0, "resets_at": "..." },
        "seven_day_opus": null,
        "iguana_necktie": null
    });
    let models = live_models_from_payload(&payload).unwrap();
    assert_eq!(models[0].quota_percent, Some(80));
}

#[test]
fn all_null_returns_error() {
    let payload = json!({
        "seven_day_opus": null,
        "iguana_necktie": null
    });
    assert!(live_models_from_payload(&payload).is_err());
}

#[test]
fn zero_utilization_contributes_100() {
    let payload = json!({
        "five_hour": { "utilization": 50.0 },
        "seven_day_omelette": { "utilization": 0.0, "resets_at": null }
    });
    let models = live_models_from_payload(&payload).unwrap();
    // min(50, 100) = 50
    assert_eq!(models[0].quota_percent, Some(50));
}
