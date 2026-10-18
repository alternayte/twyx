#!/usr/bin/env bash
# check: no-private-docs
# born: 2026-10-18
# failure: a build doc reached the public repo, or a public file names one
# rule: build docs stay on one machine and are never quoted
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

status=0

while IFS= read -r path; do
  case "$path" in
    .build/* | CLAUDE.local.md)
      printf 'private path is tracked: %s\n' "$path" >&2
      status=1
      ;;
  esac
done < <(git ls-files)

# The pattern is written as a regular expression so that this file does not
# itself contain the marker it looks for.
marker='Internal[[:space:]]+build[[:space:]]+doc'

if git grep --quiet --extended-regexp -- "$marker"; then
  printf 'a tracked file contains the private marker:\n' >&2
  git grep --extended-regexp -- "$marker" >&2 || true
  status=1
fi

exit "$status"
