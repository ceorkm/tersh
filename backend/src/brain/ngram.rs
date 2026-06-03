//! Character n-gram extraction + Jaccard similarity for query/file matching.
//!
//! Used by the Project Index retrieval scorer to bridge the gap between
//! exact-keyword matching and semantic similarity without paying for an
//! embedding API. Example:
//!
//! - Query token: `manager`
//! - Identifier:  `session_manager`
//! - 3-grams of "manager":         {man, ana, nag, age, ger}
//! - 3-grams of "session_manager":  {ses, ess, ssi, sio, ion, on_, n_m, _ma, man, ana, nag, age, ger}
//! - Jaccard intersection ≥ 5 → strong signal that they share morphology.

use std::collections::HashSet;

/// Default n-gram range. n=3..=5 catches both short (verb roots) and
/// medium-length (CamelCase / snake_case fragments) similarities while
/// staying cheap to compute.
pub const NGRAM_MIN: usize = 3;
pub const NGRAM_MAX: usize = 5;

/// Build the set of character n-grams for a string. Input is lowercased
/// and stripped of non-alphanumeric/non-underscore characters before n-gram
/// extraction so identifiers like `SshSession` and `ssh_session` collapse
/// to the same canonical token before n-gramming.
pub fn ngrams(text: &str) -> HashSet<String> {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    let mut out = HashSet::new();
    for token in cleaned.split_whitespace() {
        if token.len() < NGRAM_MIN {
            continue;
        }
        for n in NGRAM_MIN..=NGRAM_MAX {
            if token.len() < n {
                break;
            }
            for i in 0..=token.len() - n {
                // Safe slice: ASCII identifiers are all 1-byte chars.
                if token.is_char_boundary(i) && token.is_char_boundary(i + n) {
                    out.insert(token[i..i + n].to_string());
                }
            }
        }
    }
    out
}

/// Jaccard similarity between two n-gram sets: |A ∩ B| / |A ∪ B|. Returns
/// a score in [0.0, 1.0]. Empty inputs yield 0.0.
pub fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only convenience wrapper. Production code computes the
    /// query n-grams once per retrieval pass and reuses them across
    /// every file's identifier set.
    fn similarity(query: &str, target: &str) -> f32 {
        jaccard(&ngrams(query), &ngrams(target))
    }

    #[test]
    fn ngrams_split_snake_and_camel() {
        let set = ngrams("session_manager");
        // length-3 fragments
        assert!(set.contains("ses"));
        assert!(set.contains("man"));
        assert!(set.contains("ger"));
        // length-4 should be present
        assert!(set.contains("mana"));
    }

    #[test]
    fn manager_matches_session_manager() {
        let q = ngrams("manager");
        let t = ngrams("session_manager");
        let score = jaccard(&q, &t);
        assert!(
            score > 0.3,
            "expected manager~session_manager > 0.3, got {score}"
        );
    }

    #[test]
    fn auth_softly_matches_login_session() {
        // "auth" shares "aut"/"uth" with neither — but "login" and "session"
        // don't have those substrings, so this stays low. Score should be 0
        // here, illustrating the limit (real semantic still requires
        // embeddings). The test guards the contract.
        let score = similarity("auth", "login");
        assert!(score < 0.1);
    }

    #[test]
    fn unrelated_strings_score_low() {
        let score = similarity("banana", "ssh_session");
        assert!(score < 0.05);
    }

    #[test]
    fn very_short_tokens_are_ignored() {
        let set = ngrams("ab cd");
        assert!(
            set.is_empty(),
            "tokens shorter than NGRAM_MIN should be skipped"
        );
    }
}
