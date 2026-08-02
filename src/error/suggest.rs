//! Guessing what someone meant to write.
//!
//! Used wherever a name is not found and there is a list of names it could have
//! been — an undefined variable against the scope, a misspelled method against
//! the class, a bad `op` name against `OPS`.

/// Computes Levenshtein distance between two strings for fuzzy matching suggestions.
pub fn lev_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let mut d = vec![vec![0; b_chars.len() + 1]; a_chars.len() + 1];

    // Each dimension is one longer than its string, so `enumerate` walks exactly
    // the range the edit distance is defined over.
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }

    for i in 1..=a_chars.len() {
        for j in 1..=b_chars.len() {
            let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
        }
    }
    d[a_chars.len()][b_chars.len()]
}

/// Finds the closest matching candidate for `name` if one exists within a small edit distance.
pub fn did_you_mean<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut best_match = None;
    let mut best_dist = usize::MAX;

    for candidate in candidates {
        let dist = lev_distance(name, candidate);
        let max_dist = if name.len() <= 4 { 1 } else { 2 };
        if dist <= max_dist && dist < best_dist {
            best_dist = dist;
            best_match = Some(candidate);
        }
    }
    best_match
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_did_you_mean_suggestions() {
        let candidates = vec!["name", "age", "items", "count"];
        assert_eq!(did_you_mean("namme", candidates.clone()), Some("name"));
        assert_eq!(did_you_mean("cont", candidates.clone()), Some("count"));
        assert_eq!(did_you_mean("completely_unrelated", candidates), None);
    }
}
