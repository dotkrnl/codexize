use super::live_summary_advances_content;
#[test]
fn empty_sanitized_payload_is_not_a_content_advance() {
    assert!(!live_summary_advances_content("", ""));
    assert!(!live_summary_advances_content("", "prior"));
}

#[test]
fn duplicate_sanitized_payload_is_not_a_content_advance() {
    assert!(!live_summary_advances_content("same", "same"));
}

#[test]
fn fresh_sanitized_payload_is_a_content_advance() {
    assert!(live_summary_advances_content("first", ""));
    assert!(live_summary_advances_content("second", "first"));
}

