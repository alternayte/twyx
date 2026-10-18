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

# The automated conformance cases: bytes in, screen out, against the default
# engine and then wezterm. Ghostty runs under `test-ghostty`.
conformance:
    cargo test --package twyx-holder --test conformance
    cargo test --package twyx-holder --features engine-wezterm --test conformance

# Keystroke-to-screen latency through the real client, on this machine.
# Release builds only for numbers that get published.
bench:
    cargo build --release --package twyx
    cargo run --release --package twyx --bin twyx-bench -- --samples 20 -- ./target/release/twyx

# The ghostty engine needs Zig 0.16.0 on PATH and GHOSTTY_SOURCE_DIR pointing at
# a Ghostty checkout, so it runs here rather than in `test`.
test-ghostty:
    cargo test --package twyx-holder --features engine-ghostty
