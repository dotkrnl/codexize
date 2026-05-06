use super::*;

#[test]
fn session_artifact_round_trips_as_toml() {
    let artifact = SessionArtifact {
        id: "20260424-233547".to_string(),
        created_at: "2026-04-24T23:35:47Z".to_string(),
        operator: "dotkrnl".to_string(),
        status: "running".to_string(),
    };

    let encoded = toml::to_string(&artifact).expect("encode session");
    assert!(encoded.contains("id = \"20260424-233547\""));
    let decoded: SessionArtifact = toml::from_str(&encoded).expect("decode session");
    assert_eq!(decoded.id, artifact.id);
}

#[test]
fn round_artifacts_round_trip_as_toml() {
    let review_scope = ReviewScopeArtifact {
        base_sha: "abc123".to_string(),
    };
    let encoded = toml::to_string(&review_scope).expect("encode review scope");
    let decoded: ReviewScopeArtifact = toml::from_str(&encoded).expect("decode review scope");
    assert_eq!(decoded.base_sha, "abc123");
    assert!(!encoded.contains("dirty_after"));

    let review = ReviewArtifact {
        status: ReviewStatus::Revise,
        summary: "needs split".to_string(),
        feedback: vec!["split the work".to_string()],
        new_tasks: vec![TaskArtifact {
            id: 2,
            title: "Split work".to_string(),
            description: "Do less at once.".to_string(),
            test: "cargo test".to_string(),
            estimated_tokens: 1000,
            spec_refs: vec![],
            plan_refs: vec![],
        }],
    };
    let encoded = toml::to_string(&review).expect("encode review");
    let decoded: ReviewArtifact = toml::from_str(&encoded).expect("decode review");
    assert_eq!(decoded.status, ReviewStatus::Revise);
    assert_eq!(decoded.new_tasks.len(), 1);
}

#[test]
fn json_artifacts_are_rejected_by_toml_parsers() {
    let json = r#"{ "status": "approved", "summary": "old json" }"#;
    let error = toml::from_str::<ReviewArtifact>(json).expect_err("json must fail");
    assert!(error.to_string().contains("TOML"));
}

#[test]
fn review_scope_ignores_dirty_after_for_legacy_files() {
    let decoded: ReviewScopeArtifact =
        toml::from_str("base_sha = \"abc123\"\ndirty_after = true\n")
            .expect("decode legacy review scope");
    assert_eq!(decoded.base_sha, "abc123");
}
