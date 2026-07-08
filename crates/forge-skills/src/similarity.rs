//! Content-similarity helpers for the Skill Curator.
//!
//! Everything here is pure — no I/O, no config, no dependencies on the
//! runtime. The Curator turns these scores into decisions; keeping the math
//! separate keeps it easy to unit test and to reason about.
//!
//! # Two kinds of similarity
//!
//! - **Name similarity** — Jaro-Winkler on the front-matter `name` field.
//!   Lives next to the Curator in `forge-runtime` for now (historical) and
//!   is not re-exported here.
//! - **Body similarity** — Jaccard on token shingles of the Markdown body.
//!   Robust to reordering of paragraphs; ignores whitespace + case; ignores
//!   trivial punctuation. Good for "these two skills teach the same thing".
//!
//! # Merge helper
//!
//! When bodies overlap moderately (e.g. 0.60..0.85), the Curator proposes a
//! merged skill instead of archiving one. `merge_bodies` produces a
//! deterministic combined body by keeping the longer one and appending
//! non-duplicate paragraphs from the shorter, headed by a merge marker so a
//! human reviewer can see where the seams are.

use std::collections::HashSet;

/// Tokenize a body for similarity comparison. Lowercases, splits on
/// non-alphanumeric, drops tokens shorter than 3 chars (kills prepositions
/// / punctuation noise), returns owned `String`s so callers can shingle.
pub fn tokenize(body: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(body.len() / 6);
    let mut current = String::new();
    for c in body.chars() {
        if c.is_alphanumeric() {
            current.extend(c.to_lowercase());
        } else if !current.is_empty() {
            if current.len() >= 3 { out.push(std::mem::take(&mut current)); }
            else                  { current.clear(); }
        }
    }
    if current.len() >= 3 { out.push(current); }
    out
}

/// N-gram shingles over tokens. `n=3` is a good default: catches phrase
/// overlap without exploding cardinality for short bodies.
pub fn shingles(tokens: &[String], n: usize) -> HashSet<String> {
    if tokens.len() < n {
        // Fall back to individual tokens so tiny bodies still get compared.
        return tokens.iter().cloned().collect();
    }
    tokens.windows(n).map(|w| w.join(" ")).collect()
}

/// Jaccard similarity |A ∩ B| / |A ∪ B|. Returns 1.0 when both sets are
/// empty (vacuously identical), 0.0 when only one is empty.
pub fn jaccard<T: std::hash::Hash + Eq>(a: &HashSet<T>, b: &HashSet<T>) -> f64 {
    if a.is_empty() && b.is_empty() { return 1.0; }
    if a.is_empty() || b.is_empty() { return 0.0; }
    let intersection = a.intersection(b).count() as f64;
    let union        = a.union(b).count() as f64;
    intersection / union
}

/// One-shot body-similarity score using 3-gram shingles + Jaccard.
pub fn body_similarity(a: &str, b: &str) -> f64 {
    let sa = shingles(&tokenize(a), 3);
    let sb = shingles(&tokenize(b), 3);
    jaccard(&sa, &sb)
}

/// Subset ratio: fraction of the SMALLER skill's shingles present in the
/// larger. Detects "skill A is a proper subset of skill B", which body
/// Jaccard would understate (Jaccard punishes size mismatch even when the
/// smaller is fully contained).
///
/// Returns a value in [0.0, 1.0]. 1.0 means every shingle of the smaller
/// body appears in the larger body — a strong "one is contained in the
/// other" signal.
pub fn subset_ratio(a: &str, b: &str) -> f64 {
    let sa = shingles(&tokenize(a), 3);
    let sb = shingles(&tokenize(b), 3);
    if sa.is_empty() || sb.is_empty() { return 0.0; }
    let (smaller, larger) = if sa.len() <= sb.len() { (&sa, &sb) } else { (&sb, &sa) };
    let hits = smaller.iter().filter(|s| larger.contains(*s)).count() as f64;
    hits / smaller.len() as f64
}

