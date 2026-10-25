# Config

twyx reads `~/.config/twyx/config.toml`, or `$XDG_CONFIG_HOME/twyx/config.toml` when that is set. A missing file means the defaults.

The TUI and the desktop app check the file once a second. A change takes effect at once. A file with an error keeps the last good config, and the status bar shows the error. At start, a file with an error gives the defaults, and the status bar shows the error. Every error gives the file, the line and the reason.

## `[keys]`

The prefix and the bindings of each mode. See [keys](keys.md).

## `[theme]`

The TUI draws its chrome on your terminal's own background and text colours. It mixes its greys from them. The desktop app uses its own light and dark paper.

| Key | Values | Default |
|---|---|---|
| `variant` | `auto`, `light`, `dark` | `auto`: the TUI reads your terminal's background; the desktop app follows the system appearance |
| `accent` | a colour as `#rrggbb` | the variant's blue |

The variant sets the accent and the colours of agent states. `auto` asks the terminal for its background colour. A terminal that does not answer gets `dark`.

```toml
[theme]
variant = "light"
accent = "#c8401c"
```

## `[history]`

Each pane keeps its screen and scrollback on disk, so they survive a crash, a power loss and a host restart. A pane that ended shows its history, and "Run again" puts it above the new run. Closing a pane deletes its history, and `twyx history purge` deletes all of it. The files are in `~/.local/state/twyx/history`, readable only by you.

| Key | Values | Default |
|---|---|---|
| `enabled` | `true`, `false` | `true` |

`twyx pane new --no-history` turns history off for one pane.

## `[worktrees]`

`twyx worktree new <branch>` puts the new worktree next to the repository: `<repo parent>/<repo name>.worktrees/<branch>`.

| Key | Values | Default |
|---|---|---|
| `location` | a path with `{parent}`, `{repo}` and `{branch}` | `{parent}/{repo}.worktrees/{branch}` |
| `branch-prefix` | text that each new task's branch starts with, such as `me/` | none |

The path starts with `/`, `~/` or `{parent}`, and must contain `{branch}`. `{parent}` is the directory that holds the main worktree. `{repo}` is the main worktree's directory name.

```toml
[worktrees]
location = "~/worktrees/{repo}/{branch}"
```

## `[agents]`

| Key | Values | Default |
|---|---|---|
| `default` | `claude`, `pi`, `opencode`, `codex`, `cursor` | none: an agent pane runs your shell |

A recipe pane with `agent = true` starts the default agent.

```toml
[agents]
default = "claude"
```

## `[notifications]`

Each kind of inbox item can be `off` (no inbox item), `inbox` (an inbox item only), `notify` (an inbox item and a notification) or `sound` (a notification with a sound).

| Key | Values | Default |
|---|---|---|
| `needs-approval` | `off`, `inbox`, `notify`, `sound` | `sound` |
| `turn-finished` | `off`, `inbox`, `notify`, `sound` | `notify` |
| `failed` | `off`, `inbox`, `notify`, `sound` | `notify` |
| `check-ended` | `off`, `inbox`, `notify`, `sound` | `notify` |
| `sound` | a path to a sound file for the desktop client | its own sound |

No notification fires for a pane you can see in a focused client. When several clients are attached, the focused one notifies; when none is focused, the one you used last does. The TUI sends OSC 9 to Ghostty, iTerm2, WezTerm and kitty, and OSC 777 to foot and urxvt. Inside tmux it wraps OSC 9 in tmux's passthrough, which reaches your terminal only with `set -g allow-passthrough on`. With `sound`, the TUI also rings the bell.

```toml
[notifications]
turn-finished = "inbox"
```

## `[clipboard]`

A program in a pane can set your clipboard with the OSC 52 escape sequence. No pane can read it.

| `osc52` | Effect |
|---|---|
| `focused` | Only the pane that has the focus in a client can set the clipboard. This is the default. |
| `allow` | Any pane can. |
| `deny` | No pane can. |

```toml
[clipboard]
osc52 = "deny"
```

## `[[check]]`

A check is a command whose result twyx ties to the source it ran on. Your checks in `config.toml` apply to every worktree. A repository can add its own in `.twyx/checks.toml`, with the same table. Repository checks run only after you trust that file as it is; a change to the file asks again. A repository check with the name of one of yours does not show.

| Key | Value |
|---|---|
| `name` | the name you run it by |
| `command` | a command for `/bin/sh -c`, run in the worktree's directory |
| `timeout` | seconds; optional |

```toml
[[check]]
name = "test"
command = "cargo test"
timeout = 900
```

Each run is a pane without a leaf. `ctrl-g e` lists the checks: `enter` runs one, `o` shows its pane, `x` cancels it. From a shell, `twyx check list`, `twyx check run <name>`, `twyx check status [<name>]` and `twyx check trust` do the same; `twyx check run` waits and exits non-zero unless the check passes.

twyx takes the source key, the Git tree of the tracked files and the untracked files that are not ignored, before the run and after it. The result is:

- `pass`: the command exited 0 and the source key did not change.
- `fail`: the command exited with another code or a signal, and the source key did not change.
- `inconclusive`: the source changed during the run.
- `timed out`: the run passed its timeout; twyx killed its whole process group.
- `cancelled`: you stopped the run, or closed its pane.

A pass or a fail shows as `stale` once the source changes after it. A result keeps the exit code and the last 20 lines of output. A pass is evidence, not proof: the command exited 0 on that source.
