# API v1

The twyx daemon takes JSON requests on its socket. Agents, scripts and editors use this API to drive twyx. The `twyx` CLI is its reference client: each CLI subcommand that talks to the daemon uses the methods below, and `--json` prints their results.

## Connect

The socket is `$XDG_RUNTIME_DIR/twyx/twyx.sock`, or `/tmp/twyx-<uid>/twyx.sock` when `XDG_RUNTIME_DIR` is not set. Inside a twyx pane, `TWYX_SOCKET` holds the path. Only your own user can connect.

Any process of your user that can reach the socket can run commands as you, outside any agent sandbox. An agent sandbox that allows the socket contains nothing.

The daemon must run. `twyx daemon start` starts it, and any other `twyx` subcommand starts it too.

## Lines

Each message is one JSON object on one line.

A request:

```json
{"id": 1, "method": "check.run", "params": {"worktree": 3, "name": "test"}}
```

The answer to it:

```json
{"id": 1, "result": {"pane": 12}}
```

Or, when the method fails:

```json
{"id": 1, "error": {"message": "no check named \"test\" in this worktree"}}
```

- `id` is a number you choose. The answer carries the same `id`.
- `params` can be left out when a method needs none.
- The daemon answers the requests of one connection one at a time, in the order they arrive.
- The first request must be `hello`.

## hello

```json
{"id": 1, "method": "hello", "params": {"version": 1}}
```

The result has `version`, `minor`, `build`, `pid` and `clients`, the number of interactive clients attached. A daemon that does not speak the version answers with an error and closes the connection.

## Methods

IDs come from earlier results: `open` and `overview` give workspace, project and worktree IDs; `pane.list` gives pane IDs.

| Method | Params | Result |
|---|---|---|
| `daemon.status` | | `pid`, `build`, `legacy` holders |
| `daemon.stop` | | none; the connection closes. Panes keep running. |
| `daemon.kill_legacy` | | `null` |
| `open` | `path`, `switch` (bool) | `workspace`, `client`: the project of `path` opens; with `switch`, the client that last had input shows it |
| `overview` | `refresh` (bool) | projects, worktrees with branch and dirty state, workspaces |
| `workspace.switch` | `workspace` | `workspace`, `client` |
| `tab.list` | `workspace` | the workspace's tabs |
| `tab.select` | `tab` | `null`; the client that last had input shows the tab |
| `pane.list` | | every pane, with its state and agent session |
| `pane.new` | `cwd`, `argv` (empty: your shell), `place` (`right`, `down`, `stack`, `float`, `tab`), `history` (bool) | the new pane |
| `pane.send` | `pane`, `text`, `paste` (bool), `keys` (names such as `enter`, `ctrl-c`) | `null` |
| `pane.focus` | `pane` | `null`; a pane without a leaf gets a tab |
| `pane.kill` | `pane` | `null` |
| `worktree.new` | `project`, `branch`, `start` | `worktree`, `workspace`, `setup` |
| `worktree.plan_removal` | `worktree` | what removal would lose, with a `plan` ID |
| `worktree.remove` | `plan`, `delete_branch` (bool) | `worktree`, `branch` |
| `worktree.setup_file` | `worktree` | the setup file and its hash, or `null` |
| `worktree.trust_setup` | `worktree`, `hash` | `null` |
| `worktree.run_setup` | `worktree` | what setup copied, linked and ran |
| `recipe.list` | `workspace` | the recipes |
| `recipe.run` | `workspace`, `name`, `branch` (a new worktree), `switch` | `workspace`, `tabs`, `client` |
| `recipe.trust` | `workspace`, `name`, `hash` | `null` |
| `task.new` | `workspace`, `name`, `start`, `agents` (one kind or `null` per worktree), `prompt` | one entry per new worktree |
| `inbox.list` | | the inbox, newest first |
| `inbox.mark_unread` | `item` | `null` |
| `inbox.dismiss` | `item` | `null` |
| `review.status` | `worktree` | the base and each changed file, with its diff and index state |
| `review.open` | `worktree` | `null`; the client that last had input shows the review |
| `review.mark` | `worktree`, `path`, `hunks` (IDs; the whole file without them) | the review |
| `review.stage` | `worktree`, `path` (every change without it), `hunks`, `unstage` (bool) | the Git command and its output |
| `review.stage_reviewed` | `worktree` | the Git command and its output |
| `review.commit` | `worktree`, `message`, `wait` (bool) | the command, its pane and, with `wait`, its output and result |
| `review.push` | `worktree`, `wait` (bool) | as `review.commit` |
| `review.pull_request` | `worktree`, `title`, `body`, `wait` (bool) | as `review.commit`; the output has the URL |
| `notes.list` | `worktree` | the review notes |
| `notes.add` | `worktree`, `path`, `lines` (`[first, last]`), `body` | the review notes |
| `notes.send` | `worktree`, `pane` | `pane`, `result`: pasted, or why it went to the clipboard |
| `check.list` | `worktree` | the checks, each with its last run and `stale` |
| `check.run` | `worktree`, `name`, `wait` (bool) | `pane`; with `wait`, the finished run |
| `check.cancel` | `worktree`, `name` | `null` |
| `check.trust` | `worktree`, `hash` | the checks |
| `notify` | `pane`, `agent`, `event` (`Working`, `AwaitingApproval`, `AwaitingPrompt`, `TurnFinished`, `Error`, `Session`), `session` | `null` |
| `history.purge` | | `null` |
| `events.subscribe` | `kinds` (empty: all) | `null`; events follow |