/// Deterministic body merge: keep the longer body verbatim, then append any
/// paragraph from the shorter body that isn't already substring-present.
/// Emits a `<!-- merged: … -->` marker so a human reviewer can see the seam.
///
/// The reviewer is expected to tighten wording before promoting — this is
/// a scaffold, not a polished merge.
pub fn merge_bodies(a: &str, b: &str) -> String {
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = String::with_capacity(long.len() + short.len() + 64);
    out.push_str(long.trim());
    out.push_str("\n\n<!-- merged: appended paragraphs from a near-duplicate skill; edit before promoting -->\n\n");
    let long_lower = long.to_lowercase();
    let mut appended = 0usize;
    for para in short.split("\n\n") {
        let p = para.trim();
        if p.is_empty() { continue; }
        // Skip paragraphs already substring-present in the longer body.
        if long_lower.contains(&p.to_lowercase()) { continue; }
        out.push_str(p);
        out.push_str("\n\n");
        appended += 1;
    }
    if appended == 0 {
        // Nothing to add — the shorter body is fully contained. Return the
        // long body alone (no merge marker) so callers can detect this by
        // comparing to the input.
        return long.trim().to_string();
    }
    out.trim_end().to_string() + "\n"
}

/// Union two comma/space-separated tag-like fields (tools, keywords).
/// Preserves left order, then appends unseen values from right.
pub fn union_dedup<S: AsRef<str> + Clone>(left: &[S], right: &[S]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(left.len() + right.len());
    for item in left.iter().chain(right.iter()) {
        let s = item.as_ref().trim().to_string();
        if s.is_empty() { continue; }
        if seen.insert(s.clone()) { out.push(s); }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_drops_short_and_lowercases() {
        let t = tokenize("Run `cargo test` in the crate! Also lint.");
        assert!(t.contains(&"cargo".to_string()));
        assert!(t.contains(&"test".to_string()));
        assert!(t.contains(&"crate".to_string()));
        assert!(t.contains(&"also".to_string()));
        assert!(t.contains(&"lint".to_string()));
        // 2-letter tokens dropped.
        assert!(!t.contains(&"in".to_string()));
    }

    #[test]
    fn body_similarity_identical_is_one() {
        let a = "Run cargo test in the crate root. Report failures. Retry once.";
        assert!((body_similarity(a, a) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn body_similarity_disjoint_is_zero() {
        let a = "Run cargo test in the crate root. Report failures.";
        let b = "Compose a haiku about oranges falling from a tree at dusk.";
        assert!(body_similarity(a, b) < 0.05);
    }

    #[test]
    fn body_similarity_paraphrase_is_moderate() {
        // Same intent, different wording. Should be > 0 but well below 1.
        let a = "Run cargo test to verify the crate compiles and passes.";
        let b = "Run cargo test to verify the crate compiles and passes cleanly.";
        let s = body_similarity(a, b);
        assert!(s > 0.5, "expected high similarity, got {s}");
        assert!(s < 1.0, "expected < 1.0, got {s}");
    }

    #[test]
    fn subset_ratio_detects_containment() {
        let small = "Run cargo test in the crate root and report failures.";
        let large = format!("{small}\n\nAlso run cargo clippy and fix warnings before submitting a pull request for review.");
        let r = subset_ratio(small, &large);
        assert!(r > 0.9, "expected subset ratio near 1.0, got {r}");
    }

    #[test]
    fn merge_bodies_appends_nonoverlapping_paragraph() {
        let a = "Step one: run cargo test.\n\nStep two: report failures.";
        let b = "Step one: run cargo test.\n\nStep three: file an issue if new failures appear.";
        let m = merge_bodies(a, b);
        assert!(m.contains("Step two"));
        assert!(m.contains("Step three"));
        assert!(m.contains("<!-- merged:"));
    }

    #[test]
    fn merge_bodies_when_contained_returns_long_only() {
        let short = "Run cargo test.";
        let long  = "Run cargo test.\n\nReport failures with stack traces.";
        let m = merge_bodies(short, long);
        assert!(!m.contains("<!-- merged:"), "should skip marker when nothing new appended");
        assert!(m.contains("Report failures"));
    }

    #[test]
    fn union_dedup_preserves_order_and_dedupes() {
        let a = vec!["fs.read", "shell.run"];
        let b = vec!["shell.run", "http.get"];
        let out = union_dedup(&a, &b);
        assert_eq!(out, vec!["fs.read", "shell.run", "http.get"]);
    }

    #[test]
    fn jaccard_empty_sets() {
        let a: HashSet<String> = HashSet::new();
        let b: HashSet<String> = HashSet::new();
        assert!((jaccard(&a, &b) - 1.0).abs() < 1e-9);
        let mut c = HashSet::new();
        c.insert("x".to_string());
        assert_eq!(jaccard(&a, &c), 0.0);
    }
}
