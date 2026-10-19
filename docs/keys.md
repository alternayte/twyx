# Keys

Every key goes to the focused pane, except the prefix. The prefix is `ctrl-g`.

- The prefix opens command mode for one key.
- In command mode, a mode key opens a sticky mode. A sticky mode stays open until you press `esc`.
- The prefix in a sticky mode goes back to command mode.
- The prefix twice sends the prefix to the pane.
- A key with no binding in a mode does nothing.
- The TUI and the desktop client use the same keymap.

These are all the keys twyx can take from a pane: the prefix, and the keys below once the prefix has opened a mode.

## Change a key

Put bindings in `~/.config/twyx/config.toml`. A binding replaces the default for the same key in the same mode, and `none` removes it.

```toml
[keys]
prefix = "ctrl-a"

[keys.pane]
v = "split-right"
s = "none"
```

A key is a character (`h`, `H`, `|`), a name (`enter`, `tab`, `esc`, `space`, `backspace`, `delete`, `insert`, `home`, `end`, `pageup`, `pagedown`, `up`, `down`, `left`, `right`, `f1` to `f35`), and any of `ctrl-`, `alt-` and `shift-` before it. A shifted character is its own key: write `H`, not `shift-h`.

You cannot bind the prefix or `esc` in a mode. A wrong key, command or section stops twyx at start. The error gives the file, the line and the reason.

## Default bindings

### command

| Key | Command |
|---|---|
| `p` | `mode-pane` |
| `t` | `mode-tab` |
| `r` | `mode-resize` |
| `m` | `mode-move` |
| `f` | `mode-floating` |
| `n` | `split-right` |
| `x` | `close-pane` |
| `z` | `toggle-zoom` |
| `d` | `detach` |
| `h` | `focus-left` |
| `left` | `focus-left` |
| `j` | `focus-down` |
| `down` | `focus-down` |
| `k` | `focus-up` |
| `up` | `focus-up` |
| `l` | `focus-right` |
| `right` | `focus-right` |
| `1` | `go-to-tab-1` |
| `2` | `go-to-tab-2` |
| `3` | `go-to-tab-3` |
| `4` | `go-to-tab-4` |
| `5` | `go-to-tab-5` |
| `6` | `go-to-tab-6` |
| `7` | `go-to-tab-7` |
| `8` | `go-to-tab-8` |
| `9` | `go-to-tab-9` |

### pane

| Key | Command |
|---|---|
| `r` | `split-right` |
| `d` | `split-down` |
| `s` | `new-stacked` |
| `f` | `toggle-floating` |
| `z` | `toggle-zoom` |
| `x` | `close-pane` |
| `a` | `run-again` |
| `h` | `focus-left` |
| `left` | `focus-left` |
| `j` | `focus-down` |
| `down` | `focus-down` |
| `k` | `focus-up` |
| `up` | `focus-up` |
| `l` | `focus-right` |
| `right` | `focus-right` |

### tab

| Key | Command |
|---|---|
| `n` | `new-tab` |
| `x` | `close-tab` |
| `h` | `previous-tab` |
| `left` | `previous-tab` |
| `l` | `next-tab` |
| `right` | `next-tab` |
| `1` | `go-to-tab-1` |
| `2` | `go-to-tab-2` |
| `3` | `go-to-tab-3` |
| `4` | `go-to-tab-4` |
| `5` | `go-to-tab-5` |
| `6` | `go-to-tab-6` |
| `7` | `go-to-tab-7` |
| `8` | `go-to-tab-8` |
| `9` | `go-to-tab-9` |

### resize

| Key | Command |
|---|---|
| `h` | `resize-left` |
| `left` | `resize-left` |
| `j` | `resize-down` |
| `down` | `resize-down` |
| `k` | `resize-up` |
| `up` | `resize-up` |
| `l` | `resize-right` |
| `right` | `resize-right` |

### move

| Key | Command |
|---|---|
| `h` | `swap-left` |
| `left` | `swap-left` |
| `j` | `swap-down` |
| `down` | `swap-down` |
| `k` | `swap-up` |
| `up` | `swap-up` |
| `l` | `swap-right` |
| `right` | `swap-right` |

### floating

| Key | Command |
|---|---|
| `n` | `new-floating` |
| `f` | `toggle-floating` |
| `h` | `move-floating-left` |
| `left` | `move-floating-left` |
| `j` | `move-floating-down` |
| `down` | `move-floating-down` |
| `k` | `move-floating-up` |
| `up` | `move-floating-up` |
| `l` | `move-floating-right` |
| `right` | `move-floating-right` |

