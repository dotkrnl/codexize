//! Shared string-distance helpers used by the loader's unknown-key
//! suggestions and the mutate module's error messages.
//!
//! `levenshtein` and `nearest` were duplicated in `loader.rs` and
//! `mutate.rs` before extraction.

/// Compute the Levenshtein edit distance between two strings.
pub(crate) fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Compute the closest match in `candidates` to `target`, returning
/// `None` when nothing is within edit distance ≤ `max_distance`.
pub(crate) fn nearest(target: &str, candidates: &[&str], max_distance: usize) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for c in candidates {
        let d = levenshtein(target, c);
        if d <= max_distance && best.is_none_or(|(b, _)| d < b) {
            best = Some((d, c));
        }
    }
    best.map(|(_, s)| s.to_string())
}
