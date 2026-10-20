//! API v1 connections: JSON lines in, JSON lines out. Each connection is a
//! command client inside the daemon; each method becomes client protocol
//! requests and waits for their answer, one method at a time, in order.

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Sender};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use twyx_protocol::api::{self, Event as ApiEvent, Line, Reply, Request};
use twyx_protocol::client::{
    ClientHello, FromDaemon, Launch, MAJOR, MINOR, PaneInfo, ServerHello, ToDaemon,
};
use twyx_protocol::input::{InputEvent, KeyCode, KeyEvent, Modifiers};

use crate::daemon::Event;

/// Takes each daemon message for a method until it gives the answer.
type Matcher = Box<dyn FnMut(&FromDaemon) -> Option<Result<Value, String>> + Send>;

enum Start {
    Send(Vec<ToDaemon>, Matcher),
    Now(Value),
}

enum Input {
    Request(Request),
    Bad(String),
    Daemon(Box<FromDaemon>),
    Closed,
}

fn json_of(value: impl Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

fn params<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    let value = if value.is_null() { json!({}) } else { value };
    serde_json::from_value(value).map_err(|error| format!("bad params: {error}"))
}

/// Answers with what `pick` takes from the first message it takes.
fn reply<T: Serialize>(pick: impl Fn(&FromDaemon) -> Option<T> + Send + 'static) -> Matcher {
    Box::new(move |message| pick(message).map(json_of))
}

struct Session {
    events: Sender<Event>,
    writer: BufWriter<UnixStream>,
    hello: ServerHello,
    /// Every pane, as the daemon last described it.
    panes: BTreeMap<u64, PaneInfo>,
    /// The event kinds asked for; `None` before `events.subscribe`.
    kinds: Option<Vec<String>>,
    next_token: u64,
}

