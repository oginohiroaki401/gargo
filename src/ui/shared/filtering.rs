pub fn fzf_style_match(haystack: &str, needle: &str) -> Option<(i32, Vec<usize>)> {
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }

    let haystack_chars: Vec<char> = haystack.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();

    let mut positions = Vec::with_capacity(needle_chars.len());
    let mut hay_idx = 0usize;
    for &needle_ch in &needle_chars {
        let needle_lower = needle_ch.to_lowercase().next().unwrap_or(needle_ch);
        let mut found = false;
        while hay_idx < haystack_chars.len() {
            let hay = haystack_chars[hay_idx];
            let hay_lower = hay.to_lowercase().next().unwrap_or(hay);
            if hay_lower == needle_lower {
                positions.push(hay_idx);
                hay_idx += 1;
                found = true;
                break;
            }
            hay_idx += 1;
        }
        if !found {
            return None;
        }
    }

    let mut score = 0i32;
    for (i, &position) in positions.iter().enumerate() {
        if position == 0 {
            score += 12;
        }
        if position > 0 {
            let prev = haystack_chars[position - 1];
            if prev == ' ' || prev == '_' || prev == '-' || prev == '/' || prev == '.' {
                score += 10;
            }
        }
        if i > 0 {
            let prev = positions[i - 1];
            if position == prev + 1 {
                score += 18;
            } else {
                score -= (position - prev - 1) as i32;
            }
        }
        if haystack_chars[position] == needle_chars[i] {
            score += 4;
        }
    }
    score -= (haystack_chars.len() as i32) / 5;

    Some((score, positions))
}

/// Fuzzy match `haystack` against `needle` (case-insensitive).
///
/// First tries strict in-order subsequence matching. If that fails and the needle
/// contains whitespace, falls back to per-token matching where each
/// whitespace-separated token must subsequence-match the haystack but the tokens
/// themselves may appear in any order. The token fallback receives a constant
/// score penalty so true in-order matches always rank above it.
pub fn fuzzy_match(haystack: &str, needle: &str) -> Option<(i32, Vec<usize>)> {
    if needle.is_empty() {
        return Some((0, Vec::new()));
    }
    if let Some(result) = fuzzy_match_strict(haystack, needle) {
        return Some(result);
    }
    fuzzy_match_tokens(haystack, needle)
}

fn fuzzy_match_strict(haystack: &str, needle: &str) -> Option<(i32, Vec<usize>)> {
    let haystack_chars: Vec<char> = haystack.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();

    let mut positions = Vec::with_capacity(needle_chars.len());
    let mut hay_idx = 0;

    for &needle_ch in &needle_chars {
        let needle_lower = needle_ch.to_lowercase().next().unwrap_or(needle_ch);
        let mut found = false;
        while hay_idx < haystack_chars.len() {
            let hay_lower = haystack_chars[hay_idx]
                .to_lowercase()
                .next()
                .unwrap_or(haystack_chars[hay_idx]);
            if hay_lower == needle_lower {
                positions.push(hay_idx);
                hay_idx += 1;
                found = true;
                break;
            }
            hay_idx += 1;
        }
        if !found {
            return None;
        }
    }

    let score = compute_score(&haystack_chars, &needle_chars, &positions);
    Some((score, positions))
}

const TOKEN_FALLBACK_PENALTY: i32 = 50;

fn fuzzy_match_tokens(haystack: &str, needle: &str) -> Option<(i32, Vec<usize>)> {
    let tokens: Vec<&str> = needle.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }

    let mut total: i32 = 0;
    let mut all_positions: Vec<usize> = Vec::new();
    for token in &tokens {
        let (score, positions) = fuzzy_match_strict(haystack, token)?;
        total = total.saturating_add(score);
        all_positions.extend(positions);
    }
    all_positions.sort_unstable();
    all_positions.dedup();
    total = total.saturating_sub(TOKEN_FALLBACK_PENALTY);
    Some((total, all_positions))
}

fn compute_score(haystack: &[char], needle: &[char], positions: &[usize]) -> i32 {
    let mut score: i32 = 0;

    for (i, &position) in positions.iter().enumerate() {
        if position == 0 {
            score += 8;
        }

        if position > 0 {
            let prev = haystack[position - 1];
            if prev == ' ' || prev == '_' || prev == '-' || prev == '.' || prev == '/' {
                score += 8;
            }
        }

        if i > 0 && position == positions[i - 1] + 1 {
            score += 12;
        }

        if haystack[position] == needle[i] {
            score += 4;
        }

        if i > 0 {
            let gap = position as i32 - positions[i - 1] as i32 - 1;
            score -= gap;
        }
    }

    score -= (haystack.len() as i32) / 4;

    score
}

#[cfg(test)]
mod tests {
    use super::{fuzzy_match, fzf_style_match};

    #[test]
    fn fuzzy_match_is_case_insensitive() {
        let result = fuzzy_match("Save File", "sf");
        assert!(result.is_some());
    }

    #[test]
    fn fuzzy_match_returns_none_for_missing_sequence() {
        let result = fuzzy_match("Save File", "xyz");
        assert!(result.is_none());
    }

    #[test]
    fn fzf_style_match_prefers_consecutive_matches() {
        let (consecutive_score, _) = fzf_style_match("abcdef", "abc").expect("consecutive");
        let (sparse_score, _) = fzf_style_match("axbxcxdef", "abc").expect("sparse");
        assert!(consecutive_score > sparse_score);
    }

    #[test]
    fn fuzzy_match_token_fallback_allows_out_of_order_tokens() {
        // Strict subsequence "github co" cannot match "Copy GitHub URL" because
        // the 'co' in 'Copy' precedes 'github'. The token fallback should match.
        let result = fuzzy_match("Copy GitHub URL", "github co");
        assert!(result.is_some(), "token fallback should match");
    }

    #[test]
    fn fuzzy_match_strict_outranks_token_fallback() {
        // In-order match wins over reordered-token match.
        let (in_order, _) = fuzzy_match("Copy GitHub URL", "co github").expect("strict");
        let (out_of_order, _) = fuzzy_match("Copy GitHub URL", "github co").expect("fallback");
        assert!(in_order > out_of_order);
    }

    #[test]
    fn fuzzy_match_token_fallback_requires_all_tokens() {
        // "xyz" is absent from haystack, so the fallback must still fail.
        let result = fuzzy_match("Copy GitHub URL", "github xyz");
        assert!(result.is_none());
    }

    #[test]
    fn fuzzy_match_single_token_does_not_take_fallback() {
        // Single-token query: no fallback path; absence means None.
        let result = fuzzy_match("Copy GitHub URL", "xyz");
        assert!(result.is_none());
    }
}
