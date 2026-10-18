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
    cargo test --package twyx-holder --test conformance --test escapes --test neovim
    cargo test --package twyx-holder --features engine-wezterm --test conformance --test escapes --test neovim

# The agent CLI items, with the Claude Code, Pi and OpenCode installed and signed
# in on this machine. Each case runs a real agent against a real model, so this
# never runs in CI.
conformance-agents:
    cargo test --package twyx-holder --test agents -- --ignored --test-threads=3

# Latency, throughput, memory under a producer, and idle cost with ten panes,
# for twyx and for Zellij and Herdr when they are on PATH. Release builds only
# for numbers that get published.
bench:
    cargo build --release --package twyx
    ./target/release/twyx-bench --samples 20 --twyx ./target/release/twyx \
        $(command -v zellij >/dev/null && echo zellij) $(command -v herdr >/dev/null && echo herdr)

# The ghostty engine needs Zig 0.16.0 on PATH and GHOSTTY_SOURCE_DIR pointing at
# a Ghostty checkout of the commit libghostty-rs pins (3425025e), so it runs here
# rather than in `test`.
test-ghostty:
    cargo test --package twyx-holder --features engine-ghostty
