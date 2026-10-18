#!/usr/bin/env bash
# check: linux-target
# born: 2026-10-18
# failure: macOS-only code paths compile locally while the Linux build breaks in CI
# rule: the workspace type-checks for the Linux target before it is pushed
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

if [ "$(uname -s)" = "Linux" ]; then
  exit 0
fi

if ! rustup target list --installed | grep -q '^x86_64-unknown-linux-gnu$'; then
  printf 'install the cross-check target first:\n  rustup target add x86_64-unknown-linux-gnu\n' >&2
  exit 1
fi

cargo check --quiet --workspace --target x86_64-unknown-linux-gnu
