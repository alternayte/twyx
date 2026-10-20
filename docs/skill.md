---
name: twyx
description: Drive twyx, the terminal workspace this session may run in. Use it to open panes, start tasks in new Git worktrees, wait for other agents, and run checks and read their results.
---

# twyx

twyx is a terminal workspace. One daemon per user keeps panes, Git worktrees, agent sessions, review and checks. Its `twyx` command talks to the daemon. Run `twyx <command> --help` for every option.

You are inside twyx when `TWYX_SOCKET` is set. Outside, the commands still work: they start the daemon when it does not run.

Add `--json` to any command to get its result as JSON. Use it when you read results in a program.

## Find your place

- `twyx ls` lists projects, worktrees and workspaces, with branch and dirty state.
- `twyx open <path>` opens the project a path is in and prints its workspace.
- `twyx pane list` lists panes: ID, size, state and agent session.

## Create a pane

```sh
twyx pane new -- npm run dev        # a command, beside the focused pane
twyx pane new --place tab           # a shell in a new tab
```

The command prints the new pane's ID. `--cwd <dir>` sets its directory; the default is the current directory.

Send text or keys to a pane:

```sh
twyx pane send 12 --text "npm test" --key enter
twyx pane send 12 --key ctrl-c
```

`twyx pane kill <pane>` ends a pane's process. `twyx pane focus <pane>` shows it to the user.

## Start a task

A task is new work in its own Git worktree, with an agent and a first prompt:

```sh
twyx task new --name fix-login --agent claude --prompt "Fix the login timeout in auth.rs"
```

Run it inside the repository. twyx makes a branch and a worktree for each agent, runs the worktree's setup, then starts the agent with the prompt. `--agent` takes an agent kind or `shell`; repeat it for several worktrees.

## Wait for an agent

`twyx events --kind agent` prints one JSON line each time an agent session changes state:

```json
{"event":"agent","data":{"pane":12,"agent":{"kind":"claude","state":"AwaitingApproval","source":"Integration","observed_ms":1792517230732,"session":null}}}
```

`state` is `Working`, `AwaitingApproval`, `AwaitingPrompt`, `Unknown` or `None`. Read lines until the pane you wait for shows the state you want, then stop reading:

```sh
twyx events --kind agent | grep -m1 '"pane":12,"agent":{[^}]*"state":"AwaitingPrompt"'
```

`twyx inbox` lists the agent sessions and panes that need the user now, newest first.

Never answer another agent's approval prompt for the user.

## Run checks

Checks are commands configured for a worktree, such as tests. Run them inside the worktree:

```sh
twyx check list                 # the checks and their commands
twyx check run test             # waits; exits non-zero unless it passes
twyx check run test --json      # the result as JSON
twyx check status               # the last result of each check
```

A result is `pass`, `fail`, `inconclusive` (the files changed during the run), `timed out` or `cancelled`. A pass or fail shows as `stale` when the files changed after the run: run it again. A pass is evidence, not proof: it says the command exited 0 on that source.

A check from the repository's `.twyx/checks.toml` runs only after the user trusts that file. Do not trust it for the user with `--yes`; ask them.

## Review and ship

- `twyx review status` lists the changed files since the base, with review and index state.
- `twyx review stage <path>`, `twyx review unstage <path>` and `twyx review commit -m "message"` stage and commit. Commit hooks run in a pane.
- `twyx review push` and `twyx review pr --title "..."` push and open a pull request. Do these only when the user asks.

## Use the API

Programs can send JSON requests to the daemon socket directly. The twyx documentation describes API v1 in `docs/api.md`.
