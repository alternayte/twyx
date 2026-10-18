# twyx

## What this is
twyx is a developer workspace: a terminal multiplexer TUI and a native GPUI desktop app, both clients of one persistent per-user daemon that owns panes, worktrees, agents and review.
It runs Neovim, shells and agent CLIs unmodified, locally or over SSH, with no account and no AI requirement.

## Run
- No code exists yet.

## Test
- No code exists yet.

## Stack rules
- Rust stable in one Cargo workspace, not Go.
- The desktop client uses GPUI. The TUI uses Ratatui and Crossterm for its chrome only; pane cells go through the twyx emitter.
- Storage is SQLite, and only the daemon opens it. No Postgres.
- twyx ships as local binaries for macOS and Linux. No Kubernetes.
- Git runs only as the Git CLI in a subprocess, never through a Git library.
- The licence is MIT. Reading the source of Herdr, muxel, ThinkTerm and Zed is allowed; copying or adapting their code line by line is not, because they are GPL or AGPL.

## Domain words
- daemon: the one per-user twyx process that owns layouts, workspaces, agents, review, checks, the API and the database. Avoid: coordinator, mux server.
- holder: the per-pane process that owns one PTY, its terminal engine, input encoders and scrollback. Avoid: shim, PTY host.
- client: the TUI or the desktop app, attached to the daemon. Avoid: frontend, viewer.
- pane: one process tree on one PTY, run by one holder. Avoid: window, tile.
- project: one Git repository or one plain directory that twyx opens. Avoid: folder.
- worktree: one Git worktree of a project, the main one or a linked one. Avoid: checkout, clone.
- workspace: the tabs for exactly one worktree or plain directory. Avoid: mux session, layout.
- size lease: the right of one client to set the pane sizes of one tab. Avoid: input owner, resize owner.
- prefix: the one key twyx takes from a pane; it opens command mode. Avoid: leader, hotkey.
- agent session: one run of a known agent CLI in a pane. Avoid: agent run, conversation.
- inbox: the list of agent sessions and panes that need the user. Avoid: attention queue, attention item.
- review state: the per-worktree record of reviewed changes, kept as a review index and a ref. Avoid: checkpoint.
- base: the merge-base of HEAD and the base branch, where review starts. Avoid: fork point, parent.
- check: a configured command whose result is tied to a source key. Avoid: verification, verification run.
- source key: the Git tree ID of a worktree's tracked files and non-ignored untracked files. Avoid: source state, state hash.
- recipe: a TOML file that opens tabs, layouts, commands and worktrees. Avoid: template, layout file.
