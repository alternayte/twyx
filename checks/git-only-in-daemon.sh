#!/usr/bin/env bash
# check: git-only-in-daemon
# born: 2026-10-19
# failure: a Git call outside the daemon's git module skips Invariant 10 (optional locks, pager, hooks)
# rule: only crates/twyx-daemon/src/git.rs runs Git; tests may make throwaway repositories
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

if matches=$(rg -n 'Command::new\("git"\)' crates --glob '*.rs' --glob '!crates/twyx-daemon/src/git.rs' --glob '!crates/*/tests/**'); then
  printf 'Git runs outside the daemon git module:\n%s\n' "$matches" >&2
  exit 1
fi
