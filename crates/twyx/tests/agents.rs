//! Agent sessions end to end: fake agents in real panes, their kinds from
//! user rule files, and their state from the screen, the terminal and
//! `twyx notify`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use twyx_core::agent::{AgentState, Source};
use twyx_core::layout::Axis;
use twyx_protocol::client::{Launch, PaneState, Place, ToDaemon};

mod support;

use support::{Client, Sandbox, new_pane_at};

/// A fake agent that answers each line it reads. `TWYX` is the binary its
/// `notify` lines run. `hook-` lines report as a hook does, with the
/// session on stdin.
const FAKE_AGENT: &str = r#"#!/bin/sh
[ "$1" = --resume ] && printf 'resumed %s\n' "$2"
printf 'fake agent ready\n> '
while IFS= read -r line; do
  case "$line" in
    work) printf 'Working on it\n' ;;
    ask) printf 'Allow this? [y/n]\n' ;;
    busyask) "TWYX" notify working --agent fakeagent; sleep 1; printf 'Allow this? [y/n]\n' ;;
    notify-*) "TWYX" notify "${line#notify-}" --agent fakeagent ;;
    hook-*) printf '{"session_id":"s-42"}' |
      "TWYX" notify "${line#hook-}" --agent fakeagent --session-from-stdin session_id ;;
    quit) exit 0 ;;
  esac
  printf '> '
done
"#;

const FAKE_RULES: &str = r#"title = "Fake agent"
processes = ["fakeagent"]
command = ["fakeagent"]
resume = ["AGENT", "--resume", "{session}"]

[[screen]]
name = "question"
state = "awaiting-approval"
rows = 2
all = ['Allow this\? \[y/n\]']

[[screen]]
name = "busy"
state = "working"
rows = 2
all = ['^Working on it']

[[screen]]
name = "prompt"
state = "awaiting-prompt"
rows = 1
all = ['^>$']
"#;

