use super::*;

#[test]
fn quota_error_summary_single_vendor() {
    let errors = vec![QuotaError {
        vendor: VendorKind::Claude,
        message: "429".to_string(),
    }];
    assert_eq!(
        quota_error_summary(&errors),
        "model refresh: claude quota unavailable"
    );
}

#[test]
fn quota_error_summary_multiple_vendors() {
    let errors = vec![
        QuotaError {
            vendor: VendorKind::Claude,
            message: "429".to_string(),
        },
        QuotaError {
            vendor: VendorKind::Codex,
            message: "503".to_string(),
        },
    ];
    assert_eq!(
        quota_error_summary(&errors),
        "model refresh: claude, codex quotas unavailable"
    );
}
