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

# Full suite: every crate, every invariant test, the automated conformance items.
test:
    cargo test --workspace --all-features
