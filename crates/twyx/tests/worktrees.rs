//! Projects, worktrees and workspaces end to end, on throwaway repositories
//! made with the real Git CLI.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use twyx_protocol::client::{FromDaemon, Overview, Place, RemovalPlan, ToDaemon};

use support::{Client, Sandbox, new_pane_at, sh};

/// A repository with one commit on `main`, under the sandbox.
fn repo(sandbox: &Sandbox, name: &str) -> PathBuf {
    let parent = sandbox.root.join("repos");
    std::fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    git(&parent, &["init", "--quiet", "--initial-branch=main", name]);
    let path = parent.join(name);
    git(&path, &["config", "user.email", "t@example.com"]);
    git(&path, &["config", "user.name", "t"]);
    git(&path, &["config", "commit.gpgsign", "false"]);
    std::fs::write(path.join("README"), "hello\n").unwrap();
    git(&path, &["add", "README"]);
    git(&path, &["commit", "--quiet", "-m", "first"]);
    path
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn open(client: &mut Client, path: &Path) -> u64 {
    client
        .send(&ToDaemon::Open {
            path: path.to_string_lossy().into_owned(),
            switch: true,
        })
        .unwrap();
    client.expect("the open", |message| match message {
        FromDaemon::Opened { workspace, .. } => Some(*workspace),
        _ => None,
    })
}

fn overview(client: &mut Client) -> Overview {
    client.send(&ToDaemon::Overview { refresh: true }).unwrap();
    client.expect("an overview", |message| match message {
        FromDaemon::Overview(overview) => Some(overview.clone()),
        _ => None,
    })
}

fn plan(command: &mut Client, worktree: u64) -> RemovalPlan {
    command.send(&ToDaemon::PlanRemoval { worktree }).unwrap();
    command.expect("a removal plan", |message| match message {
        FromDaemon::RemovalPlan(plan) => Some(plan.clone()),
        _ => None,
    })
}

fn shown_workspace(client: &Client) -> Option<u64> {
    client
        .workspace
        .as_ref()
        .map(|workspace| workspace.workspace)
}

#[test]
fn each_worktree_has_one_workspace_and_switching_stops_no_pane() {
    let mut sandbox = Sandbox::new("switch");
    sandbox.start();
    let main = repo(&sandbox, "app");
    let linked = main.parent().unwrap().join("app-feature");
    git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature",
            &linked.to_string_lossy(),
        ],
    );
    std::fs::create_dir_all(linked.join("src")).unwrap();

    let mut client = sandbox.connect(80, 24);
    let first = open(&mut client, &main);
    client.wait_until("the main workspace", |c| shown_workspace(c) == Some(first));
    let info = client.workspace.clone().unwrap();
    assert_eq!(info.name, "app/main");
    assert_eq!(info.directory, main.to_string_lossy());
    assert_eq!(info.worktree.as_ref().unwrap().name, "main");

    // A pane with no directory starts in its workspace's.
    let pane = new_pane_at(
        &mut client,
        "",
        sh(&format!(
            "[ \"$(pwd -P)\" = '{}' ] && echo in-workspace; exec cat",
            main.display()
        )),
        Place::NewTab,
    );
    client.wait_for_text(pane, "in-workspace");

    // Any path inside a worktree opens that worktree's one workspace.
    let second = open(&mut client, &linked.join("src"));
    assert_ne!(second, first);
    client.wait_until("the feature workspace", |c| {
        shown_workspace(c) == Some(second)
    });
    let info = client.workspace.clone().unwrap();
    assert_eq!(info.worktree.as_ref().unwrap().name, "app-feature");
    assert!(info.tabs.is_empty());
    assert_eq!(open(&mut client, &linked), second);

    client
        .send(&ToDaemon::SwitchWorkspace { workspace: first })
        .unwrap();
    client.wait_until("the main workspace again", |c| {
        shown_workspace(c) == Some(first) && c.workspace.as_ref().unwrap().tabs.len() == 1
    });
    client.type_text("still here\r");
    client.wait_for_text(pane, "still here");

    let overview = overview(&mut client);
    assert_eq!(overview.projects.len(), 1);
    let mut branches: Vec<_> = overview.projects[0]
        .worktrees
        .iter()
        .map(|worktree| (worktree.name.clone(), worktree.branch.clone()))
        .collect();
    branches.sort();
    assert_eq!(
        branches,
        vec![
            ("app-feature".to_owned(), Some("feature".to_owned())),
            ("main".to_owned(), Some("main".to_owned())),
        ]
    );
}

