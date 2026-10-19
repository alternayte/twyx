# Over SSH

twyx runs on a remote host as an ordinary terminal program. Run `twyx` in your SSH session, the same as at the host's own terminal.

## Panes that outlive the session

On Linux with a systemd user manager, twyx starts the daemon as the transient user unit `twyx-daemon`, and each pane's holder in a transient scope of its own. Stopping the daemon's unit leaves the panes running.

By default, systemd ends a user's processes when the last session of that user closes. To keep panes after you log out, turn on lingering once:

```sh
loginctl enable-linger "$USER"
```

Without a systemd user manager, twyx starts the daemon and the holders in sessions of their own. They outlive the SSH connection unless the host ends every process of the user at logout.

## Clipboard

A copy in twyx (a mouse selection or a copy-mode yank) reaches your local clipboard through OSC 52, a request that twyx writes to the terminal on your side of the connection. Your local terminal must allow programs to set the clipboard. Ghostty, kitty, WezTerm and iTerm2 do, some after a setting. Inside tmux on either side, set `set-clipboard on`.
