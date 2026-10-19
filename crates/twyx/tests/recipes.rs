//! Recipes end to end: layouts, panes, trust and new worktrees.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use twyx_core::layout::{Axis, Node};
use twyx_protocol::client::{FromDaemon, RecipeInfo, RecipeSource, ToDaemon};

use support::{Client, Sandbox};

fn repo(sandbox: &Sandbox) -> PathBuf {
    let parent = sandbox.root.join("repos");
    std::fs::create_dir_all(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    git(
        &parent,
        &["init", "--quiet", "--initial-branch=main", "app"],
    );
    let path = parent.join("app");
    git(&path, &["config", "user.email", "t@example.com"]);
    git(&path, &["config", "user.name", "t"]);
    git(&path, &["config", "commit.gpgsign", "false"]);
    std::fs::create_dir_all(path.join("src")).unwrap();
    std::fs::write(path.join("src/lib.rs"), "\n").unwrap();
    git(&path, &["add", "."]);
    git(&path, &["commit", "--quiet", "-m", "first"]);
    path
}

fn git(dir: &Path, args: &[&str]) {
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

fn recipes(client: &mut Client, workspace: u64) -> Vec<RecipeInfo> {
    client.send(&ToDaemon::Recipes { workspace }).unwrap();
    client.expect("the recipes", |message| match message {
        FromDaemon::Recipes(recipes) => Some(recipes.clone()),
        _ => None,
    })
}

fn run(
    client: &mut Client,
    workspace: u64,
    name: &str,
    branch: Option<&str>,
) -> Result<u64, String> {
    client
        .send(&ToDaemon::RunRecipe {
            workspace,
            name: name.to_owned(),
            branch: branch.map(str::to_owned),
            switch: true,
        })
        .unwrap();
    client.expect("the recipe", |message| match message {
        FromDaemon::RecipeOpened { workspace, .. } => Some(Ok(*workspace)),
        FromDaemon::Refused { reason } => Some(Err(reason.clone())),
        _ => None,
    })
}

fn wait_for_file(path: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn invariant_13_a_repository_recipe_runs_only_while_its_exact_content_is_trusted() {
    let mut sandbox = Sandbox::new("rtrust");
    sandbox.start();
    let main = repo(&sandbox);
    let marker = sandbox.root.join("ran");
    let file = main.join(".twyx/recipes/mark.toml");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let text = format!(
        "[[tab]]\nlayout = \"mark\"\n\n[panes.mark]\ncommand = \"touch '{}'; exec cat\"\n",
        marker.display()
    );
    std::fs::write(&file, &text).unwrap();

    let mut client = Client::command(&sandbox.socket());
    let workspace = open(&mut client, &main);
    let listed = recipes(&mut client, workspace);
    let mark = listed.iter().find(|recipe| recipe.name == "mark").unwrap();
    let RecipeSource::Repository {
        path,
        hash,
        trusted,
    } = mark.source.clone()
    else {
        panic!("not a repository recipe: {mark:?}");
    };
    assert_eq!(path, ".twyx/recipes/mark.toml");
    assert!(!trusted);
    assert_eq!(
        mark.panes[0].command.as_deref().unwrap(),
        format!("touch '{}'; exec cat", marker.display())
    );

    let refused = run(&mut client, workspace, "mark", None).unwrap_err();
    assert!(refused.contains("not trusted"), "{refused}");

    // Trust is for the content that was shown.
    client
        .send(&ToDaemon::TrustRecipe {
            workspace,
            name: "mark".to_owned(),
            hash: "0".repeat(64),
        })
        .unwrap();
    let refused = client.expect("a refusal", |message| match message {
        FromDaemon::Refused { reason } => Some(reason.clone()),
        _ => None,
    });
    assert!(refused.contains("changed since it was shown"), "{refused}");
    assert!(!marker.exists());

    client
        .send(&ToDaemon::TrustRecipe {
            workspace,
            name: "mark".to_owned(),
            hash,
        })
        .unwrap();
    client.expect("the trust", |message| match message {
        FromDaemon::RecipeTrusted { .. } => Some(()),
        _ => None,
    });
    run(&mut client, workspace, "mark", None).unwrap();
    assert!(wait_for_file(&marker), "the trusted recipe did not run");

    // A changed file asks again.
    std::fs::remove_file(&marker).unwrap();
    std::fs::write(&file, format!("# edited\n{text}")).unwrap();
    let refused = run(&mut client, workspace, "mark", None).unwrap_err();
    assert!(refused.contains("not trusted"), "{refused}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!marker.exists());
}

#[test]
fn a_recipe_opens_its_tree_floating_layer_dirs_and_variables() {
    let mut sandbox = Sandbox::new("rlay");
    sandbox.start();
    let main = repo(&sandbox);
    std::fs::create_dir_all(sandbox.root.join("config/twyx/recipes")).unwrap();
    std::fs::write(
        sandbox.root.join("config/twyx/recipes/dev.toml"),
        r#"
description = "three panes and a float"

[[tab]]
name = "code"
layout = { row = ["left", { column = ["top", "bottom"] }], sizes = [3, 1] }
floating = [{ pane = "float", x = 10, y = 10, width = 50, height = 50 }]

[[tab]]
name = "logs"
layout = { stack = ["one", "two"], expanded = 1 }

[panes.left]
command = "[ \"$(pwd -P)\" = 'SRC' ] && echo in-src greeting=$GREETING; exec cat"
dir = "src"
env = { GREETING = "hello" }

[panes.top]
command = ["/bin/sh", "-c", "echo top; exec cat"]

[panes.bottom]
[panes.float]
[panes.one]
[panes.two]
"#
        .replace("SRC", &main.join("src").to_string_lossy()),
    )
    .unwrap();

    let mut client = sandbox.connect(100, 30);
    let workspace = open(&mut client, &main);
    let listed = recipes(&mut client, workspace);
    let names: Vec<&str> = listed.iter().map(|recipe| recipe.name.as_str()).collect();
    assert_eq!(names, vec!["dev", "editor-agent-tests"]);

    run(&mut client, workspace, "dev", None).unwrap();
    client.wait_until("both tabs", |c| {
        c.workspace
            .as_ref()
            .is_some_and(|w| w.workspace == workspace && w.tabs.len() == 2)
    });
    let info = client.workspace.clone().unwrap();
    assert_eq!(info.tabs[0].name, "code");
    let code = &info.tabs[0].layout;
    let Some(Node::Split {
        axis: Axis::Row,
        children,
        ratios,
    }) = &code.root
    else {
        panic!("not a row: {code:?}");
    };
    assert_eq!(ratios, &vec![0.75, 0.25]);
    assert!(
        matches!(&children[1], Node::Split { axis: Axis::Column, children, .. } if children.len() == 2)
    );
    assert_eq!(code.floating.len(), 1);
    assert_eq!(code.floating[0].geometry.width, 0.5);
    let Some(Node::Stack { children, expanded }) = &info.tabs[1].layout.root else {
        panic!("not a stack: {:?}", info.tabs[1].layout);
    };
    assert_eq!((children.len(), *expanded), (2, 1));

    let left = match &children_of(code)[0] {
        Node::Leaf(leaf) => leaf.pane.raw(),
        other => panic!("not a leaf: {other:?}"),
    };
    client.wait_for_text(left, "in-src greeting=hello");
}

fn children_of(layout: &twyx_core::layout::Layout) -> Vec<Node> {
    match &layout.root {
        Some(Node::Split { children, .. }) => children.clone(),
        _ => Vec::new(),
    }
}

#[test]
fn a_recipe_with_a_worktree_section_opens_in_a_new_worktree() {
    let mut sandbox = Sandbox::new("rwt");
    sandbox.start();
    let main = repo(&sandbox);
    std::fs::create_dir_all(sandbox.root.join("config/twyx/recipes")).unwrap();
    std::fs::write(
        sandbox.root.join("config/twyx/recipes/task.toml"),
        "[[tab]]\nlayout = \"shell\"\n\n[panes.shell]\n\n[worktree]\nfrom = \"main\"\n",
    )
    .unwrap();

    let mut client = Client::command(&sandbox.socket());
    let home = open(&mut client, &main);
    let refused = run(&mut client, home, "task", None).unwrap_err();
    assert!(refused.contains("give it a branch"), "{refused}");

    let opened = run(&mut client, home, "task", Some("task-1")).unwrap();
    assert_ne!(opened, home);
    assert!(main.parent().unwrap().join("app.worktrees/task-1").is_dir());

    // The built-in recipe opens anywhere: Neovim, an agent pane and a shell.
    let opened = run(&mut client, home, "editor-agent-tests", None).unwrap();
    assert_eq!(opened, home);
}
