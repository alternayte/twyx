//! A workspace's files: the list, finding text, and Open in Neovim, on
//! throwaway repositories and directories.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use twyx_core::layout::{Content, LeafId};
use twyx_protocol::client::{DocumentText, FileList, FoundText, FromDaemon, Launch, ToDaemon};

use support::{Client, Sandbox, git, repo};

fn open(client: &mut Client, path: &Path) -> u64 {
    client
        .send(&ToDaemon::Open {
            path: path.to_string_lossy().into_owned(),
            switch: true,
        })
        .unwrap();
    let workspace = client.expect("the open", |message| match message {
        FromDaemon::Opened { workspace, .. } => Some(*workspace),
        _ => None,
    });
    client.wait_until("the workspace", |c| {
        c.workspace.as_ref().map(|shown| shown.workspace) == Some(workspace)
    });
    workspace
}

fn files(client: &mut Client, workspace: u64) -> FileList {
    client.send(&ToDaemon::Files { workspace }).unwrap();
    client.expect("the files", |message| match message {
        FromDaemon::Files(list) => Some(list.clone()),
        _ => None,
    })
}

fn find(client: &mut Client, workspace: u64, query: &str) -> Vec<(String, u32)> {
    client
        .send(&ToDaemon::FindInFiles {
            workspace,
            query: query.to_owned(),
        })
        .unwrap();
    let found: FoundText = client.expect("the found lines", |message| match message {
        FromDaemon::Found(found) if found.query == query => Some(found.clone()),
        _ => None,
    });
    assert_eq!(found.error, None);
    let mut hits: Vec<(String, u32)> = found
        .hits
        .into_iter()
        .map(|hit| (hit.path, hit.line))
        .collect();
    hits.sort();
    hits
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn a_worktree_lists_and_searches_its_tracked_and_untracked_files_but_not_ignored_ones() {
    let mut sandbox = Sandbox::new("fg");
    sandbox.start();
    let main = repo(&sandbox, "app");
    write(&main.join("gone.txt"), "hello\n");
    git(&main, &["add", "gone.txt"]);
    git(&main, &["commit", "--quiet", "-m", "gone"]);
    std::fs::remove_file(main.join("gone.txt")).unwrap();
    write(&main.join(".gitignore"), "target/\n*.log\n");
    write(
        &main.join("src/new.rs"),
        "fn main() {\n    say(\"Hello\");\n}\n",
    );
    write(&main.join("target/out.txt"), "hello\n");
    write(&main.join("run.log"), "hello\n");

    let mut client = sandbox.connect(80, 24);
    let workspace = open(&mut client, &main);
    let list = files(&mut client, workspace);
    assert_eq!(list.error, None);
    assert_eq!(list.root, main.to_string_lossy());
    assert_eq!(list.paths, [".gitignore", "README", "src/new.rs"]);

    assert_eq!(
        find(&mut client, workspace, "hello"),
        [("README".to_owned(), 1), ("src/new.rs".to_owned(), 2)],
        "a lowercase query ignores case and skips ignored files"
    );
    assert_eq!(
        find(&mut client, workspace, "Hello"),
        [("src/new.rs".to_owned(), 2)],
        "a capital makes the case count"
    );
}

#[test]
fn a_plain_directory_keeps_to_its_ignore_files() {
    let mut sandbox = Sandbox::new("fp");
    sandbox.start();
    let dir = sandbox.root.join("notes");
    write(&dir.join(".gitignore"), "skip/\n");
    write(&dir.join("a.md"), "one\ntwo needle\n");
    write(&dir.join("deep/b.md"), "needle\n");
    write(&dir.join("skip/c.md"), "needle\n");
    let dir = dir.canonicalize().unwrap();

    let mut client = sandbox.connect(80, 24);
    let workspace = open(&mut client, &dir);
    assert_eq!(files(&mut client, workspace).paths, ["a.md", "deep/b.md"]);
    assert_eq!(
        find(&mut client, workspace, "needle"),
        [("a.md".to_owned(), 2), ("deep/b.md".to_owned(), 1)]
    );
}

#[test]
fn the_default_workspace_lists_no_files_and_finds_nothing() {
    let mut sandbox = Sandbox::new("fd");
    sandbox.start();
    let mut client = sandbox.connect(80, 24);
    client.wait_until("the default workspace", |c| c.workspace.is_some());
    let workspace = client.workspace.as_ref().unwrap().workspace;
    let list = files(&mut client, workspace);
    assert_eq!(list.paths, Vec::<String>::new(), "no walk of $HOME");
    assert_eq!(
        list.error.as_deref(),
        Some("open a project to list its files")
    );
    client
        .send(&ToDaemon::FindInFiles {
            workspace,
            query: "needle".to_owned(),
        })
        .unwrap();
    let found: FoundText = client.expect("the found lines", |message| match message {
        FromDaemon::Found(found) => Some(found.clone()),
        _ => None,
    });
    assert_eq!(found.hits, []);
    assert_eq!(
        found.error.as_deref(),
        Some("open a project to list its files")
    );
}

/// What the Neovim at `socket` edits: the file name and the cursor line.
fn editing(socket: &Path) -> String {
    let output = Command::new("nvim")
        .args([
            "--server",
            &socket.to_string_lossy(),
            "--remote-expr",
            "expand('%:t') .. ':' .. line('.')",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn open_in_neovim_starts_neovim_then_uses_the_running_one() {
    let mut sandbox = Sandbox::new("nv");
    sandbox.start();
    let main = repo(&sandbox, "app");
    write(&main.join("a.txt"), "one\n");
    write(&main.join("b.txt"), "one\ntwo\nthree\n");

    let mut client = sandbox.connect(100, 30);
    open(&mut client, &main);
    client
        .send(&ToDaemon::OpenInEditor {
            path: "a.txt".to_owned(),
            line: None,
        })
        .unwrap();
    client.wait_until("a Neovim pane", |c| {
        c.panes.values().any(|pane| {
            matches!(&pane.launch, Launch::Command(argv) if argv.first().map(String::as_str) == Some("nvim"))
        })
    });
    // Neovim's default server lives in the runtime directory, named by pid.
    let deadline = Instant::now() + Duration::from_secs(10);
    let socket = loop {
        let found = std::fs::read_dir(sandbox.root.join("run"))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("nvim."))
            });
        if let Some(socket) = found {
            break socket;
        }
        assert!(Instant::now() < deadline, "Neovim never listened");
        client.pump();
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while editing(&socket) != "a.txt:1" {
        assert!(Instant::now() < deadline, "Neovim does not edit a.txt");
        std::thread::sleep(Duration::from_millis(50));
    }

    // The daemon sees Neovim in the foreground once its holder polls.
    let panes = client.panes.len();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        client
            .send(&ToDaemon::OpenInEditor {
                path: "b.txt".to_owned(),
                line: Some(3),
            })
            .unwrap();
        client.send(&ToDaemon::Sync { token: 1 }).unwrap();
        client.expect("the open handled", |message| match message {
            FromDaemon::Synced { token: 1 } => Some(()),
            _ => None,
        });
        std::thread::sleep(Duration::from_millis(200));
        if editing(&socket) == "b.txt:3" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the running Neovim never opened b.txt: {} panes",
            client.panes.len()
        );
    }
    client.send(&ToDaemon::Sync { token: 2 }).unwrap();
    client.expect("the last open handled", |message| match message {
        FromDaemon::Synced { token: 2 } => Some(()),
        _ => None,
    });
    assert_eq!(client.panes.len(), panes, "no second Neovim");
}