/// Serves one API connection whose first line is waiting in `reader`.
pub(crate) fn serve(
    stream: UnixStream,
    mut reader: BufReader<UnixStream>,
    events: Sender<Event>,
    client: u64,
) {
    let Ok(write_stream) = stream.try_clone() else {
        return;
    };
    let mut writer = BufWriter::new(write_stream);
    let mut first = String::new();
    if reader.read_line(&mut first).unwrap_or(0) == 0 {
        return;
    }
    let hello = serde_json::from_str::<Request>(&first)
        .map_err(|error| (0, format!("not a request: {error}")))
        .and_then(|request| {
            if request.method != "hello" {
                return Err((request.id, "the first request must be hello".to_owned()));
            }
            let hello: api::Hello = params(request.params).map_err(|error| (request.id, error))?;
            if hello.version != api::VERSION {
                return Err((
                    request.id,
                    format!("this daemon speaks API version {}", api::VERSION),
                ));
            }
            Ok(request.id)
        });
    let hello_id = match hello {
        Ok(id) => id,
        Err((id, message)) => {
            let _ = write_line(&mut writer, &Line::Reply(failure(id, message)));
            return;
        }
    };

    let (out, from_daemon) = mpsc::channel::<FromDaemon>();
    let (input, inputs) = mpsc::channel::<Input>();
    if events
        .send(Event::ClientHello {
            client,
            hello: ClientHello {
                major: MAJOR,
                minor: MINOR,
                build: twyx_protocol::BUILD_ID.to_owned(),
                cols: 80,
                rows: 24,
                interactive: false,
                workspace: None,
            },
            out,
        })
        .is_err()
    {
        return;
    }
    let server = match from_daemon.recv() {
        Ok(FromDaemon::Welcome(hello)) => hello,
        _ => return,
    };
    let daemon_input = input.clone();
    std::thread::spawn(move || {
        for message in from_daemon {
            if daemon_input.send(Input::Daemon(Box::new(message))).is_err() {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        for line in reader.lines() {
            let Ok(line) = line else {
                break;
            };
            if line.trim().is_empty() {
                continue;
            }
            let sent = match serde_json::from_str::<Request>(&line) {
                Ok(request) => input.send(Input::Request(request)),
                Err(error) => input.send(Input::Bad(format!("not a request: {error}"))),
            };
            if sent.is_err() {
                break;
            }
        }
        let _ = input.send(Input::Closed);
    });

    let mut session = Session {
        events: events.clone(),
        writer,
        panes: server
            .panes
            .iter()
            .map(|pane| (pane.pane, pane.clone()))
            .collect(),
        hello: server,
        kinds: None,
        next_token: 0,
    };
    let welcome = api::Welcome {
        version: api::VERSION,
        minor: api::MINOR,
        build: session.hello.build.clone(),
        pid: session.hello.pid,
        clients: session.hello.clients,
    };
    let welcome = json_of(welcome).unwrap_or(Value::Null);
    if session.answer(hello_id, Ok(welcome)).is_err() {
        let _ = events.send(Event::ClientGone { client });
        return;
    }

    let mut queue: VecDeque<Request> = VecDeque::new();
    let mut pending: Option<(u64, Matcher)> = None;
    for input in inputs {
        let alive = match input {
            Input::Request(request) => {
                queue.push_back(request);
                Ok(())
            }
            Input::Bad(message) => session.answer(0, Err(message)),
            Input::Daemon(message) => {
                let message = *message;
                let shown = session.event(&message);
                let answered = match &mut pending {
                    Some((id, _)) if matches!(message, FromDaemon::Refused { .. }) => {
                        let FromDaemon::Refused { reason } = &message else {
                            unreachable!()
                        };
                        Some((*id, Err(reason.clone())))
                    }
                    Some((id, matcher)) => matcher(&message).map(|result| (*id, result)),
                    None => None,
                };
                match answered {
                    Some((id, result)) => {
                        pending = None;
                        shown.and_then(|()| session.answer(id, result))
                    }
                    None => shown,
                }
            }
            Input::Closed => break,
        };
        if alive.is_err() {
            break;
        }
        while pending.is_none() {
            let Some(request) = queue.pop_front() else {
                break;
            };
            match session.start(&request.method, request.params) {
                Ok(Start::Now(value)) => {
                    if session.answer(request.id, Ok(value)).is_err() {
                        break;
                    }
                }
                Ok(Start::Send(messages, matcher)) => {
                    for message in messages {
                        let _ = session
                            .events
                            .send(Event::ClientMessage { client, message });
                    }
                    pending = Some((request.id, matcher));
                }
                Err(error) => {
                    if session.answer(request.id, Err(error)).is_err() {
                        break;
                    }
                }
            }
        }
    }
    let _ = events.send(Event::ClientGone { client });
}

fn failure(id: u64, message: String) -> Reply {
    Reply {
        id,
        result: None,
        error: Some(api::Error { message }),
    }
}

fn write_line(writer: &mut BufWriter<UnixStream>, line: &Line) -> std::io::Result<()> {
    let mut text = serde_json::to_string(line).map_err(std::io::Error::other)?;
    text.push('\n');
    writer.write_all(text.as_bytes())?;
    writer.flush()
}

/// Text typed key by key: a line break is `enter`.
fn typed(text: &str) -> Vec<InputEvent> {
    text.chars()
        .map(|ch| {
            let code = match ch {
                '\n' | '\r' => KeyCode::Enter,
                '\t' => KeyCode::Tab,
                ch => KeyCode::Char(ch),
            };
            InputEvent::Key(KeyEvent::press(code, Modifiers::empty()))
        })
        .collect()
}

impl Session {
    fn answer(&mut self, id: u64, result: Result<Value, String>) -> std::io::Result<()> {
        let reply = match result {
            Ok(value) => Reply {
                id,
                result: Some(value),
                error: None,
            },
            Err(message) => failure(id, message),
        };
        write_line(&mut self.writer, &Line::Reply(reply))
    }

    /// Keeps the panes current and writes the events the client asked for.
    fn event(&mut self, message: &FromDaemon) -> std::io::Result<()> {
        let mut found: Vec<(&str, Value)> = Vec::new();
        match message {
            FromDaemon::Pane(info) => {
                let before = self.panes.insert(info.pane, info.clone());
                if before.and_then(|before| before.agent) != info.agent {
                    found.push(("agent", json!({ "pane": info.pane, "agent": info.agent })));
                }
                found.push(("pane", json_of(info).unwrap_or(Value::Null)));
            }
            FromDaemon::PaneClosed { pane } => {
                self.panes.remove(pane);
                found.push(("pane.closed", json!({ "pane": pane })));
            }
            FromDaemon::Workspace(info) => {
                found.push(("workspace", json_of(info).unwrap_or(Value::Null)))
            }
            FromDaemon::Overview(overview) => {
                found.push(("overview", json_of(overview).unwrap_or(Value::Null)));
            }
            FromDaemon::Inbox(items) => {
                found.push(("inbox", json_of(items).unwrap_or(Value::Null)))
            }
            FromDaemon::CheckStarted {
                worktree,
                name,
                pane,
            } => found.push((
                "check.started",
                json!({ "worktree": worktree, "name": name, "pane": pane }),
            )),
            FromDaemon::CheckEnded(run) => {
                found.push(("check.ended", json_of(run).unwrap_or(Value::Null)));
            }
            FromDaemon::Shipped(report) => {
                found.push(("ship", json_of(report).unwrap_or(Value::Null)))
            }
            _ => {}
        }
        let Some(kinds) = &self.kinds else {
            return Ok(());
        };
        for (name, data) in found {
            let kind = name.split('.').next().unwrap_or(name);
            if kinds.is_empty() || kinds.iter().any(|wanted| wanted == kind) {
                let event = Line::Event(ApiEvent {
                    event: name.to_owned(),
                    data,
                });
                write_line(&mut self.writer, &event)?;
            }
        }
        Ok(())
    }

    /// `message`, then a `Sync` whose answer says it was not refused.
    fn synced(&mut self, mut messages: Vec<ToDaemon>) -> Start {
        self.next_token += 1;
        let token = self.next_token;
        messages.push(ToDaemon::Sync { token });
        Start::Send(
            messages,
            reply(move |message| match message {
                FromDaemon::Synced { token: done } if *done == token => Some(Value::Null),
                _ => None,
            }),
        )
    }

    fn start(&mut self, method: &str, value: Value) -> Result<Start, String> {
        Ok(match method {
            "hello" => return Err("hello comes once, first".to_owned()),
            "daemon.status" => Start::Now(json_of(json!({
                "pid": self.hello.pid,
                "build": self.hello.build,
                "legacy": self.hello.legacy,
            }))?),
            "daemon.stop" => Start::Send(vec![ToDaemon::Shutdown], Box::new(|_| None)),
            "daemon.kill_legacy" => self.synced(vec![ToDaemon::KillLegacy]),
            "open" => {
                let open: api::Open = params(value)?;
                Start::Send(
                    vec![ToDaemon::Open {
                        path: open.path,
                        switch: open.switch,
                    }],
                    reply(|message| match message {
                        FromDaemon::Opened { workspace, client } => {
                            Some(json!({ "workspace": workspace, "client": client }))
                        }
                        _ => None,
                    }),
                )
            }
            "overview" => {
                let asked: api::OverviewParams = params(value)?;
                Start::Send(
                    vec![ToDaemon::Overview {
                        refresh: asked.refresh,
                    }],
                    reply(|message| match message {
                        FromDaemon::Overview(overview) => Some(overview.clone()),
                        _ => None,
                    }),
                )
            }
            "workspace.switch" => {
                let asked: api::WorkspaceRef = params(value)?;
                Start::Send(
                    vec![ToDaemon::SwitchWorkspace {
                        workspace: asked.workspace,
                    }],
                    reply(|message| match message {
                        FromDaemon::Switched { workspace, client } => {
                            Some(json!({ "workspace": workspace, "client": client }))
                        }
                        _ => None,
                    }),
                )
            }
            "tab.list" => {
                let asked: api::WorkspaceRef = params(value)?;
                let workspace = asked.workspace;
                Start::Send(
                    vec![ToDaemon::WorkspaceInfo { workspace }],
                    reply(move |message| match message {
                        FromDaemon::Workspace(info) if info.workspace == workspace => {
                            Some(info.tabs.clone())
                        }
                        _ => None,
                    }),
                )
            }
            "tab.select" => {
                let asked: api::TabRef = params(value)?;
                self.synced(vec![ToDaemon::ShowTab { tab: asked.tab }])
            }
            "pane.list" => Start::Now(json_of(self.panes.values().collect::<Vec<_>>())?),
            "pane.new" => {
                let asked: api::NewPane = params(value)?;
                let known: Vec<u64> = self.panes.keys().copied().collect();
                Start::Send(
                    vec![ToDaemon::NewPane {
                        launch: if asked.argv.is_empty() {
                            Launch::Shell
                        } else {
                            Launch::Command(asked.argv)
                        },
                        cwd: asked.cwd,
                        place: asked.place.place(),
                        history: asked.history,
                    }],
                    reply(move |message| match message {
                        FromDaemon::Pane(info) if !known.contains(&info.pane) => Some(info.clone()),
                        _ => None,
                    }),
                )
            }
            "pane.kill" => {
                let asked: api::PaneRef = params(value)?;
                self.synced(vec![ToDaemon::KillPane { pane: asked.pane }])
            }
            "pane.focus" => {
                let asked: api::PaneRef = params(value)?;
                self.synced(vec![ToDaemon::ShowPane { pane: asked.pane }])
            }
            "pane.send" => {
                let asked: api::SendToPane = params(value)?;
                let mut input = match (asked.text, asked.paste) {
                    (Some(text), true) => vec![InputEvent::Paste(text)],
                    (Some(text), false) => typed(&text),
                    (None, _) => Vec::new(),
                };
                for name in &asked.keys {
                    input.push(InputEvent::Key(api::key(name)?));
                }
                if input.is_empty() {
                    return Err("pane.send needs text or keys".to_owned());
                }
                self.synced(vec![ToDaemon::SendToPane {
                    pane: asked.pane,
                    input,
                }])
            }
            "worktree.new" => {
                let asked: api::NewWorktree = params(value)?;
                Start::Send(
                    vec![ToDaemon::NewWorktree {
                        project: asked.project,
                        branch: asked.branch,
                        start: asked.start,
                    }],
                    reply(|message| match message {
                        FromDaemon::WorktreeAdded {
                            worktree,
                            workspace,
                            setup,
                        } => Some(json!({
                            "worktree": worktree,
                            "workspace": workspace,
                            "setup": setup,
                        })),
                        _ => None,
                    }),
                )
            }
            "worktree.plan_removal" => {
                let asked: api::WorktreeRef = params(value)?;
                Start::Send(
                    vec![ToDaemon::PlanRemoval {
                        worktree: asked.worktree,
                    }],
                    reply(|message| match message {
                        FromDaemon::RemovalPlan(plan) => Some(plan.clone()),
                        _ => None,
                    }),
                )
            }
            "worktree.remove" => {
                let asked: api::RemoveWorktree = params(value)?;
                Start::Send(
                    vec![ToDaemon::RemoveWorktree {
                        plan: asked.plan,
                        delete_branch: asked.delete_branch,
                    }],
                    reply(|message| match message {
                        FromDaemon::WorktreeRemoved { worktree, branch } => {
                            Some(json!({ "worktree": worktree, "branch": branch }))
                        }
                        _ => None,
                    }),
                )
            }
            "worktree.setup_file" => {
                let asked: api::WorktreeRef = params(value)?;
                Start::Send(
                    vec![ToDaemon::Setup {
                        worktree: asked.worktree,
                    }],
                    reply(|message| match message {
                        FromDaemon::SetupFile(info) => Some(info.clone()),
                        _ => None,
                    }),
                )
            }
            "worktree.trust_setup" => {
                let asked: api::TrustFile = params(value)?;
                Start::Send(
                    vec![ToDaemon::TrustSetup {
                        worktree: asked.worktree,
                        hash: asked.hash,
                    }],
                    reply(|message| match message {
                        FromDaemon::SetupTrusted { .. } => Some(Value::Null),
                        _ => None,
                    }),
                )
            }
            "worktree.run_setup" => {
                let asked: api::WorktreeRef = params(value)?;
                Start::Send(
                    vec![ToDaemon::RunSetup {
                        worktree: asked.worktree,
                    }],
                    reply(|message| match message {
                        FromDaemon::SetupDone { report, .. } => Some(report.clone()),
                        _ => None,
                    }),
                )
            }
            "recipe.list" => {
                let asked: api::WorkspaceRef = params(value)?;
                Start::Send(
                    vec![ToDaemon::Recipes {
                        workspace: asked.workspace,
                    }],
                    reply(|message| match message {
                        FromDaemon::Recipes(recipes) => Some(recipes.clone()),
                        _ => None,
                    }),
                )
            }
            "recipe.run" => {
                let asked: api::RunRecipe = params(value)?;
                Start::Send(
                    vec![ToDaemon::RunRecipe {
                        workspace: asked.workspace,
                        name: asked.name,
                        branch: asked.branch,
                        switch: asked.switch,
                    }],
                    reply(|message| match message {
                        FromDaemon::RecipeOpened {
                            workspace,
                            tabs,
                            client,
                        } => {
                            Some(json!({ "workspace": workspace, "tabs": tabs, "client": client }))
                        }
                        _ => None,
                    }),
                )
            }
            "recipe.trust" => {
                let asked: api::TrustRecipe = params(value)?;
                Start::Send(
                    vec![ToDaemon::TrustRecipe {
                        workspace: asked.workspace,
                        name: asked.name,
                        hash: asked.hash,
                    }],
                    reply(|message| match message {
                        FromDaemon::RecipeTrusted { .. } => Some(Value::Null),
                        _ => None,
                    }),
                )
            }
            "task.new" => {
                let asked: api::NewTask = params(value)?;
                let mut left = asked.agents.len().max(1);
                let mut added = Vec::new();
                Start::Send(
                    vec![ToDaemon::NewTask {
                        workspace: asked.workspace,
                        name: asked.name,
                        start: asked.start,
                        agents: asked.agents,
                        prompt: asked.prompt,
                    }],
                    Box::new(move |message| {
                        let FromDaemon::WorktreeAdded {
                            worktree,
                            workspace,
                            setup,
                        } = message
                        else {
                            return None;
                        };
                        added.push(json!({
                            "worktree": worktree,
                            "workspace": workspace,
                            "setup": setup,
                        }));
                        left -= 1;
                        (left == 0).then(|| Ok(Value::Array(std::mem::take(&mut added))))
                    }),
                )
            }
            "inbox.list" => Start::Send(
                vec![ToDaemon::Inbox],
                reply(|message| match message {
                    FromDaemon::Inbox(items) => Some(items.clone()),
                    _ => None,
                }),
            ),
            "inbox.mark_unread" => {
                let asked: api::ItemRef = params(value)?;
                self.synced(vec![ToDaemon::MarkUnread { item: asked.item }])
            }
            "inbox.dismiss" => {
                let asked: api::ItemRef = params(value)?;
                self.synced(vec![ToDaemon::Dismiss { item: asked.item }])
            }
            "review.status" => {
                let asked: api::WorktreeRef = params(value)?;
                review(ToDaemon::Review {
                    worktree: asked.worktree,
                })
            }
            "review.open" => {
                let asked: api::WorktreeRef = params(value)?;
                self.synced(vec![ToDaemon::OpenReview {
                    worktree: Some(asked.worktree),
                }])
            }
            "review.mark" => {
                let asked: api::MarkReviewed = params(value)?;
                review(ToDaemon::MarkReviewed {
                    worktree: asked.worktree,
                    path: asked.path,
                    hunks: asked.hunks,
                })
            }
            "review.stage" => {
                let asked: api::Stage = params(value)?;
                shipped(
                    ToDaemon::Stage {
                        worktree: asked.worktree,
                        path: asked.path,
                        hunks: asked.hunks,
                        unstage: asked.unstage,
                    },
                    false,
                )
            }
            "review.stage_reviewed" => {
                let asked: api::WorktreeRef = params(value)?;
                shipped(
                    ToDaemon::StageReviewed {
                        worktree: asked.worktree,
                    },
                    false,
                )
            }
            "review.commit" => {
                let asked: api::Commit = params(value)?;
                shipped(
                    ToDaemon::Commit {
                        worktree: asked.worktree,
                        message: asked.message,
                    },
                    asked.wait,
                )
            }
            "review.push" => {
                let asked: api::Push = params(value)?;
                shipped(
                    ToDaemon::Push {
                        worktree: asked.worktree,
                    },
                    asked.wait,
                )
            }
            "review.pull_request" => {
                let asked: api::PullRequest = params(value)?;
                shipped(
                    ToDaemon::OpenPullRequest {
                        worktree: asked.worktree,
                        title: asked.title,
                        body: asked.body,
                    },
                    asked.wait,
                )
            }
            "notes.list" => {
                let asked: api::WorktreeRef = params(value)?;
                notes(ToDaemon::Notes {
                    worktree: asked.worktree,
                })
            }
            "notes.add" => {
                let asked: api::AddNote = params(value)?;
                notes(ToDaemon::AddNote {
                    worktree: asked.worktree,
                    path: asked.path,
                    lines: asked.lines,
                    body: asked.body,
                })
            }
            "notes.send" => {
                let asked: api::SendNotes = params(value)?;
                Start::Send(
                    vec![ToDaemon::SendNotes {
                        worktree: asked.worktree,
                        pane: asked.pane,
                    }],
                    reply(|message| match message {
                        FromDaemon::Delivered { pane, result } => {
                            Some(json!({ "pane": pane, "result": result }))
                        }
                        _ => None,
                    }),
                )
            }
            "check.list" => {
                let asked: api::WorktreeRef = params(value)?;
                checks(ToDaemon::Checks {
                    worktree: asked.worktree,
                })
            }
            "check.trust" => {
                let asked: api::TrustFile = params(value)?;
                checks(ToDaemon::TrustChecks {
                    worktree: asked.worktree,
                    hash: asked.hash,
                })
            }
            "check.cancel" => {
                let asked: api::CheckRef = params(value)?;
                self.synced(vec![ToDaemon::CancelCheck {
                    worktree: asked.worktree,
                    name: asked.name,
                }])
            }
            "check.run" => {
                let asked: api::CheckRef = params(value)?;
                let (worktree, name, wait) = (asked.worktree, asked.name.clone(), asked.wait);
                let mut started: Option<u64> = None;
                Start::Send(
                    vec![ToDaemon::RunCheck {
                        worktree: asked.worktree,
                        name: asked.name,
                    }],
                    Box::new(move |message| match message {
                        FromDaemon::CheckStarted {
                            worktree: at,
                            name: named,
                            pane,
                        } if started.is_none() && *at == worktree && *named == name => {
                            if !wait {
                                return Some(Ok(json!({ "pane": pane })));
                            }
                            started = Some(*pane);
                            None
                        }
                        FromDaemon::CheckEnded(run) if started.is_some() && run.pane == started => {
                            Some(json_of(run))
                        }
                        _ => None,
                    }),
                )
            }
            "notify" => {
                let asked: api::Notify = params(value)?;
                self.synced(vec![ToDaemon::Notify {
                    pane: asked.pane,
                    agent: asked.agent,
                    event: asked.event,
                    session: asked.session,
                }])
            }
            "history.purge" => self.synced(vec![ToDaemon::PurgeHistory]),
            "events.subscribe" => {
                let asked: api::Subscribe = params(value)?;
                if let Some(unknown) = asked
                    .kinds
                    .iter()
                    .find(|kind| !api::EVENT_KINDS.contains(&kind.as_str()))
                {
                    return Err(format!(
                        "no event kind {unknown:?}; the kinds are {}",
                        api::EVENT_KINDS.join(", ")
                    ));
                }
                self.kinds = Some(asked.kinds);
                self.synced(vec![ToDaemon::WatchOverview])
            }
            other => return Err(format!("no method {other:?}")),
        })
    }
}

fn review(request: ToDaemon) -> Start {
    Start::Send(
        vec![request],
        reply(|message| match message {
            FromDaemon::Review(review) => Some(review.clone()),
            _ => None,
        }),
    )
}

fn notes(request: ToDaemon) -> Start {
    Start::Send(
        vec![request],
        reply(|message| match message {
            FromDaemon::Notes { notes, .. } => Some(notes.clone()),
            _ => None,
        }),
    )
}

fn checks(request: ToDaemon) -> Start {
    Start::Send(
        vec![request],
        reply(|message| match message {
            FromDaemon::Checks(info) => Some(info.clone()),
            _ => None,
        }),
    )
}

/// A shipping step's report: when it starts, or with `wait` when its pane
/// exits. A step that could not start is an error.
fn shipped(request: ToDaemon, wait: bool) -> Start {
    let mut pane: Option<u64> = None;
    Start::Send(
        vec![request],
        Box::new(move |message| {
            let FromDaemon::Shipped(report) = message else {
                return None;
            };
            if pane.is_some() && report.pane != pane {
                return None;
            }
            match &report.result {
                Some(Err(error)) if report.pane.is_none() => Some(Err(error.clone())),
                None if wait => {
                    pane = report.pane;
                    None
                }
                _ => Some(json_of(report)),
            }
        }),
    )
}
