//! Projects, worktrees and workspaces end to end, on throwaway repositories
//! made with the real Git CLI.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use twyx_core::check::Outcome;
use twyx_protocol::client::{
    CheckRun, ChecksInfo, FromDaemon, IndexState, Overview, Place, RemovalPlan, ReviewInfo,
    Reviewed, SetupReport, ShipReport, ToDaemon,
};

use support::{Client, Sandbox, git, new_pane_at, repo, sh};

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
            ..
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
        .send(&ToDaemon::RemoveWorktree {
            plan: first.plan,
            delete_branch: false,
        })
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
        .send(&ToDaemon::RemoveWorktree {
            plan: second.plan,
            delete_branch: false,
        })
        .unwrap();
    command.expect("the removal", |message| match message {
        FromDaemon::WorktreeRemoved { worktree: gone, .. } => {
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
        .send(&ToDaemon::RemoveWorktree {
            plan: clean.plan,
            delete_branch: false,
        })
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

fn new_worktree(client: &mut Client, project: u64, branch: &str) -> (u64, PathBuf, SetupReport) {
    client
        .send(&ToDaemon::NewWorktree {
            project,
            branch: branch.to_owned(),
            start: None,
        })
        .unwrap();
    client.expect("the new worktree", |message| match message {
        FromDaemon::WorktreeAdded {
            worktree, setup, ..
        } => Some((
            worktree.worktree,
            PathBuf::from(&worktree.path),
            setup.clone(),
        )),
        _ => None,
    })
}

fn commit(dir: &Path, file: &str) {
    std::fs::write(dir.join(file), file).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "--quiet", "-m", file]);
}

fn remove(client: &mut Client, plan: &RemovalPlan) -> Option<Result<String, String>> {
    client
        .send(&ToDaemon::RemoveWorktree {
            plan: plan.plan,
            delete_branch: true,
        })
        .unwrap();
    client.expect("the removal", |message| match message {
        FromDaemon::WorktreeRemoved { branch, .. } => Some(branch.clone()),
        _ => None,
    })
}

#[test]
fn invariant_24_a_branch_is_force_deleted_only_when_the_prompt_listed_its_unmerged_commits() {
    let mut sandbox = Sandbox::new("branch");
    sandbox.start();
    let main = repo(&sandbox, "app");
    let mut command = Client::command(&sandbox.socket());
    open(&mut command, &main);
    let project = overview(&mut command).projects[0].project;
    let has_branch = |name: &str| !git(&main, &["branch", "--list", name]).trim().is_empty();

    // Unmerged commits that the prompt listed: -D.
    let (listed, path, _) = new_worktree(&mut command, project, "listed");
    commit(&path, "work.txt");
    let plan = plan(&mut command, listed);
    assert_eq!(plan.branch.as_deref(), Some("listed"));
    assert_eq!(plan.unmerged.len(), 1);
    assert_eq!(remove(&mut command, &plan), Some(Ok("listed".to_owned())));
    assert!(!has_branch("listed"));

    // Merged: -d.
    let (merged, _, _) = new_worktree(&mut command, project, "merged");
    let plan_merged = self::plan(&mut command, merged);
    assert!(plan_merged.unmerged.is_empty());
    assert_eq!(
        remove(&mut command, &plan_merged),
        Some(Ok("merged".to_owned()))
    );
    assert!(!has_branch("merged"));

    // A commit after a prompt that listed none: the worktree goes, the
    // branch stays.
    let (late, path, _) = new_worktree(&mut command, project, "late");
    let plan_late = self::plan(&mut command, late);
    assert!(plan_late.unmerged.is_empty());
    commit(&path, "late.txt");
    let Some(Err(error)) = remove(&mut command, &plan_late) else {
        panic!("the branch was deleted");
    };
    assert!(error.contains("the branch late stays"), "{error}");
    assert!(!path.exists());
    assert!(has_branch("late"));
}

#[test]
fn a_new_worktree_gets_its_ignored_files_and_runs_setup_only_once_trusted() {
    let mut sandbox = Sandbox::new("setup");
    sandbox.start();
    let main = repo(&sandbox, "app");
    std::fs::write(main.join(".gitignore"), "node_modules/\n.env\n").unwrap();
    std::fs::write(main.join(".worktreeinclude"), "# secrets\n.env\n").unwrap();
    std::fs::create_dir_all(main.join(".twyx")).unwrap();
    std::fs::write(
        main.join(".twyx/worktree.toml"),
        "shared = [\"node_modules\"]\nsetup = [\"touch setup-ran\"]\n",
    )
    .unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "--quiet", "-m", "setup"]);
    std::fs::write(main.join(".env"), "KEY=1\n").unwrap();
    std::fs::create_dir_all(main.join("node_modules/dep")).unwrap();

    let mut command = Client::command(&sandbox.socket());
    open(&mut command, &main);
    let project = overview(&mut command).projects[0].project;
    let (worktree, path, report) = new_worktree(&mut command, project, "feature");
    assert_eq!(report.copied, vec![".env"]);
    assert!(report.untrusted);
    assert!(
        report.linked.is_empty() && report.pane.is_none(),
        "{report:?}"
    );
    assert_eq!(
        std::fs::read_to_string(path.join(".env")).unwrap(),
        "KEY=1\n"
    );
    assert!(!path.join("node_modules").exists());

    command.send(&ToDaemon::Setup { worktree }).unwrap();
    let info = command
        .expect("the setup file", |message| match message {
            FromDaemon::SetupFile(info) => Some(info.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(info.commands, vec!["touch setup-ran"]);
    command
        .send(&ToDaemon::TrustSetup {
            worktree,
            hash: info.hash,
        })
        .unwrap();
    command.expect("the trust", |message| match message {
        FromDaemon::SetupTrusted { .. } => Some(()),
        _ => None,
    });
    command.send(&ToDaemon::RunSetup { worktree }).unwrap();
    let report = command.expect("the setup", |message| match message {
        FromDaemon::SetupDone { report, .. } => Some(report.clone()),
        _ => None,
    });
    assert_eq!(report.linked, vec!["node_modules"]);
    assert!(report.pane.is_some(), "{report:?}");
    assert!(
        path.join("node_modules")
            .symlink_metadata()
            .unwrap()
            .is_symlink()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.join("setup-ran").exists() {
        assert!(Instant::now() < deadline, "the setup commands did not run");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_task_starts_each_agent_with_the_prompt_once_its_trusted_setup_ends() {
    let mut sandbox = Sandbox::new("task");
    let bin = sandbox.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let agent = bin.join("taskagent");
    // It records the prompt it started with and when setup had run.
    std::fs::write(
        &agent,
        "#!/bin/sh\n[ -e setup-ran ] && touch after-setup\nprintf '%s' \"$2\" > prompt-seen\nexec cat\n",
    )
    .unwrap();
    std::fs::set_permissions(&agent, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let agents = sandbox.root.join("config/twyx/agents");
    std::fs::create_dir_all(&agents).unwrap();
    let agent = agent.to_string_lossy();
    std::fs::write(
        agents.join("taskagent.toml"),
        format!(
            "title = \"Task agent\"\nprocesses = [\"taskagent\"]\ncommand = [\"{agent}\"]\n\
             launch_with_prompt = [\"{agent}\", \"--prompt\", \"{{prompt}}\"]\n"
        ),
    )
    .unwrap();
    sandbox.start();
    let main = repo(&sandbox, "app");
    std::fs::create_dir_all(main.join(".twyx")).unwrap();
    std::fs::write(
        main.join(".twyx/worktree.toml"),
        "setup = [\"sleep 1\", \"touch setup-ran\"]\n",
    )
    .unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "--quiet", "-m", "setup"]);

    let mut command = Client::command(&sandbox.socket());
    let workspace = open(&mut command, &main);
    let main_worktree = overview(&mut command).projects[0].worktrees[0].worktree;
    command
        .send(&ToDaemon::Setup {
            worktree: main_worktree,
        })
        .unwrap();
    let info = command
        .expect("the setup file", |message| match message {
            FromDaemon::SetupFile(info) => Some(info.clone()),
            _ => None,
        })
        .unwrap();
    command
        .send(&ToDaemon::TrustSetup {
            worktree: main_worktree,
            hash: info.hash,
        })
        .unwrap();
    command.expect("the trust", |message| match message {
        FromDaemon::SetupTrusted { .. } => Some(()),
        _ => None,
    });

    command
        .send(&ToDaemon::NewTask {
            workspace,
            name: "Fix the Login".to_owned(),
            start: None,
            agents: vec![Some("taskagent".to_owned()), None],
            prompt: Some("hi there".to_owned()),
        })
        .unwrap();
    let mut started = Vec::new();
    while started.len() < 2 {
        let (branch, note) = command.expect("a started task", |message| match message {
            FromDaemon::TaskStarted { branch, note, .. } => Some((branch.clone(), note.clone())),
            _ => None,
        });
        started.push((branch, note));
    }
    started.sort();
    assert_eq!(started[0], ("fix-the-login".to_owned(), None));
    assert_eq!(started[1].0, "fix-the-login-2");
    assert!(
        started[1]
            .1
            .as_deref()
            .is_some_and(|note| note.contains("clipboard")),
        "a shell takes no prompt: {:?}",
        started[1].1
    );

    let path = main.parent().unwrap().join("app.worktrees/fix-the-login");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.join("prompt-seen").exists() {
        assert!(Instant::now() < deadline, "the agent did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        std::fs::read_to_string(path.join("prompt-seen")).unwrap(),
        "hi there"
    );
    assert!(
        path.join("after-setup").exists(),
        "the agent started before setup ended"
    );
    assert_eq!(
        git(&path, &["branch", "--show-current"]).trim(),
        "fix-the-login"
    );
}

fn review(client: &mut Client, message: ToDaemon) -> ReviewInfo {
    client.send(&message).unwrap();
    client.expect("a review", |message| match message {
        FromDaemon::Review(review) => Some(review.clone()),
        _ => None,
    })
}

#[test]
fn invariant_12_a_review_lives_in_its_ref_until_its_worktree_is_removed() {
    let mut sandbox = Sandbox::new("review");
    sandbox.start();
    let main = repo(&sandbox, "app");
    let mut command = Client::command(&sandbox.socket());
    open(&mut command, &main);
    let project = overview(&mut command).projects[0].project;
    let (worktree, path, _) = new_worktree(&mut command, project, "feature");
    commit(&path, "one.txt");
    std::fs::write(path.join("two.txt"), "two\n").unwrap();

    let first = review(&mut command, ToDaemon::Review { worktree });
    assert_eq!(first.error, None);
    assert_eq!(first.base, git(&main, &["rev-parse", "main"]).trim());
    let states: Vec<(&str, Reviewed)> = first
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.reviewed))
        .collect();
    assert_eq!(
        states,
        vec![("one.txt", Reviewed::No), ("two.txt", Reviewed::No)]
    );

    let marked = review(
        &mut command,
        ToDaemon::MarkReviewed {
            worktree,
            path: "one.txt".to_owned(),
            hunks: None,
        },
    );
    let states: Vec<(&str, Reviewed)> = marked
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.reviewed))
        .collect();
    assert_eq!(
        states,
        vec![("two.txt", Reviewed::No), ("one.txt", Reviewed::Yes)]
    );
    assert_eq!(
        git(
            &main,
            &["for-each-ref", "--format=%(refname)", "refs/twyx/review/"]
        )
        .trim(),
        "refs/twyx/review/feature"
    );
    // The main index stays as Git left it; review writes only its own.
    assert_eq!(git(&path, &["status", "--porcelain"]).trim(), "?? two.txt");

    let plan = plan(&mut command, worktree);
    remove(&mut command, &plan);
    assert!(
        git(&main, &["for-each-ref", "refs/twyx/review/"]).is_empty(),
        "the removed worktree's review ref is left"
    );
}

#[test]
fn invariant_5_a_prompt_is_pasted_only_at_a_trusted_prompt_with_bracketed_paste_on() {
    let mut sandbox = Sandbox::new("inject");
    let bin = sandbox.root.join("bin");
    let agents = sandbox.root.join("config/twyx/agents");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&agents).unwrap();
    let twyx = env!("CARGO_BIN_EXE_twyx");
    // Both wait at their prompt by their integration; only one turns on
    // bracketed paste. Each keeps every byte it reads.
    for (kind, paste) in [
        ("pasteagent", "printf '\\033[?2004h'; "),
        ("plainagent", ""),
    ] {
        let script = bin.join(kind);
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nstty raw -echo\n{paste}sleep 1\n{twyx} notify awaiting-prompt --agent {kind}\nexec cat > received\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        std::fs::write(
            agents.join(format!("{kind}.toml")),
            format!(
                "title = \"{kind}\"\nprocesses = [\"{kind}\"]\ncommand = [\"{}\"]\n",
                script.display()
            ),
        )
        .unwrap();
    }
    sandbox.start();
    let main = repo(&sandbox, "app");
    let mut command = Client::command(&sandbox.socket());
    let workspace = open(&mut command, &main);
    command
        .send(&ToDaemon::NewTask {
            workspace,
            name: "paste".to_owned(),
            start: None,
            agents: vec![Some("pasteagent".to_owned()), Some("plainagent".to_owned())],
            prompt: Some("hi there\n".to_owned()),
        })
        .unwrap();
    let mut results = Vec::new();
    while results.len() < 2 {
        results.push(command.expect("a delivery", |message| match message {
            FromDaemon::Delivered { result, .. } => Some(result.clone()),
            _ => None,
        }));
    }
    assert!(results.contains(&Ok(())), "{results:?}");
    assert!(
        results.iter().any(|result| result
            .as_ref()
            .is_err_and(|reason| reason.contains("bracketed paste off"))),
        "{results:?}"
    );

    let received = main.parent().unwrap().join("app.worktrees/paste/received");
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read(&received).unwrap_or_default().is_empty() {
        assert!(Instant::now() < deadline, "the agent got nothing");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        std::fs::read(&received).unwrap(),
        b"\x1b[200~hi there\x1b[201~",
        "one bracketed paste, with no line break at its end"
    );
    assert!(
        std::fs::read(
            main.parent()
                .unwrap()
                .join("app.worktrees/paste-2/received")
        )
        .unwrap_or_default()
        .is_empty(),
        "a pane with bracketed paste off got the text"
    );
}

