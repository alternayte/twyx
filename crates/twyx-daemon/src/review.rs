//! Review requests: each runs as a background job, one at a time per
//! worktree, since they share its review index.

use std::sync::{Arc, Mutex};

use twyx_core::layout::Content;
use twyx_protocol::client::{IndexState, ReviewFile, ReviewInfo, Reviewed, ShipReport};

use crate::daemon::Daemon;
use crate::git;
use crate::worktrees::Done;

impl Daemon {
    /// Shows the review leaf of a worktree to the client: the one its
    /// workspace has, or a new one in a tab of its own.
    pub(crate) fn open_review(&mut self, client: u64, worktree: Option<u64>) {
        let worktree = worktree.or_else(|| {
            let workspace = self.client_workspace(client);
            self.workspaces.get(&workspace)?.worktree
        });
        let Some(worktree) = worktree.filter(|worktree| self.worktrees.contains_key(worktree))
        else {
            self.refuse(client, "not in a Git worktree".to_owned());
            return;
        };
        let workspace = match self.worktree_workspace(worktree) {
            Ok(workspace) => workspace,
            Err(error) => {
                self.refuse(client, format!("{error:#}"));
                return;
            }
        };
        let existing = self.workspace_tabs(workspace).into_iter().find_map(|tab| {
            self.tab(tab)?
                .layout
                .leaves()
                .into_iter()
                .find(|leaf| leaf.content == Content::Review(worktree))
                .map(|leaf| leaf.id)
        });
        let leaf = match existing {
            Some(leaf) => leaf,
            None => {
                let Some(tab) = self.add_tab(workspace, "review".to_owned()) else {
                    self.refuse(client, "could not record a new tab".to_owned());
                    return;
                };
                let leaf = self.new_review_leaf(worktree);
                if let Some(entry) = self.tab_mut(tab) {
                    entry.layout.split(None, leaf, twyx_core::layout::Axis::Row);
                    entry.focus = Some(leaf.id);
                }
                leaf.id
            }
        };
        self.show_leaf(client, workspace, leaf);
    }

    /// Sends a worktree's review, after `change`.
    pub(crate) fn review(&mut self, client: u64, worktree: u64, change: Option<Change>) {
        let Some(entry) = self.worktrees.get(&worktree) else {
            self.refuse(client, format!("no worktree {worktree}"));
            return;
        };
        let path = std::path::PathBuf::from(&entry.path);
        let name = entry.admin.clone();
        let base_branch = self
            .projects
            .get(&entry.project)
            .and_then(|project| project.base_branch.clone());
        let lock = Arc::clone(
            self.review_locks
                .entry(worktree)
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        );
        self.job(move || {
            let _one_at_a_time = lock.lock();
            let (failed, shipped) = match change {
                Some(change) => apply(&path, &name, base_branch.as_deref(), worktree, change),
                None => (None, None),
            };
            let mut review = match git::review::state(&path, &name, base_branch.as_deref()) {
                Ok(state) => info(worktree, state, git::ship::index_paths(&path).ok()),
                Err(error) => ReviewInfo {
                    worktree,
                    base_ref: String::new(),
                    base: String::new(),
                    files: Vec::new(),
                    error: Some(format!("{error:#}")),
                },
            };
            if let Some(error) = failed {
                review.error = Some(format!("{error:#}"));
            }
            Done::Reviewed {
                client,
                review,
                shipped,
            }
        });
    }
}

/// A change to make before a review is read.
#[derive(Debug)]
pub(crate) enum Change {
    /// Mark a file's hunks reviewed, or all of it.
    Mark(String, Option<Vec<u64>>),
    /// Stage or unstage a file's hunks, a file, or everything.
    Stage {
        path: Option<String>,
        hunks: Option<Vec<u64>>,
        unstage: bool,
    },
    /// Stage the files whose changes are all reviewed.
    StageReviewed,
}

