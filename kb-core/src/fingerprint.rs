use sha2::{Digest, Sha256};

use crate::types::RawQueryParams;

/// Compute a deterministic fingerprint for a query.
///
/// Normalizes inputs (lowercase, sorted) so that equivalent queries always
/// produce the same hash regardless of parameter ordering.
/// Returns a 16-char hex string (first 8 bytes of SHA-256).
pub fn fingerprint(params: &RawQueryParams) -> String {
    let mut normalized = params.clone();

    // Normalize: lowercase keywords, sort all lists
    normalized.keywords = normalized.keywords.to_lowercase().trim().to_string();
    normalized.tags.sort();
    normalized.tags.iter_mut().for_each(|t| *t = t.to_lowercase());
    normalized.impact.sort();
    normalized
        .impact
        .iter_mut()
        .for_each(|i| *i = i.to_uppercase());
    normalized.protocol_categories.sort();
    normalized
        .protocol_categories
        .iter_mut()
        .for_each(|c| *c = c.to_lowercase());

    let json = serde_json::to_string(&normalized).expect("RawQueryParams must serialize");
    let hash = Sha256::digest(json.as_bytes());

    // 16 hex chars from first 8 bytes
    hash.iter()
        .take(8)
        .map(|b| format!("{:02x}", b))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_params(keywords: &str, tags: &[&str], categories: &[&str]) -> RawQueryParams {
        RawQueryParams {
            keywords: keywords.to_string(),
            impact: vec!["HIGH".to_string(), "MEDIUM".to_string()],
            tags: tags.iter().map(|s| s.to_string()).collect(),
            protocol_categories: categories.iter().map(|s| s.to_string()).collect(),
            min_quality: None,
        }
    }

    #[test]
    fn deterministic_same_input() {
        let p = make_params("reentrancy", &["Reentrancy"], &["Lending"]);
        assert_eq!(fingerprint(&p), fingerprint(&p));
    }

    #[test]
    fn deterministic_different_order() {
        let p1 = make_params("reentrancy", &["ERC4626", "Reentrancy"], &["Lending"]);
        let p2 = make_params("reentrancy", &["Reentrancy", "ERC4626"], &["Lending"]);
        assert_eq!(fingerprint(&p1), fingerprint(&p2));
    }

    #[test]
    fn case_insensitive_keywords() {
        let p1 = make_params("Reentrancy", &[], &[]);
        let p2 = make_params("reentrancy", &[], &[]);
        assert_eq!(fingerprint(&p1), fingerprint(&p2));
    }

    #[test]
    fn case_insensitive_tags() {
        let p1 = make_params("", &["Reentrancy"], &[]);
        let p2 = make_params("", &["reentrancy"], &[]);
        assert_eq!(fingerprint(&p1), fingerprint(&p2));
    }

    #[test]
    fn whitespace_trimmed() {
        let p1 = make_params("  reentrancy  ", &[], &[]);
        let p2 = make_params("reentrancy", &[], &[]);
        assert_eq!(fingerprint(&p1), fingerprint(&p2));
    }

    #[test]
    fn different_queries_different_fingerprints() {
        let p1 = make_params("reentrancy", &[], &[]);
        let p2 = make_params("overflow", &[], &[]);
        assert_ne!(fingerprint(&p1), fingerprint(&p2));
    }

    #[test]
    fn length_is_16_hex_chars() {
        let p = make_params("test", &[], &[]);
        let fp = fingerprint(&p);
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
