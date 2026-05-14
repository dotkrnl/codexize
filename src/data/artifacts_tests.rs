use super::*;

#[test]
fn json_artifacts_are_rejected_by_toml_parsers() {
    let json = r#"{ "status": "approved", "summary": "old json" }"#;
    let error = toml::from_str::<ReviewArtifact>(json).expect_err("json must fail");
    assert!(error.to_string().contains("TOML"));
}

#[test]
fn review_scope_rejects_unknown_fields() {
    let error =
        toml::from_str::<ReviewScopeArtifact>("base_sha = \"abc123\"\ndirty_after = true\n")
            .expect_err("unknown review-scope fields must fail");
    assert!(error.to_string().contains("unknown field"));
}