#[test]
fn the_shown_worktree_follows_its_branch_and_dirty_state() {
    let mut sandbox = Sandbox::new("status");
    sandbox.start();
    let main = repo(&sandbox, "app");
    let mut client = sandbox.connect(80, 24);
    let workspace = open(&mut client, &main);
    client.wait_until("a clean main", |c| {
        shown_workspace(c) == Some(workspace)
            && c.workspace
                .as_ref()
                .and_then(|w| w.worktree.as_ref()?.git.clone())
                .is_some_and(|git| git.branch.as_deref() == Some("main") && git.changed == 0)
    });

    std::fs::write(main.join("new.txt"), "x\n").unwrap();
    let listed = overview(&mut client);
    assert_eq!(
        listed.projects[0].worktrees[0]
            .git
            .as_ref()
            .unwrap()
            .changed,
        1
    );

    // A changed HEAD is read again within the poll, without a request.
    git(&main, &["checkout", "--quiet", "-b", "other"]);
    let started = Instant::now();
    client.wait_until("the new branch", |c| {
        c.workspace
            .as_ref()
            .and_then(|w| w.worktree.as_ref()?.git.clone())
            .is_some_and(|git| git.branch.as_deref() == Some("other"))
    });
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{}",
        sandbox.daemon_log()
    );
}

#[test]
fn invariant_20_removal_kills_the_listed_panes_before_git_runs_and_refuses_a_new_one() {
    let mut sandbox = Sandbox::new("remove");
    sandbox.start();
    let main = repo(&sandbox, "app");
    let mut client = sandbox.connect(80, 24);
    let home = open(&mut client, &main);
    client.wait_until("the main workspace", |c| shown_workspace(c) == Some(home));
    let keep = new_pane_at(&mut client, "", sh("exec cat"), Place::NewTab);

    let project = overview(&mut client).projects[0].project;
    client
        .send(&ToDaemon::NewWorktree {
            project,
            branch: "fix".to_owned(),
            start: None,
        })
        .unwrap();
    let (worktree, workspace) = client.expect("the new worktree", |message| match message {
        FromDaemon::WorktreeAdded {
            worktree,
            workspace,
        } => Some((worktree.clone(), *workspace)),
        _ => None,
    });
    let path = PathBuf::from(&worktree.path);
    assert_eq!(path, main.parent().unwrap().join("app.worktrees/fix"));
    assert_eq!(worktree.branch.as_deref(), Some("fix"));

    // A pane that notices if its directory goes while it still runs, and
    // runs on for a moment after the kill.
    let marker = sandbox.root.join("removed-under-a-live-pane");
    let watch = sh(&format!(
        "trap 'n=0' HUP; while :; do [ -d '{}' ] || echo gone >> '{}'; \
         [ -n \"$n\" ] && n=$((n+1)) && [ $n -gt 15 ] && exit 0; sleep 0.02; done",
        path.display(),
        marker.display()
    ));
    let spawned_here = new_pane_at(
        &mut client,
        &path.to_string_lossy(),
        watch.clone(),
        Place::NewTab,
    );
    client
        .send(&ToDaemon::SwitchWorkspace { workspace })
        .unwrap();
    client.wait_until("the fix workspace", |c| {
        shown_workspace(c) == Some(workspace)
    });
    let inside = new_pane_at(&mut client, "", watch.clone(), Place::NewTab);
    std::fs::write(path.join("wip.txt"), "wip\n").unwrap();

    let mut command = Client::command(&sandbox.socket());
    let first = plan(&mut command, worktree.worktree);
    let mut listed: Vec<u64> = first.panes.iter().map(|pane| pane.pane).collect();
    listed.sort();
    assert_eq!(listed, vec![spawned_here, inside]);
    assert_eq!(first.dirty, vec![("??".to_owned(), "wip.txt".to_owned())]);

    // A pane that starts after the prompt was not confirmed.
    let late = new_pane_at(&mut client, "", watch, Place::NewTab);
    command
        .send(&ToDaemon::RemoveWorktree { plan: first.plan })
        .unwrap();
    let refused = command.expect("a refusal", |message| match message {
        FromDaemon::Refused { reason } => Some(reason.clone()),
        _ => None,
    });
    assert!(refused.contains(&format!("pane {late}")), "{refused}");
    assert!(path.is_dir());
    assert!(client.panes.contains_key(&inside));

    let second = plan(&mut command, worktree.worktree);
    assert_eq!(second.panes.len(), 3);
    command
        .send(&ToDaemon::RemoveWorktree { plan: second.plan })
        .unwrap();
    command.expect("the removal", |message| match message {
        FromDaemon::WorktreeRemoved { worktree: gone } => {
            (*gone == worktree.worktree).then_some(())
        }
        _ => None,
    });
    assert!(!path.exists());
    // A pane still alive would see the directory gone within its linger.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !marker.exists(),
        "Git removed the directory under a live pane"
    );

    // The client on the removed workspace moves to the default one, and the
    // pane outside the worktree runs on.
    client.wait_until("the default workspace", |c| {
        shown_workspace(c).is_some_and(|shown| shown != workspace && shown != home)
    });
    client.wait_until("the killed panes to go", |c| {
        [spawned_here, inside, late]
            .iter()
            .all(|pane| !c.panes.contains_key(pane))
    });
    assert!(client.panes.contains_key(&keep));
    let after = overview(&mut command);
    assert_eq!(after.projects[0].worktrees.len(), 1);
    assert!(!after.workspaces.iter().any(|w| w.workspace == workspace));
    // The branch stays: removal deletes it only when asked.
    assert!(git(&main, &["branch", "--list", "fix"]).contains("fix"));
}

