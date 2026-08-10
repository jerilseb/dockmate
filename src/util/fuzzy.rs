//! A small subsequence fuzzy matcher.
//!
//! Deliberately not a full fzf clone: it scores contiguous runs, word-boundary
//! hits and prefix matches, which is enough to rank a few hundred container and
//! image names sensibly. It also returns the matched character indices so the
//! UI can highlight them.

/// Score plus the indices (in `haystack` char positions) that matched.
#[derive(Debug, Clone)]
pub struct Match {
    pub score: i32,
    pub indices: Vec<usize>,
}

/// Scores at or above this came from an exact substring hit. Keeping the two
/// kinds of match in separate bands means a literal hit always wins, which is
/// what people expect when they type a whole word.
const SUBSTRING_BAND: i32 = 1_000;

/// Case-insensitive subsequence match. Returns `None` when `needle` is not a
/// subsequence of `haystack`. An empty needle matches everything with score 0.
///
/// An exact substring is found first, because the greedy subsequence scan below
/// takes the leftmost candidate for each character and can miss the obvious
/// alignment: searching `rest` in `container: restart` would otherwise latch
/// onto the `r` in `container`.
pub fn match_str(haystack: &str, needle: &str) -> Option<Match> {
    if needle.is_empty() {
        return Some(Match {
            score: 0,
            indices: Vec::new(),
        });
    }

    let hay: Vec<char> = haystack.chars().collect();
    let nee: Vec<char> = needle.chars().collect();

    if let Some(at) = find_substring(&hay, &nee) {
        let mut score = SUBSTRING_BAND + nee.len() as i32 * 4;
        if at == 0 {
            score += 20;
        } else if is_boundary(hay[at - 1]) {
            score += 10;
        }
        score -= (hay.len() as i32) / 8;
        return Some(Match {
            score,
            indices: (at..at + nee.len()).collect(),
        });
    }

    let mut indices = Vec::with_capacity(nee.len());
    let mut score = 0i32;
    let mut hi = 0usize;
    let mut run = 0i32;

    for &n in &nee {
        let target = n.to_ascii_lowercase();
        let found = loop {
            if hi >= hay.len() {
                break None;
            }
            let h = hay[hi];
            hi += 1;
            if h.to_ascii_lowercase() == target {
                break Some(hi - 1);
            }
            // A miss breaks the contiguous run.
            run = 0;
        };

        let at = found?;
        // Contiguity is the strongest signal: each additional adjacent char is
        // worth more than the last.
        run += 1;
        score += 4 + run * 4;
        if at == 0 {
            score += 12; // prefix match
        } else if is_boundary(hay[at - 1]) {
            score += 8; // word boundary
        }
        indices.push(at);
    }

    // Prefer shorter haystacks when the score is otherwise equal, so
    // `db` ranks `db` above `some-long-db-sidecar`.
    score -= (hay.len() as i32) / 8;
    // Clamp so a long subsequence match can never stray into the substring
    // band and outrank a literal hit.
    Some(Match {
        score: score.min(SUBSTRING_BAND - 1),
        indices,
    })
}

fn is_boundary(c: char) -> bool {
    matches!(c, '-' | '_' | '/' | '.' | ':' | ' ' | '@')
}

/// Case-insensitive contiguous search over char slices, so the returned offset
/// is a char index and lines up with everything else here.
fn find_substring(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&start| {
        hay[start..start + needle.len()]
            .iter()
            .zip(needle)
            .all(|(h, n)| h.eq_ignore_ascii_case(n))
    })
}

/// Rank `items` against `needle`, returning the surviving indices best-first.
/// With an empty needle the original order is preserved.
pub fn rank<T, F>(items: &[T], needle: &str, key: F) -> Vec<usize>
where
    F: Fn(&T) -> String,
{
    if needle.is_empty() {
        return (0..items.len()).collect();
    }
    let mut scored: Vec<(usize, i32)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| match_str(&key(item), needle).map(|m| (i, m.score)))
        .collect();
    // Stable sort keeps the underlying (already sorted) order for ties.
    scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_subsequence_fails() {
        assert!(match_str("postgres", "xyz").is_none());
    }

    #[test]
    fn empty_needle_matches() {
        assert!(match_str("anything", "").is_some());
    }

    #[test]
    fn contiguous_beats_scattered() {
        let contiguous = match_str("evalm8-server", "server").unwrap().score;
        let scattered = match_str("some-very-random-thing", "server");
        // "server" isn't a subsequence of that string at all.
        assert!(scattered.is_none());
        let loose = match_str("s-e-r-v-e-r", "server").unwrap().score;
        assert!(contiguous > loose, "{contiguous} !> {loose}");
    }

    #[test]
    fn indices_point_at_matched_chars() {
        // p-o-s-t-g-r-e-s → 'p' at 0, 'g' at 4.
        let m = match_str("postgres", "pg").unwrap();
        assert_eq!(m.indices, vec![0, 4]);
    }

    #[test]
    fn substring_indices_are_contiguous() {
        let m = match_str("container: restart", "rest").unwrap();
        assert_eq!(m.indices, vec![11, 12, 13, 14]);
    }

    #[test]
    fn literal_hit_beats_a_scattered_one() {
        // The greedy scan alone would latch onto the 'r' in "container" and
        // rank "remove selected" higher.
        let items = vec!["remove selected", "container: restart", "refresh"];
        let ranked = rank(&items, "rest", |s| s.to_string());
        assert_eq!(items[ranked[0]], "container: restart");
    }

    #[test]
    fn subsequence_never_reaches_the_substring_band() {
        let scattered = match_str("s-e-r-v-e-r-s-e-r-v-e-r", "server").unwrap();
        let literal = match_str("webserver", "server").unwrap();
        assert!(
            scattered.score < literal.score,
            "{} !< {}",
            scattered.score,
            literal.score
        );
    }

    #[test]
    fn ranking_puts_best_first() {
        let items = vec!["argilla-postgres-1", "postgres", "prometheus-gateway"];
        let ranked = rank(&items, "postgres", |s| s.to_string());
        assert_eq!(items[ranked[0]], "postgres");
    }
}