/// Makes `change`: a marking that failed, or what a staging step ran.
fn apply(
    path: &std::path::Path,
    name: &str,
    base_branch: Option<&str>,
    worktree: u64,
    change: Change,
) -> (Option<anyhow::Error>, Option<ShipReport>) {
    let staged = match change {
        Change::Mark(file, hunks) => {
            return (
                git::review::mark(path, name, base_branch, &file, hunks.as_deref()).err(),
                None,
            );
        }
        Change::Stage {
            path: Some(file),
            hunks: Some(hunks),
            unstage,
        } => git::review::state(path, name, base_branch).and_then(|state| {
            let chosen: Vec<_> = state
                .unreviewed
                .iter()
                .filter(|diff| diff.path == file)
                .flat_map(|diff| &diff.hunks)
                .filter(|hunk| hunks.contains(&hunk.id()))
                .cloned()
                .collect();
            if chosen.is_empty() {
                anyhow::bail!("those hunks of {file} changed; look again");
            }
            git::ship::stage_hunks(path, &file, &chosen, unstage)
        }),
        Change::Stage {
            path: file,
            unstage,
            ..
        } => git::ship::stage_paths(path, &file.into_iter().collect::<Vec<_>>(), unstage),
        Change::StageReviewed => git::review::state(path, name, base_branch).and_then(|state| {
            let done: Vec<String> = state
                .reviewed
                .into_iter()
                .filter(|file| !state.unreviewed.iter().any(|diff| &diff.path == file))
                .collect();
            if done.is_empty() {
                anyhow::bail!("no file has all its changes reviewed");
            }
            git::ship::stage_paths(path, &done, false)
        }),
    };
    let report = match staged {
        Ok(shown) => ShipReport {
            worktree,
            command: shown.command,
            output: shown.output,
            result: Some(Ok(())),
            pane: None,
        },
        Err(error) => ShipReport {
            worktree,
            command: String::new(),
            output: String::new(),
            result: Some(Err(format!("{error:#}"))),
            pane: None,
        },
    };
    (None, Some(report))
}

fn index_state(
    path: &str,
    paths: Option<&(
        std::collections::BTreeSet<String>,
        std::collections::BTreeSet<String>,
    )>,
) -> IndexState {
    let Some((staged, unstaged)) = paths else {
        return IndexState::Unstaged;
    };
    match (staged.contains(path), unstaged.contains(path)) {
        (false, false) => IndexState::Committed,
        (false, true) => IndexState::Unstaged,
        (true, false) => IndexState::Staged,
        (true, true) => IndexState::Partly,
    }
}

fn info(
    worktree: u64,
    state: git::review::State,
    paths: Option<(
        std::collections::BTreeSet<String>,
        std::collections::BTreeSet<String>,
    )>,
) -> ReviewInfo {
    let paths = paths.as_ref();
    let mut files: Vec<ReviewFile> = state
        .unreviewed
        .into_iter()
        .map(|diff| ReviewFile {
            path: diff.path.clone(),
            reviewed: if state.reviewed.contains(&diff.path) {
                Reviewed::Partly
            } else {
                Reviewed::No
            },
            index: index_state(&diff.path, paths),
            diff: Some(diff),
        })
        .collect();
    files.extend(state.too_large.into_iter().map(|path| ReviewFile {
        index: index_state(&path, paths),
        path,
        reviewed: Reviewed::TooLarge,
        diff: None,
    }));
    let done: Vec<ReviewFile> = state
        .reviewed
        .into_iter()
        .filter(|path| !files.iter().any(|file| &file.path == path))
        .map(|path| ReviewFile {
            index: index_state(&path, paths),
            path,
            reviewed: Reviewed::Yes,
            diff: None,
        })
        .collect();
    files.extend(done);
    ReviewInfo {
        worktree,
        base_ref: state.base_ref,
        base: state.base,
        files,
        error: None,
    }
}
