# Config

twyx reads `~/.config/twyx/config.toml`, or `$XDG_CONFIG_HOME/twyx/config.toml` when that is set. A missing file means the defaults.

The TUI checks the file once a second. A change takes effect at once. A file with an error keeps the last good config, and the status bar shows the error. At start, an error stops twyx. Every error gives the file, the line and the reason.

## `[keys]`

The prefix and the bindings of each mode. See [keys](keys.md).

## `[theme]`

twyx draws its chrome on your terminal's own background and text colours. It mixes its greys from them.

| Key | Values | Default |
|---|---|---|
| `variant` | `auto`, `light`, `dark` | `auto`: twyx reads your terminal's background |
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
| `sound` | a path to a sound file for the desktop client | its own sound |

No notification fires for a pane you can see in a focused client. When several clients are attached, the focused one notifies; when none is focused, the one you used last does. The TUI sends OSC 9 to Ghostty, iTerm2, WezTerm and kitty, and OSC 777 to foot and urxvt. Inside tmux it wraps OSC 9 in tmux's passthrough, which reaches your terminal only with `set -g allow-passthrough on`. With `sound`, the TUI also rings the bell.

```toml
[notifications]
turn-finished = "inbox"
```
