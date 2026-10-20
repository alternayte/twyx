//! New tasks: which agent each new worktree gets.

/// One agent kind per worktree, `None` for a shell. `named` is the kinds
/// the user gave, in order, with `shell` for a shell; the last one fills the
/// rest of `count`, and with none given each worktree gets `default`.
pub fn per_worktree(
    named: &[String],
    count: usize,
    default: Option<String>,
) -> Vec<Option<String>> {
    let named: Vec<Option<String>> = named
        .iter()
        .map(|agent| agent.trim())
        .filter(|agent| !agent.is_empty())
        .map(|agent| (agent != "shell").then(|| agent.to_owned()))
        .collect();
    let last = named.last().cloned().unwrap_or(default);
    let total = count.max(named.len()).max(1);
    let mut all = named;
    all.resize(total, last);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_agent_fills_the_count() {
        let some = |kind: &str| Some(kind.to_owned());
        assert_eq!(
            per_worktree(&["claude".into(), "pi".into()], 3, None),
            vec![some("claude"), some("pi"), some("pi")]
        );
        assert_eq!(per_worktree(&[], 2, some("codex")), vec![some("codex"); 2]);
        assert_eq!(
            per_worktree(&["shell".into()], 1, some("codex")),
            vec![None]
        );
    }
}
