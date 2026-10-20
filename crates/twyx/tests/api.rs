//! API v1 end to end: JSON lines on the daemon socket.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use twyx_protocol::api::Client;
use twyx_protocol::client::{CheckRun, PaneInfo};

use support::Sandbox;

fn line(reader: &mut BufReader<UnixStream>) -> Value {
    let mut text = String::new();
    reader.read_line(&mut text).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn the_api_speaks_json_lines_and_answers_each_method_in_order() {
    let mut sandbox = Sandbox::new("api");
    std::fs::create_dir_all(sandbox.root.join("config/twyx")).unwrap();
    std::fs::write(
        sandbox.root.join("config/twyx/config.toml"),
        "[[check]]\nname = \"ok\"\ncommand = \"true\"\n",
    )
    .unwrap();
    sandbox.start();

    // The envelope, byte for byte, and the rules of the first line.
    let stream = UnixStream::connect(sandbox.socket()).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    writer
        .write_all(b"{\"id\":7,\"method\":\"overview\"}\n")
        .unwrap();
    assert_eq!(
        line(&mut reader),
        json!({"id": 7, "error": {"message": "the first request must be hello"}})
    );
    let stream = UnixStream::connect(sandbox.socket()).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    writer
        .write_all(b"{\"id\":1,\"method\":\"hello\",\"params\":{\"version\":1}}\n{\"id\":2,\"method\":\"nope\"}\n")
        .unwrap();
    let welcome = line(&mut reader);
    assert_eq!(welcome["id"], 1);
    assert_eq!(welcome["result"]["version"], 1);
    assert_eq!(
        line(&mut reader),
        json!({"id": 2, "error": {"message": "no method \"nope\""}})
    );

    let mut api = Client::connect(&sandbox.socket()).unwrap();
    let repo = sandbox.root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    };
    git(&["init", "--quiet", "-b", "main"]);
    git(&[
        "-c",
        "user.email=t@e",
        "-c",
        "user.name=t",
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
        "one",
    ]);
    let opened: Value = api
        .call("open", &json!({"path": repo.to_str().unwrap()}))
        .unwrap();
    let workspace = opened["workspace"].as_u64().unwrap();
    let overview: Value = api.call("overview", &json!({})).unwrap();
    let worktree = overview["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|each| each["workspace"] == workspace)
        .unwrap()["worktree"]
        .as_u64()
        .unwrap();

    let () = api
        .call("events.subscribe", &json!({"kinds": ["check"]}))
        .unwrap();
    let run: CheckRun = api
        .call(
            "check.run",
            &json!({"worktree": worktree, "name": "ok", "wait": true}),
        )
        .unwrap();
    assert_eq!(run.outcome, Some(twyx_core::check::Outcome::Pass));
    let started = api.event().unwrap();
    assert_eq!(started.event, "check.started");
    assert_eq!(api.event().unwrap().event, "check.ended");

    let missing = api
        .call::<Value>(
            "check.run",
            &json!({"worktree": worktree, "name": "missing"}),
        )
        .unwrap_err();
    assert!(missing.to_string().contains("no check named"), "{missing}");

    // Text typed into a pane, then a key, through the clients' queue.
    let out = repo.join("got.txt");
    let pane: PaneInfo = api
        .call(
            "pane.new",
            &json!({
                "argv": ["sh", "-c", "read line; echo \"got $line\" > got.txt"],
                "cwd": repo.to_str().unwrap(),
            }),
        )
        .unwrap();
    let () = api
        .call(
            "pane.send",
            &json!({"pane": pane.pane, "text": "hello", "keys": ["enter"]}),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_to_string(&out).unwrap_or_default() != "got hello\n" {
        assert!(Instant::now() < deadline, "the pane did not read the line");
        std::thread::sleep(Duration::from_millis(20));
    }
    let panes: Vec<PaneInfo> = api.call("pane.list", &json!({})).unwrap();
    assert!(panes.iter().any(|each| each.pane == pane.pane));
    let refused = api
        .call::<()>("pane.send", &json!({"pane": 999_999, "keys": ["enter"]}))
        .unwrap_err();
    assert!(refused.to_string().contains("not running"), "{refused}");
}