twyx never commits, pushes or opens a pull request unless a client calls the method for it.

## Events

After `events.subscribe`, the daemon also writes event lines:

```json
{"event": "agent", "data": {"pane": 12, "agent": {"kind": "claude", "state": "AwaitingApproval", "source": "Integration", "observed_ms": 1792517230732, "session": "6f1c…"}}}
```

| Kind | Events | Data |
|---|---|---|
| `agent` | `agent` | `pane`, and its agent session, or `null` once it ends |
| `pane` | `pane`, `pane.closed` | the pane; the ID of a closed pane |
| `workspace` | `workspace` | a workspace's tabs and layout |
| `overview` | `overview` | the overview, after each change |
| `inbox` | `inbox` | the inbox, after each change |
| `check` | `check.started`, `check.ended` | `worktree`, `name`, `pane`; the finished run. `check.started` comes only for runs this connection starts. |
| `ship` | `ship` | a commit, push or pull request step, as it starts and as it ends |

An agent's `state` is `Working`, `AwaitingApproval`, `AwaitingPrompt`, `Unknown` or `None`. Its `source` says where the state came from: `Integration`, `Screen`, `Terminal` or `Process`.

An event can arrive between a request and its answer. A client that reads answers must keep the events it reads.

`twyx events --kind agent` prints the same events, one line each.

## Check results

A run's `outcome` is `Pass`, `Fail`, `Inconclusive` (the source changed during the run), `TimedOut` or `Cancelled`. `check.list` marks a pass or a fail `stale` once the worktree's source changes after it. A pass is evidence, not proof: the command exited 0 on that source.

## Examples

The examples use Python with only its standard library. Any language that can open a Unix socket and read lines works.

```python
import json, os, socket

def socket_path():
    if os.environ.get("TWYX_SOCKET"):
        return os.environ["TWYX_SOCKET"]
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if runtime:
        return os.path.join(runtime, "twyx", "twyx.sock")
    return f"/tmp/twyx-{os.getuid()}/twyx.sock"

class Twyx:
    def __init__(self):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(socket_path())
        self.lines = self.sock.makefile("r")
        self.next = 0
        self.events = []
        self.call("hello", version=1)

    def call(self, method, **params):
        self.next += 1
        self.sock.sendall((json.dumps({"id": self.next, "method": method, "params": params}) + "\n").encode())
        while True:
            line = json.loads(self.lines.readline())
            if "event" in line:
                self.events.append(line)
            elif line["id"] == self.next:
                if "error" in line:
                    raise RuntimeError(line["error"]["message"])
                return line["result"]

    def event(self):
        if self.events:
            return self.events.pop(0)
        return json.loads(self.lines.readline())
```

### Open a worktree with a recipe

This opens the repository at `~/src/app`, makes a worktree on a new branch `fix-login`, and opens the recipe `editor-agent-tests` in it.

```python
twyx = Twyx()
opened = twyx.call("open", path=os.path.expanduser("~/src/app"))
done = twyx.call("recipe.run", workspace=opened["workspace"],
                 name="editor-agent-tests", branch="fix-login", switch=True)
print("tabs", done["tabs"], "in workspace", done["workspace"])
```

A repository recipe runs only after you trust it. `recipe.list` gives its `hash`, and `recipe.trust` trusts that content. Show the recipe to the person first.

### Wait until an agent needs input

```python
twyx = Twyx()
twyx.call("events.subscribe", kinds=["agent"])
while True:
    event = twyx.event()
    agent = event["data"]["agent"]
    if agent and agent["state"] in ("AwaitingApproval", "AwaitingPrompt"):
        print("pane", event["data"]["pane"], agent["kind"], agent["state"])
        break
```

The inbox gives the same answer for agents that already wait: `inbox.list`.

### Run checks and read the result

```python
twyx = Twyx()
here = twyx.call("open", path=os.getcwd())
overview = twyx.call("overview")
worktree = next(w["worktree"] for w in overview["workspaces"]
                if w["workspace"] == here["workspace"])
for check in twyx.call("check.list", worktree=worktree)["checks"]:
    run = twyx.call("check.run", worktree=worktree, name=check["check"]["name"], wait=True)
    print(run["name"], run["outcome"], "exit", run["exit"])
    print(run["tail"])
```

From a shell, `twyx check run test --json` does the same for one check and exits non-zero unless it passes.