/// The document leaves of the shown workspace: leaf and path.
fn documents(client: &Client) -> Vec<(LeafId, String)> {
    client
        .workspace
        .iter()
        .flat_map(|shown| &shown.tabs)
        .flat_map(|tab| tab.layout.leaves())
        .filter_map(|leaf| match leaf.content {
            Content::Document(path) => Some((leaf.id, path)),
            _ => None,
        })
        .collect()
}

fn read(client: &mut Client, leaf: LeafId, since: Option<i64>) -> DocumentText {
    client.send(&ToDaemon::Document { leaf, since }).unwrap();
    client.expect("the document", |message| match message {
        FromDaemon::Document(document) => Some(document.clone()),
        _ => None,
    })
}

/// The daemon answers an unchanged file with nothing: it reads no file,
/// so the `Synced` after the request comes first and alone.
fn unchanged(client: &mut Client, leaf: LeafId, since: Option<i64>) {
    client.send(&ToDaemon::Document { leaf, since }).unwrap();
    client.send(&ToDaemon::Sync { token: 7 }).unwrap();
    client.expect("the request handled", |message| match message {
        FromDaemon::Document(document) => panic!("sent again: {document:?}"),
        FromDaemon::Synced { token: 7 } => Some(()),
        _ => None,
    });
}

