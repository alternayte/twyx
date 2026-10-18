# Conformance

What twyx checks, and how each item runs. A pass means the item ran on macOS and
Linux with the versions named in the run.

## Status

The matrix is the target for 0.1.0. The automated runners listed below arrive
with the conformance harness; until then, `docs/performance.md` and the release
notes carry the measured results.

## Terminal rendering

| Item | Runs as | Oracle |
|---|---|---|
| Undercurl and underline colour | Neovim `:hi` with `guisp` and `t.underline=curl` | Neovim's own screen model via its RPC socket |
| Truecolor | Neovim `:hi` with a 24-bit colour | the engine's screen model and the known RGB value |
| Wide cells and grapheme clusters | CJK text, ZWJ emoji, combining marks | the cursor column from Neovim's RPC does not drift |
| Osc 8 hyperlinks | a link printed by a program | the client's link state and the visible cells |
| Synchronized output | a program that draws in one flush | no torn frame while it draws |
| Scroll region and alternate screen | Neovim entering and leaving the alternate screen | screen content after leave |
| OSC 52 write | Neovim `yy` with `clipboard=unnamed` | the clipboard contains the yanked text |

## Input

| Item | Runs as | Oracle |
|---|---|---|
| `<C-i>`, `<Tab>`, `<S-CR>`, `<C-S-x>` arrive as different keys | Ghostty, kitty keyboard protocol on | Neovim's `:map` for each key, each mapping reported separately |
| Legacy keys through tmux | tmux as the outer terminal | Neovim receives the same keys as above, except the ones legacy encoding cannot express |
| A 1 MB bracketed paste arrives intact | a script that pastes a generated 1 MB line | byte count in the pane and no lost paste markers |
| Focus gained and lost | switching windows | Neovim's `FocusGained` and `FocusLost` autocmds fire |
| Mouse drag resizes a Neovim split | a drag on a split border | Neovim's window layout after the drag |
| Mouse wheel scrolls a TUI | wheel events in a pager | the visible line range |

## Agent CLIs

Local only, because they need credentials.

| Item | Runs as | Oracle |
|---|---|---|
| Multiline input | a prompt with three lines | the agent's rendered input box |
| Ctrl-C during streaming output | interrupt a long answer | the agent returns to its prompt |
| Resize during streaming output | resize the window mid-answer | no lost glyphs, no wrong cursor |
| A permission prompt | an agent asks for approval | status `awaiting-approval` in the inbox |
| Done detection | an agent finishes a turn | status `awaiting-prompt`, then `idle` once seen |

## Escape sequences

| Item | Runs as | Oracle |
|---|---|---|
| A subset of esctest | the expected-screen cases of esctest | the expected screen in the engine's model |
| Clipboard read refused | OSC 52 read from pane output | nothing is returned to the pane |
| Link targets outside `http`, `https`, `file` | an OSC 8 link with `javascript:` | the client does not offer to open it |

## Lifecycle

| Item | Runs as | Oracle |
|---|---|---|
| A pane survives the client | close the client, reattach | the same process, the same screen |
| A pane survives a daemon crash | `kill -9` the daemon, start it again | the pane is adopted with its screen |
| A pane survives a daemon upgrade | replace the daemon while a pane runs | the pane is adopted by the new daemon |
| Pane history after a holder crash | `kill -9` a holder | the restored screen equals the last write, plus any recorded gap |
| Restore after a host restart | restart the machine | layouts restore, panes show as ended with history |

## Running the items

- `just conformance` runs the automated items: escape-sequence cases, paste,
  focus, mouse, clipboard policy and lifecycle. It drives the real binaries on
  real PTYs in a temporary runtime directory.
- `crates/twyx-holder/tests/neovim.rs` runs the Neovim items in a holder: it
  starts Neovim on a real PTY, sends input events as a client does, and reads
  Neovim's state over its RPC socket and the screen from the holder's frames.
  It needs Neovim 0.11 or later on PATH. The OSC 52 item checks the yank inside
  the pane; the SSH leg runs by hand.
- `just conformance-agents` runs the agent CLI items with the agent CLIs
  installed and signed in on this machine. It never runs in CI.
- CI runs the Neovim items on Ubuntu and macOS, in Ghostty and inside tmux where
  the runner provides them.
- Every item names the terminal it ran in, the terminal version, and the twyx
  commit.
