//! Review requests: each runs as a background job, one at a time per
//! worktree, since they share its review index.

use std::sync::{Arc, Mutex};

use twyx_protocol::client::{ReviewFile, ReviewInfo, Reviewed};

use crate::daemon::Daemon;
use crate::git;
use crate::worktrees::Done;

impl Daemon {
    /// Sends a worktree's review, after marking `mark` reviewed: a path and
    /// its hunks, or all of it.
    pub(crate) fn review(
        &mut self,
        client: u64,
        worktree: u64,
        mark: Option<(String, Option<Vec<u64>>)>,
    ) {
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
            let marked = match &mark {
                Some((file, hunks)) => {
                    git::review::mark(&path, &name, base_branch.as_deref(), file, hunks.as_deref())
                        .err()
                }
                None => None,
            };
            let mut review = match git::review::state(&path, &name, base_branch.as_deref()) {
                Ok(state) => info(worktree, state),
                Err(error) => ReviewInfo {
                    worktree,
                    base_ref: String::new(),
                    base: String::new(),
                    files: Vec::new(),
                    error: Some(format!("{error:#}")),
                },
            };
            if let Some(error) = marked {
                review.error = Some(format!("{error:#}"));
            }
            Done::Reviewed { client, review }
        });
    }
}

fn info(worktree: u64, state: git::review::State) -> ReviewInfo {
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
            diff: Some(diff),
        })
        .collect();
    files.extend(state.too_large.into_iter().map(|path| ReviewFile {
        path,
        reviewed: Reviewed::TooLarge,
        diff: None,
    }));
    let done: Vec<ReviewFile> = state
        .reviewed
        .into_iter()
        .filter(|path| !files.iter().any(|file| &file.path == path))
        .map(|path| ReviewFile {
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