fn run_check(client: &mut Client, worktree: u64, name: &str) -> CheckRun {
    client
        .send(&ToDaemon::RunCheck {
            worktree,
            name: name.to_owned(),
        })
        .unwrap();
    client.expect("the check's end", |message| match message {
        FromDaemon::CheckEnded(run) if run.name == name => Some(run.clone()),
        _ => None,
    })
}

fn checks(client: &mut Client, worktree: u64) -> ChecksInfo {
    client.send(&ToDaemon::Checks { worktree }).unwrap();
    client.expect("the checks", |message| match message {
        FromDaemon::Checks(info) => Some(info.clone()),
        _ => None,
    })
}

#[test]
fn invariant_11_a_check_passes_or_fails_only_on_an_unchanged_source_key() {
    let mut sandbox = Sandbox::new("checks");
    std::fs::create_dir_all(sandbox.root.join("config/twyx")).unwrap();
    std::fs::write(
        sandbox.root.join("config/twyx/config.toml"),
        r#"
[[check]]
name = "pass"
command = "true"

[[check]]
name = "fail"
command = "echo broken; exit 3"

[[check]]
name = "edits"
command = "echo new > edited.txt"

[[check]]
name = "slow"
command = "sleep 30 & echo $! > ../slow.pid; wait"
timeout = 1

[[check]]
name = "wait"
command = "sleep 30"
"#,
    )
    .unwrap();
    sandbox.start();
    let main = repo(&sandbox, "app");
    std::fs::write(main.join(".gitignore"), "target/\n").unwrap();
    std::fs::create_dir_all(main.join(".twyx")).unwrap();
    std::fs::write(
        main.join(".twyx/checks.toml"),
        "[[check]]\nname = \"repo\"\ncommand = \"touch target/ran\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(main.join("target")).unwrap();

    let mut client = Client::command(&sandbox.socket());
    open(&mut client, &main);
    let worktree = overview(&mut client).projects[0].worktrees[0].worktree;

    let pass = run_check(&mut client, worktree, "pass");
    assert_eq!(pass.outcome, Some(Outcome::Pass));
    assert_eq!(pass.key_after.as_ref(), Some(&pass.key_before));
    let fail = run_check(&mut client, worktree, "fail");
    assert_eq!((fail.outcome, fail.exit), (Some(Outcome::Fail), Some(3)));
    assert!(fail.tail.contains("broken"), "{:?}", fail.tail);

    let edits = run_check(&mut client, worktree, "edits");
    assert_eq!(
        edits.outcome,
        Some(Outcome::Inconclusive),
        "exit 0 on a changed source"
    );
    assert_ne!(edits.key_after.as_ref(), Some(&edits.key_before));

    let started = Instant::now();
    let slow = run_check(&mut client, worktree, "slow");
    assert_eq!(slow.outcome, Some(Outcome::TimedOut));
    assert!(started.elapsed() < Duration::from_secs(10));
    let pid = std::fs::read_to_string(main.parent().unwrap().join("slow.pid")).unwrap();
    let alive = Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "the timeout killed the whole process group");

    client
        .send(&ToDaemon::RunCheck {
            worktree,
            name: "wait".to_owned(),
        })
        .unwrap();
    client.expect("the start", |message| match message {
        FromDaemon::CheckStarted { name, .. } if name == "wait" => Some(()),
        _ => None,
    });
    client
        .send(&ToDaemon::CancelCheck {
            worktree,
            name: "wait".to_owned(),
        })
        .unwrap();
    let cancelled = client.expect("the cancel", |message| match message {
        FromDaemon::CheckEnded(run) if run.name == "wait" => Some(run.clone()),
        _ => None,
    });
    assert_eq!(cancelled.outcome, Some(Outcome::Cancelled));

    // Invariant 13: the repository's check runs only once its file is trusted.
    client
        .send(&ToDaemon::RunCheck {
            worktree,
            name: "repo".to_owned(),
        })
        .unwrap();
    let refused = client.expect("a refusal", |message| match message {
        FromDaemon::Refused { reason } => Some(reason.clone()),
        _ => None,
    });
    assert!(refused.contains("not trusted"), "{refused}");
    let info = checks(&mut client, worktree);
    let file = info.file.clone().unwrap();
    assert!(!file.trusted);
    assert!(
        info.checks
            .iter()
            .any(|check| check.repository && check.check.name == "repo")
    );
    client
        .send(&ToDaemon::TrustChecks {
            worktree,
            hash: file.hash,
        })
        .unwrap();
    client.expect("the trusted checks", |message| match message {
        FromDaemon::Checks(info) if info.file.as_ref().is_some_and(|file| file.trusted) => Some(()),
        _ => None,
    });
    let repo_run = run_check(&mut client, worktree, "repo");
    assert_eq!(
        repo_run.outcome,
        Some(Outcome::Pass),
        "an ignored file is not source"
    );

    let info = checks(&mut client, worktree);
    let shown = |info: &ChecksInfo, name: &str| {
        info.checks
            .iter()
            .find(|check| check.check.name == name)
            .and_then(|check| check.run.clone())
            .unwrap()
    };
    assert!(!shown(&info, "repo").stale);
    assert!(
        shown(&info, "pass").stale,
        "edits changed the source after pass ran"
    );
    std::fs::write(main.join("later.txt"), "later\n").unwrap();
    assert!(shown(&checks(&mut client, worktree), "repo").stale);
}

