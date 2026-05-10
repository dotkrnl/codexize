use super::*;
use serde_json::json;

#[test]
fn duplicate_buckets_are_min_d() {
    let payload = json!({
        "buckets": [
            {
                "modelId": "gemini-2.5-pro",
                "remainingFraction": 0.80,
                "resetTime": "2026-05-10T12:00:00Z"
            },
            {
                "modelId": "gemini-2.5-pro",
                "remainingFraction": 0.30,
                "resetTime": "2026-05-11T00:00:00Z"
            }
        ]
    });
    let models = live_models_from_payload(&payload).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "gemini-2.5-pro");
    // MIN(80, 30) = 30
    assert_eq!(models[0].quota_percent, Some(30));
    assert_eq!(
        models[0].quota_resets_at,
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-05-11T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        )
    );
}

#[test]
fn single_bucket_preserved() {
    let payload = json!({
        "buckets": [
            {
                "modelId": "gemini-2.5-flash",
                "remainingFraction": 0.47,
                "reset_time": "2026-05-10T12:00:00Z"
            }
        ]
    });
    let models = live_models_from_payload(&payload).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "gemini-2.5-flash");
    assert_eq!(models[0].quota_percent, Some(47));
    assert!(models[0].quota_resets_at.is_some());
}

#[test]
fn missing_buckets_returns_error() {
    let payload = json!({ "buckets": [] });
    assert!(live_models_from_payload(&payload).is_err());
}
