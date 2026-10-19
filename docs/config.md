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
