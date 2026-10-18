set shell := ["bash", "-euo", "pipefail", "-c"]

# twyx-desktop needs Xcode's Metal toolchain, which is not on this machine yet;
# CI builds it on macOS. Drop the exclusions when it is.

default:
    @just --list

# Inner loop: lesson checks, format, lints, tests, licences.
check:
    for script in checks/*.sh; do "$script"; done
    cargo fmt --all --check
    cargo clippy --workspace --exclude twyx-desktop --all-targets -- -D warnings
    cargo test --workspace --exclude twyx-desktop
    cargo deny check licenses

# Full suite: every crate, every invariant test, the automated conformance items.
test:
    cargo test --workspace --exclude twyx-desktop --all-features
