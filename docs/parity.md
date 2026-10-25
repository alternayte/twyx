# Parity

twyx has two clients: the TUI and the desktop app. Both attach to the same daemon and use the same keymap. This page lists each feature and its level in each client.

- **full**: the feature works as described.
- **partial**: some of the feature works. The note says what is missing.
- **none**: the client does not have the feature. The note says why.

## Panes and layout

| Feature | TUI | Desktop |
|---|---|---|
| Tabs: new, close, rename, go to by number | full | full |
| Nested splits, focus, resize, swap | full | full |
| Stacked panes | full | full |
| Floating panes: new, move, resize | full | full |
| Zoom | full | full |
| Drag a pane by its title to split, stack or float it | none | full |
| Drag a rule between panes to resize | full | full |
| Copy mode with search | full | full |
| Select text with the mouse and copy it | full | full |
| Mouse wheel to the pane | full | full |
| Open a link from pane output (OSC 8) | full: your terminal opens it | full: cmd-click shows the target first |
| Two clients on one tab, with the size lease | full | full |

## Projects and workspaces

| Feature | TUI | Desktop |
|---|---|---|
| Sidebar: projects, worktrees, workspaces, panes, agents | full | full |
| Status marks and their roll-up | full | full |
| Worktree actions: new, remove, set up | full | full |
| Workspace switcher | full | full |
| Recipes | full | full |
| New task | full | full |

## Agents and the inbox

| Feature | TUI | Desktop |
|---|---|---|
| Inbox list: jump, mark unread, dismiss | full | full |
| Inbox in the sidebar | none: the inbox list shows it | full |
| Notifications | full: OSC 9 or OSC 777 and the bell | full: OS notification and sound |
| Unread count on the Dock icon | none: a terminal has no Dock icon | full on macOS |
| Resume agent sessions | full | full |
| Agent integrations offer | full | full |

## Review and checks

| Feature | TUI | Desktop |
|---|---|---|
| File list with review state | full | full |
| Unified diff | full | full |
| Side-by-side diff | partial: only when the pane is wide enough | full |
| Line numbers in the diff | none | full |
| Syntax highlighting | none | full |
| Hunk navigation and mark reviewed | full | full |
| Notes and send to agent | full | full |
| Stage, commit, push, pull request | full | full |
| Checks: run, cancel, output, trust | full | full |

## Files and documents

| Feature | TUI | Desktop |
|---|---|---|
| File tree | none: by design, quick open opens files | full |
| Quick open | full | full; the palette also lists the files |
| Find in files | full | full |
| Markdown documents: headings, lists, task lists, tables, code, links, reload on change | full | full |
| Images in documents | partial: the description shows; a terminal shows no picture | full |
| Syntax highlighting in document code | none | full |
| Open in Neovim | full | full |

## Chrome

| Feature | TUI | Desktop |
|---|---|---|
| Command palette | full | full |
| Key sheet | full | full |
| Themes from `[theme]` | full: on the terminal's own colours | full: on the app's own paper |
| Config reload | full | full |
| Motion: travelling focus, tab and selection marks, mark cross-fade | none: the TUI stays still | full |
| Reduced motion | not needed | full |
| Runs over SSH | full | none: run the TUI on the remote host |
