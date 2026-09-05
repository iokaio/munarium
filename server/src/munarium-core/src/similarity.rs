// SPDX-License-Identifier: Apache-2.0
//! Ratcliff-Obershelp similarity ratio, matching Python's
//! `difflib.SequenceMatcher.ratio()` semantics (2*M/T over recursive longest
//! matching blocks). The original repetition gate and drift engine both key off
//! this exact ratio, so the port must agree on the algorithm, not just the
//! idea.

/// Similarity in [0, 1] over the byte sequences of the lowercased inputs.
pub fn similarity_ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.to_lowercase().chars().collect();
    let b: Vec<char> = b.to_lowercase().chars().collect();
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    let matches = matching_chars(&a, &b);
    (2.0 * matches as f64) / total as f64
}

fn matching_chars(a: &[char], b: &[char]) -> usize {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (ai, bi, len) = longest_match(a, b);
    if len == 0 {
        return 0;
    }
    len + matching_chars(&a[..ai], &b[..bi]) + matching_chars(&a[ai + len..], &b[bi + len..])
}

/// Longest common substring via the classic DP row sweep.
fn longest_match(a: &[char], b: &[char]) -> (usize, usize, usize) {
    let mut best = (0usize, 0usize, 0usize);
    let mut prev = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        let mut row = vec![0usize; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            if ca == cb {
                let l = prev[j] + 1;
                row[j + 1] = l;
                if l > best.2 {
                    best = (i + 1 - l, j + 1 - l, l);
                }
            }
        }
        prev = row;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_is_one() {
        assert!((similarity_ratio("abc def", "abc def") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn disjoint_is_zero() {
        assert_eq!(similarity_ratio("aaa", "zzz"), 0.0);
    }

    #[test]
    fn case_insensitive() {
        assert!((similarity_ratio("Hello World", "hello world") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn near_duplicate_crosses_repetition_threshold() {
        let a = "The chapter opens with rain on the harbor and the bell ringing twice.";
        let b = "The chapter opens with rain on the harbor and the bell ringing once.";
        assert!(similarity_ratio(a, b) >= 0.9);
    }

    #[test]
    fn different_prose_stays_under_threshold() {
        let a = "The chapter opens with rain on the harbor and the bell ringing twice.";
        let b = "Elsewhere the desert convoy counted its water cans in silence.";
        assert!(similarity_ratio(a, b) < 0.9);
    }
}
