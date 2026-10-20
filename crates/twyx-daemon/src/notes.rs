//! Review notes: anchored to lines of a file at a blob, moved with the
//! diff when the file changes, or marked outdated when their lines change.

use std::path::PathBuf;

use twyx_core::diff::move_lines;
use twyx_core::pane::PaneId;
use twyx_protocol::client::{FromDaemon, NoteInfo, NoteState};

use crate::daemon::Daemon;
use crate::git;
use crate::inject::Purpose;
use crate::store::SavedNote;
use crate::worktrees::Done;

/// Where an open note is now: new blob and lines, or `None` when outdated.
pub(crate) type Moved = Vec<(u64, Option<(String, (u32, u32))>)>;

impl Daemon {
    /// Sends a worktree's notes, with the open ones moved to the file as it
    /// is now.
    pub(crate) fn notes(&mut self, client: u64, worktree: u64) {
        let Some(path) = self
            .worktrees
            .get(&worktree)
            .map(|entry| PathBuf::from(&entry.path))
        else {
            self.refuse(client, format!("no worktree {worktree}"));
            return;
        };
        let open: Vec<SavedNote> = match self.store.notes(worktree) {
            Ok(notes) => notes
                .into_iter()
                .filter(|note| note.state == NoteState::Open)
                .collect(),
            Err(error) => {
                self.refuse(client, format!("{error:#}"));
                return;
            }
        };
        self.job(move || {
            let moved = open
                .into_iter()
                .filter_map(|note| {
                    let now = git::review::store_file(&path, &note.path).ok();
                    match now {
                        Some(now) if now == note.blob => None,
                        Some(now) => {
                            let lines = git::review::blob_hunks(&path, &note.blob, &now)
                                .ok()
                                .and_then(|hunks| move_lines(note.lines.0, note.lines.1, &hunks));
                            Some((note.id, lines.map(|lines| (now, lines))))
                        }
                        // The file is gone.
                        None => Some((note.id, None)),
                    }
                })
                .collect();
            Done::NotesMoved {
                client,
                worktree,
                moved,
            }
        });
    }

    pub(crate) fn notes_moved(&mut self, client: u64, worktree: u64, moved: Moved) {
        for (id, to) in &moved {
            let to = to.as_ref().map(|(blob, lines)| (blob.as_str(), *lines));
            if let Err(error) = self.store.move_note(*id, to) {
                eprintln!("twyx daemon: {error:#}");
            }
        }
        let notes = self
            .store
            .notes(worktree)
            .unwrap_or_default()
            .into_iter()
            .map(|note| NoteInfo {
                note: note.id,
                path: note.path,
                lines: note.lines,
                body: note.body,
                state: note.state,
            })
            .collect();
        self.reply(client, FromDaemon::Notes { worktree, notes });
    }

    pub(crate) fn add_note(
        &mut self,
        client: u64,
        worktree: u64,
        path: String,
        lines: (u32, u32),
        body: String,
    ) {
        let Some(root) = self
            .worktrees
            .get(&worktree)
            .map(|entry| PathBuf::from(&entry.path))
        else {
            self.refuse(client, format!("no worktree {worktree}"));
            return;
        };
        self.job(move || {
            let blob = git::review::store_file(&root, &path).map_err(|error| format!("{error:#}"));
            Done::NoteAnchored {
                client,
                worktree,
                note: blob.map(|blob| SavedNote {
                    id: 0,
                    path,
                    blob,
                    lines,
                    body,
                    state: NoteState::Open,
                }),
            }
        });
    }

    pub(crate) fn note_anchored(
        &mut self,
        client: u64,
        worktree: u64,
        note: Result<SavedNote, String>,
    ) {
        match note.map(|note| self.store.add_note(worktree, &note)) {
            Ok(Ok(())) => self.notes(client, worktree),
            Ok(Err(error)) => self.refuse(client, format!("{error:#}")),
            Err(reason) => self.refuse(client, reason),
        }
    }

    /// Sends the open notes, as one Markdown text, to an agent pane.
    pub(crate) fn send_notes(&mut self, client: u64, worktree: u64, pane: PaneId) {
        let notes: Vec<SavedNote> = self
            .store
            .notes(worktree)
            .unwrap_or_default()
            .into_iter()
            .filter(|note| note.state == NoteState::Open)
            .collect();
        if notes.is_empty() {
            self.refuse(client, "no open notes to send".to_owned());
            return;
        }
        let mut text = String::from("Review notes:\n");
        for note in &notes {
            let lines = if note.lines.0 == note.lines.1 {
                format!("line {}", note.lines.0)
            } else {
                format!("lines {}-{}", note.lines.0, note.lines.1)
            };
            text.push_str(&format!("\n- `{}` {lines}: {}", note.path, note.body));
        }
        let ids = notes.iter().map(|note| note.id).collect();
        self.inject(client, pane, text, Purpose::Notes(ids));
    }

    pub(crate) fn notes_sent(&mut self, ids: &[u64]) {
        if let Err(error) = self.store.notes_sent(ids) {
            eprintln!("twyx daemon: {error:#}");
        }
    }
}
