#!/usr/bin/env bash
# check: no-gpui-in-twyx
# born: 2026-10-18
# failure: the twyx binary needs a display or GPU libraries, so it cannot install on a server
# rule: twyx never depends on GPUI
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

tree=$(cargo tree --package twyx --edges normal,build --prefix none)

if matches=$(printf '%s\n' "$tree" | grep '^gpui'); then
  printf 'twyx depends on gpui:\n%s\n' "$matches" >&2
  exit 1
fi