fn shipped(client: &mut Client, done: bool) -> ShipReport {
    client.expect("a shipping step", |message| match message {
        FromDaemon::Shipped(report) if report.result.is_some() == done => Some(report.clone()),
        _ => None,
    })
}

#[test]
fn staging_follows_review_hunks_and_commit_and_push_run_in_panes() {
    let mut sandbox = Sandbox::new("ship");
    sandbox.start();
    let main = repo(&sandbox, "app");
    let lines: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
    std::fs::write(main.join("a.txt"), lines.join("\n") + "\n").unwrap();
    git(&main, &["add", "a.txt"]);
    git(&main, &["commit", "--quiet", "-m", "lines"]);
    let mut changed = lines.clone();
    changed[1] = "line two".to_owned();
    changed[14] = "line fifteen".to_owned();
    std::fs::write(main.join("a.txt"), changed.join("\n") + "\n").unwrap();
    std::fs::write(main.join("new.txt"), "new\n").unwrap();

    let mut client = Client::command(&sandbox.socket());
    open(&mut client, &main);
    let worktree = overview(&mut client).projects[0].worktrees[0].worktree;
    let file = |review: &ReviewInfo, path: &str| {
        review
            .files
            .iter()
            .find(|file| file.path == path)
            .cloned()
            .unwrap()
    };
    let first = review(&mut client, ToDaemon::Review { worktree });
    let hunks = file(&first, "a.txt").diff.unwrap().hunks;
    assert_eq!(hunks.len(), 2, "two changes far apart");
    assert_eq!(file(&first, "a.txt").index, IndexState::Unstaged);

    client
        .send(&ToDaemon::Stage {
            worktree,
            path: Some("a.txt".to_owned()),
            hunks: Some(vec![hunks[1].id()]),
            unstage: false,
        })
        .unwrap();
    let report = shipped(&mut client, true);
    assert_eq!(report.result, Some(Ok(())), "{report:?}");
    assert!(
        report.command.starts_with("git apply --cached"),
        "{}",
        report.command
    );
    let after = client.expect("a review", |message| match message {
        FromDaemon::Review(review) => Some(review.clone()),
        _ => None,
    });
    assert_eq!(file(&after, "a.txt").index, IndexState::Partly);
    let cached = git(&main, &["diff", "--cached"]);
    assert!(
        cached.contains("+line fifteen") && !cached.contains("+line two"),
        "{cached}"
    );

    client
        .send(&ToDaemon::Stage {
            worktree,
            path: Some("a.txt".to_owned()),
            hunks: Some(vec![hunks[1].id()]),
            unstage: true,
        })
        .unwrap();
    assert_eq!(shipped(&mut client, true).result, Some(Ok(())));
    assert_eq!(git(&main, &["diff", "--cached"]), "");

    // A new file stages whole; only fully reviewed files go with "stage reviewed".
    review(
        &mut client,
        ToDaemon::MarkReviewed {
            worktree,
            path: "new.txt".to_owned(),
            hunks: None,
        },
    );
    client.send(&ToDaemon::StageReviewed { worktree }).unwrap();
    assert_eq!(shipped(&mut client, true).result, Some(Ok(())));
    assert_eq!(
        git(&main, &["diff", "--cached", "--name-only"]),
        "new.txt\n"
    );

    std::fs::write(
        main.join(".git/hooks/pre-commit"),
        "#!/bin/sh\necho hook ran\n",
    )
    .unwrap();
    Command::new("chmod")
        .args(["+x", ".git/hooks/pre-commit"])
        .current_dir(&main)
        .status()
        .unwrap();
    client
        .send(&ToDaemon::Commit {
            worktree,
            message: "add new".to_owned(),
        })
        .unwrap();
    let running = shipped(&mut client, false);
    assert!(running.pane.is_some());
    let done = shipped(&mut client, true);
    assert_eq!(done.result, Some(Ok(())), "{done:?}");
    assert!(done.output.contains("hook ran"), "{:?}", done.output);
    assert_eq!(git(&main, &["log", "-1", "--format=%s"]), "add new\n");

    let remote = sandbox.root.join("remote.git");
    git(
        &main,
        &["init", "--quiet", "--bare", remote.to_str().unwrap()],
    );
    git(
        &main,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    client.send(&ToDaemon::Push { worktree }).unwrap();
    let running = shipped(&mut client, false);
    assert_eq!(running.command, "git push --set-upstream -- origin main");
    assert_eq!(shipped(&mut client, true).result, Some(Ok(())));
    assert_eq!(
        git(&main, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "origin/main\n"
    );
}
