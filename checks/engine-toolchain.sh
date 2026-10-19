#!/usr/bin/env bash
# check: engine-toolchain
# born: 2026-10-19
# failure: the libghostty-vt build script fails deep inside `zig build` when Zig is missing or another version
# rule: Zig 0.16 is on PATH before cargo builds the engine
set -euo pipefail

if ! command -v zig >/dev/null; then
  printf 'the terminal engine builds with Zig 0.16; install it and put zig on PATH\n' >&2
  exit 1
fi
version=$(zig version)
case "$version" in
  0.16.*) ;;
  *)
    printf 'the terminal engine builds with Zig 0.16, but zig on PATH is %s\n' "$version" >&2
    exit 1
    ;;
esac
