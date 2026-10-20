# twyx

A developer workspace: a terminal multiplexer and a native desktop app over one
persistent local daemon.

twyx runs Neovim, shells and agent CLIs unmodified, locally or over SSH. It needs
no account, no cloud, no model and no credentials.

Status: pre-release. Version 0.1.0 is in development and no release exists yet.
The terminal client works. The desktop client is not ready for use.

## Install

You need these on PATH:

- Rust through rustup. The repository pins the toolchain, and rustup installs it on the first build.
- Zig 0.16. The terminal engine builds with it.
- Git 2.34 or later. Review uses `git apply --cached --3way`, which older Git refuses.

Build and install from a clone:

```sh
git clone https://github.com/alternayte/twyx.git
cd twyx
cargo install --locked --path crates/twyx
```

This puts `twyx` in `~/.cargo/bin`. To update, pull and run the same `cargo install` command again. Then run `twyx daemon stop`. Your panes keep running, and the next `twyx` starts the new daemon, which adopts them.

## Start

Run `twyx` in your terminal. It starts the daemon when none runs, and it attaches the TUI.

The prefix is `ctrl-g`. Every other key goes to the focused pane.

- `ctrl-g n` splits the pane. `ctrl-g x` closes it.
- `ctrl-g h`, `j`, `k`, `l` move the focus.
- `ctrl-g t` then `n` opens a new tab. `ctrl-g 1` to `9` go to a tab.
- `ctrl-g :` opens the palette, which finds panes and commands.
- `ctrl-g d` detaches. The panes keep running. Run `twyx` again to attach.
- `ctrl-g ctrl-g` sends `ctrl-g` to the pane.

[docs/keys.md](docs/keys.md) lists every key and tells you how to change them.

## Projects and worktrees

A workspace is the tabs of one Git worktree or one plain directory. twyx starts in a default workspace at your home directory.

- `twyx open <path>` opens the project that the path is in and shows its workspace.
- `ctrl-g b` shows the sidebar: projects, worktrees, workspaces and their panes.
- `ctrl-g s` opens the workspace switcher.
- `ctrl-g w` opens workspace mode. In it, `n` makes a worktree and `x` removes one.
- `twyx ls` lists projects, worktrees and workspaces, with the branch and dirty state.
- `twyx worktree new <branch>` makes a worktree. `twyx worktree rm <branch>` removes one. Before it removes anything, it shows the changes and commits that you lose and the panes that it ends.
- `twyx recipe list` lists the recipes. `twyx recipe run <name>` opens the tabs and panes of a recipe.

Every command takes `--help`. Most take `--json`.

## Configure

twyx reads `~/.config/twyx/config.toml`. A missing file means the defaults. [docs/config.md](docs/config.md) lists every setting.

## Over SSH

Install twyx on the remote host and run `twyx` there. [docs/ssh.md](docs/ssh.md) tells you how to keep panes after you log out and how to copy to your local clipboard.

## When something is wrong

- `twyx doctor` prints what your terminal supports.
- `twyx daemon status` tells you if the daemon runs.
- `twyx daemon stop` stops the daemon. The panes keep running, and the next `twyx` starts a new daemon.
- The daemon keeps its database and pane history in `~/.local/state/twyx`.

Licence: MIT.
