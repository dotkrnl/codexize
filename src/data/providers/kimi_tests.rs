use super::*;
use serde_json::json;

#[test]
fn usage_limit_reset_time_is_preserved() {
    let data = json!({
        "remaining_percent": 42.0,
        "resets_at": "2026-05-10T12:00:00Z"
    });
    let object = data.as_object().unwrap();

    assert_eq!(usage_remaining_percent(object), Some(42));
    assert_eq!(
        usage_reset_time(object),
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-05-10T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
        )
    );
}

#[test]
fn live_models_from_payload_preserves_shared_and_named_reset_times() {
    let payload = json!({
        "usage": {
            "remaining_percent": 80,
            "resets_at": "2026-05-10T12:00:00Z"
        },
        "limits": [
            {
                "detail": {
                    "name": "kimi-k2.6",
                    "limit": 100,
                    "used": 25,
                    "resetTime": "2026-05-11T00:00:00Z"
                }
            }
        ]
    });

    let models = live_models_from_payload(&payload).unwrap();
    let shared = models
        .iter()
        .find(|model| model.name == SHARED_QUOTA_KEY)
        .expect("shared quota row");
    let named = models
        .iter()
        .find(|model| model.name == "kimi-k2.6")
        .expect("named quota row");

    assert!(shared.quota_resets_at.is_some());
    assert!(named.quota_resets_at.is_some());
}