fn install(sandbox: &Sandbox, kind: &str, rules: &str, script: &str) -> PathBuf {
    let agents = sandbox.root.join("config/twyx/agents");
    std::fs::create_dir_all(&agents).expect("create the agents directory");
    let bin = sandbox.root.join("bin");
    std::fs::create_dir_all(&bin).expect("create bin");
    let path = bin.join(kind);
    std::fs::write(
        agents.join(format!("{kind}.toml")),
        rules.replace("AGENT", &path.to_string_lossy()),
    )
    .expect("write the rule file");
    std::fs::write(&path, script.replace("TWYX", env!("CARGO_BIN_EXE_twyx")))
        .expect("write the agent");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

fn start(sandbox: &Sandbox, client: &mut Client, agent: &Path) -> u64 {
    new_pane_at(
        client,
        &sandbox.cwd(),
        Launch::Command(vec![agent.to_string_lossy().into_owned()]),
        Place::Split(Axis::Row),
    )
}

fn agent_of(client: &Client, pane: u64) -> Option<(String, AgentState, Source)> {
    let agent = client.panes.get(&pane)?.agent.as_ref()?;
    Some((agent.kind.clone(), agent.state, agent.source))
}

fn wait_for_state(client: &mut Client, pane: u64, state: AgentState, source: Source) {
    client.wait_until(
        &format!("{} from {}", state.name(), source.name()),
        |client| {
            agent_of(client, pane).is_some_and(|(_, seen, from)| seen == state && from == source)
        },
    );
}

fn pane_list(sandbox: &Sandbox) -> String {
    let output = sandbox
        .command(&["pane", "list"])
        .stdout(Stdio::piped())
        .output()
        .expect("run twyx pane list");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn an_agent_session_starts_with_its_process_follows_its_screen_and_ends_with_it() {
    let mut sandbox = Sandbox::new("ascreen");
    let agent = install(&sandbox, "fakeagent", FAKE_RULES, FAKE_AGENT);
    sandbox.start();
    let mut client = sandbox.connect(60, 10);
    let pane = start(&sandbox, &mut client, &agent);

    wait_for_state(
        &mut client,
        pane,
        AgentState::AwaitingPrompt,
        Source::Screen,
    );
    assert_eq!(agent_of(&client, pane).unwrap().0, "fakeagent");

    client.type_text("work\r");
    wait_for_state(&mut client, pane, AgentState::Working, Source::Screen);
    client.type_text("ask\r");
    wait_for_state(
        &mut client,
        pane,
        AgentState::AwaitingApproval,
        Source::Screen,
    );
    client.type_text("y\r");
    wait_for_state(
        &mut client,
        pane,
        AgentState::AwaitingPrompt,
        Source::Screen,
    );

    client.type_text("quit\r");
    client.wait_until("the session to end with the process", |client| {
        client
            .panes
            .get(&pane)
            .is_some_and(|info| info.agent.is_none())
    });
}

#[test]
fn an_integration_outranks_the_screen_until_the_user_sends_a_key() {
    let mut sandbox = Sandbox::new("anotify");
    let agent = install(&sandbox, "fakeagent", FAKE_RULES, FAKE_AGENT);
    sandbox.start();
    let mut client = sandbox.connect(60, 10);
    let pane = start(&sandbox, &mut client, &agent);
    wait_for_state(
        &mut client,
        pane,
        AgentState::AwaitingPrompt,
        Source::Screen,
    );

    client.type_text("busyask\r");
    wait_for_state(&mut client, pane, AgentState::Working, Source::Integration);
    client.wait_for_text(pane, "Allow this?");
    // Past the screen's settle time, so the holder has read it.
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        pane_list(&sandbox).contains("fakeagent working (integration)"),
        "the screen's approval overrode the integration: {}",
        pane_list(&sandbox)
    );

    // The user answered what the integration reported; the screen decides
    // until the integration reports again.
    client.type_text("n");
    wait_for_state(
        &mut client,
        pane,
        AgentState::AwaitingApproval,
        Source::Screen,
    );

    client.type_text("\rhook-awaiting-approval\r");
    wait_for_state(
        &mut client,
        pane,
        AgentState::AwaitingApproval,
        Source::Integration,
    );
    assert_eq!(
        client.panes[&pane]
            .agent
            .as_ref()
            .unwrap()
            .session
            .as_deref(),
        Some("s-42"),
        "the session reference from the hook's stdin"
    );
}

#[test]
fn after_a_host_restart_an_agent_pane_resumes_its_session() {
    let mut sandbox = Sandbox::new("aresume");
    let agent = install(&sandbox, "fakeagent", FAKE_RULES, FAKE_AGENT);
    sandbox.start();
    let mut client = sandbox.connect(60, 10);
    let pane = start(&sandbox, &mut client, &agent);
    wait_for_state(
        &mut client,
        pane,
        AgentState::AwaitingPrompt,
        Source::Screen,
    );
    client.type_text("hook-session\r");
    client.wait_until("the session reference", |client| {
        client.panes[&pane]
            .agent
            .as_ref()
            .is_some_and(|agent| agent.session.as_deref() == Some("s-42"))
    });
    drop(client);

    // A host restart ends the daemon, the holder and the agent.
    sandbox.kill();
    let socket = sandbox.root.join("run/twyx/panes").join(format!(
        "{}.sock",
        twyx_core::pane::PaneId::from_raw(pane).stem()
    ));
    for process in [socket.as_path(), agent.as_path()] {
        let _ = std::process::Command::new("pkill")
            .args(["-9", "-f"])
            .arg(process)
            .status();
    }
    sandbox.start();
    let mut client = sandbox.connect(60, 10);
    client.wait_until("the ended pane to offer Resume", |client| {
        client
            .panes
            .get(&pane)
            .is_some_and(|info| info.resume.as_deref() == Some("fakeagent"))
    });
    assert_eq!(client.panes[&pane].state, PaneState::Lost);
    assert!(
        client.panes[&pane].agent.is_none(),
        "nothing runs without a user action"
    );

    client
        .send(&ToDaemon::Resume { panes: vec![pane] })
        .expect("resume");
    client.wait_until("a new pane in the leaf", |client| {
        !client.panes.contains_key(&pane) && client.panes.len() == 1
    });
    let resumed = *client.panes.keys().next().unwrap();
    client.wait_for_text(resumed, "resumed s-42");
    client.wait_until("the resumed session to keep its reference", |client| {
        client.panes[&resumed].agent.as_ref().is_some_and(|agent| {
            agent.kind == "fakeagent" && agent.session.as_deref() == Some("s-42")
        })
    });
}

#[test]
fn invariant_15_output_silence_never_sets_a_state_other_than_unknown() {
    let mut sandbox = Sandbox::new("aquiet");
    let rules =
        "title = \"Quiet agent\"\nprocesses = [\"quietagent\"]\ncommand = [\"quietagent\"]\n";
    // A prompt mark once the session has started, then nothing at all.
    let script = "#!/bin/sh\nsleep 2\nprintf '\\033]133;C\\007'\nwhile :; do sleep 1; done\n";
    let agent = install(&sandbox, "quietagent", rules, script);
    sandbox.start();
    let mut client = sandbox.connect(60, 10);
    let pane = start(&sandbox, &mut client, &agent);

    client.wait_until("the session to start", |client| {
        agent_of(client, pane).is_some()
    });
    assert_eq!(
        agent_of(&client, pane).map(|(_, state, source)| (state, source)),
        Some((AgentState::Unknown, Source::Process))
    );

    wait_for_state(&mut client, pane, AgentState::Working, Source::Terminal);
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        pane_list(&sandbox).contains("quietagent working (terminal)"),
        "silence changed the state: {}",
        pane_list(&sandbox)
    );
}
