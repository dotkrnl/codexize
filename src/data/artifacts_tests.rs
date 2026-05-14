use super::*;

#[test]
fn review_scope_rejects_unknown_fields() {
    let error =
        toml::from_str::<ReviewScopeArtifact>("base_sha = \"abc123\"\ndirty_after = true\n")
            .expect_err("unknown review-scope fields must fail");
    assert!(error.to_string().contains("unknown field"));
}
