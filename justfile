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

# Full suite: every crate, every invariant test and the automated conformance
# items.
test:
    cargo test --workspace

# The automated conformance cases: bytes in and expected screens, escape
# sequences, and the Neovim items through a real holder.
conformance:
    cargo test --package twyx-holder --test conformance --test escapes --test neovim

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
