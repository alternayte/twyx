#!/usr/bin/env bash
# check: theme-tokens
# born: 2026-10-19
# failure: a colour written into a widget drifts from the theme and breaks light or dark paper
# rule: the TUI draws every colour from twyx_core::theme; no colour literal in crates/twyx-tui/src
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

pattern='Rgb::hex\(|Color::(Rgb|Indexed)\([0-9]|Color::(Black|Red|Green|Yellow|Blue|Magenta|Cyan|Gray|DarkGray|Light[A-Za-z]+|White)\b|0x[0-9A-Fa-f]{6}\b'

if matches=$(grep -rnE "$pattern" crates/twyx-tui/src); then
  printf 'colour literals outside the theme tokens:\n%s\n' "$matches" >&2
  exit 1
fi
