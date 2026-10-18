set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# Inner loop: lesson checks, format, lints, tests, licences.
check:
    for script in checks/*.sh; do "$script"; done
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    cargo deny check licenses

# Full suite: every crate, every invariant test, the automated conformance items,
# and the wezterm engine on top of the default one.
test:
    cargo test --workspace
    cargo test --package twyx-holder --features engine-wezterm

# The ghostty engine needs Zig 0.16.0 on PATH and GHOSTTY_SOURCE_DIR pointing at
# a Ghostty checkout, so it runs here rather than in `test`.
test-ghostty:
    cargo test --package twyx-holder --features engine-ghostty
