//! A workspace's files for the clients: the list, the lines that hold a
//! query, and "Open in Neovim".
//!
//! A worktree's files come from Git. A plain directory's come from a walk
//! that keeps to its ignore files.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use twyx_core::layout::Axis;
use twyx_core::tuning;
use twyx_protocol::client::{FileList, FoundText, Hit, Launch, Place};

use crate::daemon::Daemon;
use crate::git;
use crate::worktrees::Done;

impl Daemon {
    /// The workspace's directory, and whether it is a Git worktree.
    fn file_root(&self, workspace: u64) -> Option<(PathBuf, bool)> {
        let entry = self.workspaces.get(&workspace)?;
        let worktree = entry
            .worktree
            .and_then(|worktree| self.worktrees.get(&worktree));
        Some(match worktree {
            Some(worktree) => (PathBuf::from(&worktree.path), true),
            None => (PathBuf::from(&entry.directory), false),
        })
    }

    pub(crate) fn files(&mut self, client: u64, workspace: u64) {
        let Some((root, git)) = self.file_root(workspace) else {
            self.refuse(client, format!("no workspace {workspace}"));
            return;
        };
        self.job(move || {
            let listed = if git {
                git::files::list(&root, tuning::FILES_MAX)
            } else {
                Ok(walk(&root, tuning::FILES_MAX))
            };
            let (paths, truncated, error) = match listed {
                Ok((paths, truncated)) => (paths, truncated, None),
                Err(error) => (Vec::new(), false, Some(format!("{error:#}"))),
            };
            Done::Files {
                client,
                list: FileList {
                    workspace,
                    root: root.to_string_lossy().into_owned(),
                    paths,
                    truncated,
                    error,
                },
            }
        });
    }

    pub(crate) fn find_in_files(&mut self, client: u64, workspace: u64, query: String) {
        let Some((root, git)) = self.file_root(workspace) else {
            self.refuse(client, format!("no workspace {workspace}"));
            return;
        };
        self.job(move || {
            let found = if query.is_empty() {
                Ok((Vec::new(), false))
            } else if git {
                git::files::grep(&root, &query, tuning::FOUND_MAX, tuning::FOUND_LINE_CHARS)
            } else {
                Ok(find(&root, &query))
            };
            let (hits, truncated, error) = match found {
                Ok((hits, truncated)) => (hits, truncated, None),
                Err(error) => (Vec::new(), false, Some(format!("{error:#}"))),
            };
            Done::Found {
                client,
                found: FoundText {
                    workspace,
                    query,
                    root: root.to_string_lossy().into_owned(),
                    hits,
                    truncated,
                    error,
                },
            }
        });
    }