#[test]
fn a_document_opens_once_and_is_sent_again_only_when_it_changes() {
    let mut sandbox = Sandbox::new("dc");
    sandbox.start();
    let main = repo(&sandbox, "app");
    write(&main.join("docs/guide.md"), "# Guide\n");

    let mut client = sandbox.connect(80, 24);
    open(&mut client, &main);
    let tabs = client.workspace.as_ref().unwrap().tabs.len();
    for _ in 0..2 {
        client
            .send(&ToDaemon::OpenDocument {
                path: "docs/guide.md".to_owned(),
                switch: false,
            })
            .unwrap();
        client.wait_until("the document leaf", |c| documents(c).len() == 1);
    }
    assert_eq!(
        client.workspace.as_ref().unwrap().tabs.len(),
        tabs + 1,
        "opening the same file again shows its leaf"
    );
    let (leaf, path) = documents(&client).remove(0);
    let guide = main.join("docs/guide.md");
    assert_eq!(path, guide.to_string_lossy());

    let first = read(&mut client, leaf, None);
    assert_eq!(first.text, "# Guide\n");
    assert_eq!(first.root, main.to_string_lossy());
    assert_eq!(first.error, None);
    unchanged(&mut client, leaf, first.modified);

    write(&guide, "# Guide\n\nMore.\n");
    let later = std::time::SystemTime::now() + Duration::from_secs(2);
    std::fs::File::options()
        .write(true)
        .open(&guide)
        .unwrap()
        .set_modified(later)
        .unwrap();
    let second = read(&mut client, leaf, first.modified);
    assert_eq!(second.text, "# Guide\n\nMore.\n");

    std::fs::remove_file(&guide).unwrap();
    let gone = read(&mut client, leaf, second.modified);
    assert!(gone.error.is_some(), "{gone:?}");
}

#[test]
fn twyx_open_on_a_markdown_file_records_a_document_that_its_workspace_lands_on() {
    let mut sandbox = Sandbox::new("do");
    sandbox.start();
    let main = repo(&sandbox, "app");
    write(&main.join("docs/guide.md"), "# Guide\n");
    let output = sandbox
        .command(&["open", "docs/guide.md", "--no-switch", "--json"])
        .current_dir(&main)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut client = sandbox.connect(80, 24);
    open(&mut client, &main);
    client.wait_until("the document tab", |c| {
        let Some(shown) = &c.workspace else {
            return false;
        };
        shown
            .tabs
            .iter()
            .find(|tab| Some(tab.tab) == c.tab)
            .is_some_and(|tab| {
                tab.layout.leaves().iter().any(|leaf| {
                    leaf.content
                        == Content::Document(
                            main.join("docs/guide.md").to_string_lossy().into_owned(),
                        )
                })
            })
    });
}
