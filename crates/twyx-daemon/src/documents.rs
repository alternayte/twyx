//! Document leaves: a file of a workspace shown as a Markdown document.
//! The daemon reads the file; each client parses and draws it.

use std::io::Read;
use std::path::PathBuf;

use twyx_core::layout::{Axis, Content, Leaf, LeafId};
use twyx_core::tuning;
use twyx_protocol::client::DocumentText;

use crate::daemon::Daemon;
use crate::worktrees::Done;

impl Daemon {
    /// Shows the document leaf of `path`, or opens one in a new tab named
    /// after the file, in the workspace whose directory holds the file.
    pub(crate) fn open_document(&mut self, client: u64, path: String, switch: bool) {
        let own = self.client_workspace(client);
        let path = match self.file_root(own) {
            Some((root, _)) => root.join(&path),
            None => PathBuf::from(&path),
        };
        let path = match path.canonicalize() {
            Ok(path) if path.is_file() => path,
            _ => {
                self.refuse(client, format!("{} is not a file", path.display()));
                return;
            }
        };
        let workspace = self.workspace_holding(&path, own);
        let path = path.to_string_lossy().into_owned();
        let content = Content::Document(path.clone());
        let existing = self.workspace_tabs(workspace).into_iter().find_map(|tab| {
            let leaf = self
                .tab(tab)?
                .layout
                .leaves()
                .into_iter()
                .find(|leaf| leaf.content == content)?;
            Some((tab, leaf.id))
        });
        let (tab, leaf) = match existing {
            Some(found) => found,
            None => {
                let name = std::path::Path::new(&path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let Some(tab) = self.add_tab(workspace, name) else {
                    self.refuse(client, "could not record a new tab".to_owned());
                    return;
                };
                let id = LeafId(self.next_leaf);
                self.next_leaf += 1;
                if let Some(entry) = self.tab_mut(tab) {
                    entry.layout.split(None, Leaf { id, content }, Axis::Row);
                    entry.focus = Some(id);
                }
                (tab, id)
            }
        };
        let interactive = self.client(client).is_some_and(|entry| entry.interactive);
        if interactive || switch {
            self.show_leaf(client, workspace, leaf);
        } else {
            // A client that attaches to the workspace lands on it.
            if let Some(entry) = self.tab_mut(tab) {
                entry.focus = Some(leaf);
            }
            if let Some(entry) = self.workspaces.get_mut(&workspace) {
                entry.active_tab = Some(tab);
            }
            self.broadcast_workspace();
        }
    }

    /// The workspace whose directory holds `path` most closely; `own`
    /// when none does.
    fn workspace_holding(&self, path: &std::path::Path, own: u64) -> u64 {
        let holds = |workspace: u64| {
            self.file_root(workspace)
                .and_then(|(root, _)| root.canonicalize().ok())
                .filter(|root| path.starts_with(root))
                .map(|root| root.components().count())
        };
        self.workspaces
            .keys()
            .filter_map(|workspace| Some((holds(*workspace)?, *workspace)))
            .max()
            .map_or(own, |(_, workspace)| workspace)
    }

    /// Sends a document leaf's text, unless the file is unchanged since
    /// `since`.
    pub(crate) fn document(&mut self, client: u64, leaf: LeafId, since: Option<i64>) {
        let found = self.tabs.iter().find_map(|(_, tab)| {
            let found = tab
                .layout
                .leaves()
                .into_iter()
                .find(|each| each.id == leaf)?;
            match found.content {
                Content::Document(path) => Some((tab.workspace, path)),
                _ => None,
            }
        });
        let Some((workspace, path)) = found else {
            self.refuse(client, format!("no document leaf {}", leaf.0));
            return;
        };
        let root = self
            .file_root(workspace)
            .map(|(root, _)| root.to_string_lossy().into_owned())
            .unwrap_or_default();
        let modified = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX));
        if modified.is_some() && modified == since {
            return;
        }
        self.job(move || {
            let mut text = Vec::new();
            let read = std::fs::File::open(&path)
                .and_then(|file| file.take(tuning::DOCUMENT_BYTES + 1).read_to_end(&mut text));
            let truncated = text.len() as u64 > tuning::DOCUMENT_BYTES;
            text.truncate(tuning::DOCUMENT_BYTES as usize);
            let (text, error) = match read {
                Ok(_) => (String::from_utf8_lossy(&text).into_owned(), None),
                Err(error) => (String::new(), Some(format!("{path}: {error}"))),
            };
            Done::Document {
                client,
                document: DocumentText {
                    leaf,
                    path,
                    root,
                    text,
                    modified,
                    truncated,
                    error,
                },
            }
        });
    }
}
