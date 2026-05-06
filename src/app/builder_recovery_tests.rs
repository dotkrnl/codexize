#[test]
fn recovery_error_detail_preserves_anyhow_chain() {
    let err = anyhow::anyhow!("outer").context("inner");
    assert_eq!(super::recovery_error_detail(&err), "inner: outer");
}
