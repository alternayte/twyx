# Keys

Every key goes to the focused pane, except the prefix. The prefix is `ctrl-g`.

- The prefix opens command mode for one key.
- In command mode, a mode key opens a sticky mode. A sticky mode stays open until you press `esc`.
- The prefix in a sticky mode goes back to command mode.
- The prefix twice sends the prefix to the pane.
- A key with no binding in a mode does nothing.
- The TUI and the desktop client use the same keymap.
- `ctrl-g ?` shows every bound key, by mode, with your own bindings. `j` and `k` scroll it. `esc` or `?` closes it.

Copy mode (`ctrl-g c`) moves a cursor over the pane and its scrollback. `v` starts a selection, and `y` or `enter` copies it to the clipboard and leaves copy mode. `/` searches from the newest output toward the oldest, and `n` and `N` move between the matches. Every client of the pane sees the same copy cursor. The mouse wheel over a shell opens copy mode too.

Workspace mode (`ctrl-g w`) shows the sidebar while it is open and moves a cursor over it: `j` and `k` move, `enter` shows the workspace, worktree or pane under the cursor, `h` and `l` go to the previous and next workspace, `n` makes a worktree of the project under the cursor, `x` removes the worktree under the cursor after a prompt, and `u` sets it up again. `ctrl-g b` keeps the sidebar open beside the panes, and `ctrl-g s` opens the workspace switcher.

These are all the keys twyx can take from a pane: the prefix, the keys below once the prefix has opened a mode, and the desktop shortcuts.

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
| `c` | `mode-copy` |
| `n` | `split-right` |
| `x` | `close-pane` |
| `z` | `toggle-zoom` |
| `:` | `palette` |
| `?` | `help` |
| `d` | `detach` |
| `w` | `mode-workspace` |
| `s` | `switcher` |
| `b` | `toggle-sidebar` |
| `i` | `inbox` |
| `v` | `review` |
| `e` | `checks` |
| `o` | `open-file` |
| `/` | `find-in-files` |
| `F` | `files` |
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
| `e` | `resume` |
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
| `r` | `rename-tab` |
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

### copy

| Key | Command |
|---|---|
| `v` | `copy-select` |
| `V` | `copy-select-line` |
| `y` | `copy-yank` |
| `enter` | `copy-yank` |
| `/` | `copy-search` |
| `n` | `copy-next` |
| `N` | `copy-previous` |
| `h` | `copy-left` |
| `left` | `copy-left` |
| `j` | `copy-down` |
| `down` | `copy-down` |
| `k` | `copy-up` |
| `up` | `copy-up` |
| `l` | `copy-right` |
| `right` | `copy-right` |
| `w` | `copy-word-forward` |
| `b` | `copy-word-back` |
| `0` | `copy-line-start` |
| `$` | `copy-line-end` |
| `g` | `copy-top` |
| `G` | `copy-bottom` |
| `ctrl-u` | `copy-half-page-up` |
| `ctrl-d` | `copy-half-page-down` |
| `pageup` | `copy-page-up` |
| `pagedown` | `copy-page-down` |

### workspace

| Key | Command |
|---|---|
| `enter` | `sidebar-open` |
| `n` | `new-worktree` |
| `t` | `new-task` |
| `x` | `remove-worktree` |
| `u` | `setup-worktree` |
| `s` | `switcher` |
| `b` | `toggle-sidebar` |
| `j` | `sidebar-down` |
| `down` | `sidebar-down` |
| `k` | `sidebar-up` |
| `up` | `sidebar-up` |
| `l` | `next-workspace` |
| `right` | `next-workspace` |
| `h` | `previous-workspace` |
| `left` | `previous-workspace` |


## Desktop shortcuts

The desktop client also takes these keys from the panes. They are fixed.

| macOS | Linux | Action |
|---|---|---|
| `cmd-t` | | new tab |
| `cmd-d` | | split right |
| `cmd-shift-d` | | split down |
| `cmd-w` | | close the pane |
| `cmd-shift-p` | | palette |
| `cmd-p` | | open a file |
| `cmd-shift-f` | | find in files |
| `cmd-shift-e` | | show the files or the spaces in the sidebar |
| `cmd-c` | `ctrl-shift-c` | copy the selection |
| `cmd-v` | `ctrl-shift-v` | paste |

A mouse drag in a pane selects text, unless the pane's program asked for the mouse. Hold `shift` to select in such a pane.

## Review view

These keys work while a review leaf has the focus. They are fixed. The view shows them on its last two rows.

| Key | Action |
|---|---|
| `j` `k` | next and previous file |
| `n` `p` | next and previous hunk |
| `space`, `enter` | mark the hunk reviewed |
| `f` | mark the file reviewed |
| `s` | one side or two sides |
| `c` | add a note on the hunk |
| `S` | send the open notes to an agent pane |
| `r` | read the review again |
| `a` `A` | stage the hunk, the file |
| `u` `U` | unstage the hunk, the file |
| `+` `-` | stage, unstage every change |
| `R` | stage the files whose changes are all reviewed |
| `C` | commit the staged changes; the hooks run in a new tab |
| `P` | push the branch, after you confirm; the first push sets the upstream |
| `O` | open a pull request with `gh` |

## Checks

`ctrl-g e` lists the checks of the worktree. In the list, `enter` runs the check, `o` shows its last run's pane, `x` cancels a running check, and `t` asks to trust the repository's check file.

## Files

`ctrl-g o` lists the files of the workspace. Type to narrow the list, and `enter` opens the file in Neovim. `ctrl-g /` finds text in the files: type the text, and `enter` opens the chosen line in Neovim. A query with no capital letter ignores case. The list has the tracked files and the untracked files that Git does not ignore. In a directory that is not a Git repository, the list has the files that its ignore files do not hide.

twyx opens the file in the Neovim of a pane in the workspace. With no Neovim in the workspace, it starts Neovim in a new pane.

In the desktop client, `ctrl-g F` shows the file tree in the sidebar and gives it the keys. In the tree, `j` and `k` move, `enter` opens a directory or opens a file, `l` opens a directory, `h` closes it or goes to its parent, and `esc` gives the keys back to the panes. The TUI has no file tree.
