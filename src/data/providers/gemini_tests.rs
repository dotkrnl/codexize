use super::*;
use serde_json::json;

#[test]
fn duplicate_buckets_are_min_d() {
    let payload = json!({
        "buckets": [
            { "modelId": "gemini-2.5-pro", "remainingFraction": 0.80 },
            { "modelId": "gemini-2.5-pro", "remainingFraction": 0.30 }
        ]
    });
    let models = live_models_from_payload(&payload).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "gemini-2.5-pro");
    // MIN(80, 30) = 30
    assert_eq!(models[0].quota_percent, Some(30));
}

#[test]
fn single_bucket_preserved() {
    let payload = json!({
        "buckets": [
            { "modelId": "gemini-2.5-flash", "remainingFraction": 0.47 }
        ]
    });
    let models = live_models_from_payload(&payload).unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].name, "gemini-2.5-flash");
    assert_eq!(models[0].quota_percent, Some(47));
}

#[test]
fn missing_buckets_returns_error() {
    let payload = json!({ "buckets": [] });
    assert!(live_models_from_payload(&payload).is_err());
}