    /// Opens `path` at `line` in the Neovim of a pane in the client's
    /// workspace and shows that pane; with no Neovim there, starts one in
    /// a new pane beside the focused one.
    pub(crate) fn open_in_editor(&mut self, client: u64, path: String, line: Option<u32>) {
        let workspace = self.client_workspace(client);
        let path = match self.file_root(workspace) {
            Some((root, _)) => root.join(&path),
            None => PathBuf::from(&path),
        };
        if !path.is_file() {
            self.refuse(client, format!("{} is not a file", path.display()));
            return;
        }
        let editors: Vec<(twyx_core::pane::PaneId, String)> = self
            .panes
            .iter()
            .filter(|(_, entry)| entry.workspace == workspace)
            .filter_map(|(pane, entry)| {
                let (pid, name) = entry.agent.foreground()?;
                if name != "nvim" {
                    return None;
                }
                let argv = match &entry.launch {
                    Launch::Command(argv) => argv.as_slice(),
                    Launch::Shell | Launch::Unknown => &[],
                };
                Some((*pane, server(pid, argv)?))
            })
            .collect();
        // The focused pane first, then the rest in pane order.
        let focused = self
            .client_focus(client)
            .and_then(|(tab, leaf)| self.tab(tab)?.layout.leaf(leaf)?.terminal());
        let chosen = editors
            .iter()
            .find(|(pane, _)| Some(*pane) == focused)
            .or_else(|| editors.iter().min_by_key(|(pane, _)| pane.raw()));
        match chosen {
            Some((pane, address)) => {
                let (pane, address) = (*pane, address.clone());
                match remote_edit(&address, &path, line) {
                    Ok(()) => self.show_pane(client, pane),
                    Err(error) => self.refuse(client, error),
                }
            }
            None => {
                let mut argv = vec!["nvim".to_owned()];
                if let Some(line) = line {
                    argv.push(format!("+{line}"));
                }
                argv.push(path.to_string_lossy().into_owned());
                let cwd = path
                    .parent()
                    .map(|dir| dir.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.new_pane(
                    client,
                    Launch::Command(argv),
                    cwd,
                    Place::Split(Axis::Row),
                    true,
                );
            }
        }
    }
}

/// The server address of the Neovim with this pid: the one `--listen`
/// gave it, else Neovim's default, whose name holds the pid of the server
/// process. Since Neovim 0.10 the terminal UI in the foreground runs the
/// server as its child.
fn server(pid: u32, argv: &[String]) -> Option<String> {
    if let Some(at) = argv.iter().position(|arg| arg == "--listen") {
        return argv.get(at + 1).cloned();
    }
    let mut names = vec![format!("nvim.{pid}.0")];
    if let Ok(output) = std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .output()
    {
        names.extend(
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .map(|child| format!("nvim.{child}.0")),
        );
    }
    let mut places: Vec<PathBuf> = Vec::new();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        places.push(PathBuf::from(runtime));
    }
    // `stdpath("run")` without XDG_RUNTIME_DIR: a random directory under
    // `$TMPDIR/nvim.$USER`.
    let user = std::env::var("USER").unwrap_or_default();
    for temp in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let base = temp.join(format!("nvim.{user}"));
        places.push(base.clone());
        if let Ok(entries) = std::fs::read_dir(&base) {
            places.extend(entries.flatten().map(|entry| entry.path()));
        }
    }
    places
        .iter()
        .flat_map(|place| names.iter().map(|name| place.join(name)))
        .find(|socket| socket.exists())
        .map(|socket| socket.to_string_lossy().into_owned())
}

/// Asks the Neovim at `address` to edit `path` at `line`.
fn remote_edit(address: &str, path: &Path, line: Option<u32>) -> Result<(), String> {
    // A Vim string in single quotes doubles its quotes.
    let quoted = path.to_string_lossy().replace('\'', "''");
    let jump = line.map(|line| format!("+{line} ")).unwrap_or_default();
    let expr = format!("execute('edit {jump}' .. fnameescape('{quoted}'))");
    let output = std::process::Command::new("nvim")
        .args(["--server", address, "--remote-expr", &expr])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("run nvim --server {address}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "nvim --server {address} --remote-expr failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The files under `root` that its ignore files do not hide, sorted, at
/// most `limit`; whether there were more.
fn walk(root: &Path, limit: usize) -> (Vec<String>, bool) {
    let mut paths = Vec::new();
    let mut truncated = false;
    for entry in ignore::WalkBuilder::new(root)
        .require_git(false)
        .build()
        .flatten()
    {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        if paths.len() == limit {
            truncated = true;
            break;
        }
        paths.push(
            relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
        );
    }
    paths.sort();
    (paths, truncated)
}

/// The lines of the text files under `root` that hold `query`, as
/// `git grep` finds them in a worktree.
fn find(root: &Path, query: &str) -> (Vec<Hit>, bool) {
    let fold = !query.chars().any(char::is_uppercase);
    let needle = if fold {
        query.to_lowercase()
    } else {
        query.to_owned()
    };
    let (paths, _) = walk(root, tuning::FILES_MAX);
    let mut hits = Vec::new();
    for path in paths {
        let Ok(file) = std::fs::File::open(root.join(&path)) else {
            continue;
        };
        let mut text = Vec::new();
        if file
            .take(tuning::FOUND_FILE_BYTES)
            .read_to_end(&mut text)
            .is_err()
            || text.contains(&0)
        {
            continue;
        }
        for (number, line) in BufReader::new(text.as_slice()).lines().enumerate() {
            let Ok(line) = line else {
                break;
            };
            let holds = if fold {
                line.to_lowercase().contains(&needle)
            } else {
                line.contains(&needle)
            };
            if !holds {
                continue;
            }
            if hits.len() == tuning::FOUND_MAX {
                return (hits, true);
            }
            hits.push(Hit {
                path: path.clone(),
                line: u32::try_from(number + 1).unwrap_or(u32::MAX),
                text: line.chars().take(tuning::FOUND_LINE_CHARS).collect(),
            });
        }
    }
    (hits, false)
}
