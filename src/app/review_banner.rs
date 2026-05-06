// Review-banner helpers: a literal banner prepended to spec/plan files when
// they're auto-opened for review and stripped (by exact match) once the
// editor closes. Kept in its own module so the literal stays pinned and the
// file IO is easy to find.
/// Prepended to spec/plan files when they're auto-opened for review, then
/// stripped (by exact match) once the editor closes. Keep the literal stable
/// — `strip_review_banner` removes only this exact string, so any drift
/// would leave the banner sitting in the file forever.
pub(crate) const REVIEW_BANNER: &str = "\
████████████████████████████████████████████████████████████████████████
██                                                                    ██
██   PLEASE REVIEW THIS DOCUMENT, THEN CLOSE THE EDITOR TO CONTINUE.  ██
██                                                                    ██
██   This banner is auto-inserted on open and removed on close —      ██
██   leave it in place; it will not appear in the saved artifact.     ██
██                                                                    ██
████████████████████████████████████████████████████████████████████████
";
pub(crate) fn prepend_review_banner(path: &std::path::Path) -> bool {
    let Ok(existing) = std::fs::read_to_string(path) else {
        return false;
    };
    if existing.contains(REVIEW_BANNER) {
        return false;
    }
    let mut combined = String::with_capacity(REVIEW_BANNER.len() + existing.len());
    combined.push_str(REVIEW_BANNER);
    combined.push_str(&existing);
    std::fs::write(path, combined).is_ok()
}
pub(crate) fn strip_review_banner(path: &std::path::Path) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path)?;
    let Some(idx) = existing.find(REVIEW_BANNER) else {
        return Ok(());
    };
    let mut stripped = String::with_capacity(existing.len() - REVIEW_BANNER.len());
    stripped.push_str(&existing[..idx]);
    stripped.push_str(&existing[idx + REVIEW_BANNER.len()..]);
    std::fs::write(path, stripped)
}