#[test]
fn a_change_after_a_clean_prompt_is_never_discarded() {
    let mut sandbox = Sandbox::new("no-force");
    sandbox.start();
    let main = repo(&sandbox, "app");
    let linked = main.parent().unwrap().join("app-clean");
    git(
        &main,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "clean",
            &linked.to_string_lossy(),
        ],
    );
    let mut command = Client::command(&sandbox.socket());
    open(&mut command, &main);
    let worktree = overview(&mut command).projects[0]
        .worktrees
        .iter()
        .find(|worktree| worktree.name == "app-clean")
        .unwrap()
        .worktree;

    let clean = plan(&mut command, worktree);
    assert!(clean.dirty.is_empty());
    std::fs::write(linked.join("late.txt"), "late\n").unwrap();
    command
        .send(&ToDaemon::RemoveWorktree { plan: clean.plan })
        .unwrap();
    let refused = command.expect("a refusal", |message| match message {
        FromDaemon::Refused { reason } => Some(reason.clone()),
        _ => None,
    });
    assert!(refused.starts_with("git worktree remove"), "{refused}");
    assert!(linked.join("late.txt").exists());

    // The main worktree is never offered for removal.
    let main_id = overview(&mut command).projects[0]
        .worktrees
        .iter()
        .find(|worktree| worktree.name == "main")
        .unwrap()
        .worktree;
    command
        .send(&ToDaemon::PlanRemoval { worktree: main_id })
        .unwrap();
    let refused = command.expect("a refusal", |message| match message {
        FromDaemon::Refused { reason } => Some(reason.clone()),
        _ => None,
    });
    assert!(refused.contains("main worktree"), "{refused}");
}

#[test]
fn the_cli_opens_lists_creates_and_removes_from_inside_a_repository() {
    let mut sandbox = Sandbox::new("cli");
    sandbox.start();
    let main = repo(&sandbox, "app");
    let run = |args: &[&str]| {
        let output = sandbox
            .command(args)
            .current_dir(&main)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "twyx {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };

    let opened: serde_json::Value = serde_json::from_str(&run(&["open", ".", "--json"])).unwrap();
    assert_eq!(opened["workspace"]["name"], "app/main");
    assert!(opened["client"].is_null());

    run(&["worktree", "new", "topic"]);
    let listed: serde_json::Value =
        serde_json::from_str(&run(&["worktree", "list", "--json"])).unwrap();
    let names: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|worktree| worktree["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["main", "topic"]);
    assert!(run(&["ls"]).contains("topic"));

    let removed = run(&["worktree", "rm", "topic", "--yes"]);
    assert!(removed.contains("removed topic"), "{removed}");
    assert!(!main.parent().unwrap().join("app.worktrees/topic").exists());
}
