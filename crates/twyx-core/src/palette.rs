//! The palette's search: one query over every entry, in order of fit.
//!
//! A query matches an entry when its characters appear in the entry in
//! order, ignoring case. Runs of adjacent characters and characters at the
//! start of a word fit better.

/// One entry that matched, with the character positions that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub index: usize,
    pub positions: Vec<usize>,
    score: i32,
}

/// The entries that match `query`, best first. An empty query keeps every
/// entry in its own order.
pub fn search<'a>(entries: impl IntoIterator<Item = &'a str>, query: &str) -> Vec<Found> {
    let query: Vec<char> = query
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    let mut found: Vec<Found> = entries
        .into_iter()
        .enumerate()
        .filter_map(|(index, text)| {
            fit(text, &query).map(|(score, positions)| Found {
                index,
                positions,
                score,
            })
        })
        .collect();
    found.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
    found
}

fn fit(text: &str, query: &[char]) -> Option<(i32, Vec<usize>)> {
    let chars: Vec<char> = text.chars().collect();
    let mut positions = Vec::with_capacity(query.len());
    let mut score = 0;
    let mut from = 0;
    for wanted in query {
        let at =
            (from..chars.len()).find(|&at| chars[at].to_lowercase().eq(wanted.to_lowercase()))?;
        let word_start = at == 0 || !chars[at - 1].is_alphanumeric();
        let adjacent = positions.last().is_some_and(|last: &usize| *last + 1 == at);
        score += 1 + if adjacent { 4 } else { 0 } + if word_start { 3 } else { 0 };
        // A gap costs a little, so tighter matches win.
        score -= i32::try_from(at - from).unwrap_or(i32::MAX).min(5);
        positions.push(at);
        from = at + 1;
    }
    Some((score, positions))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order<'a>(entries: &[&'a str], query: &str) -> Vec<&'a str> {
        search(entries.iter().copied(), query)
            .into_iter()
            .map(|found| entries[found.index])
            .collect()
    }

    #[test]
    fn characters_in_order_match_ignoring_case() {
        assert_eq!(
            order(&["split right", "close pane", "Spec Theme"], "spe"),
            ["Spec Theme", "close pane"]
        );
        assert!(order(&["close pane"], "pc").is_empty());
    }

    #[test]
    fn word_starts_and_runs_beat_scattered_matches() {
        assert_eq!(
            order(&["pane content", "new tab"], "nt"),
            ["new tab", "pane content"]
        );
    }

    #[test]
    fn positions_name_the_matched_characters() {
        let found = search(["run again"], "rag");
        assert_eq!(found[0].positions, [0, 4, 5]);
    }

    #[test]
    fn an_empty_query_keeps_every_entry_in_order() {
        assert_eq!(order(&["b", "a"], ""), ["b", "a"]);
    }
}
