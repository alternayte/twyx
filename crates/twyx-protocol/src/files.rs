//! A workspace's files as both clients show them: the file tree with its
//! keys, and the finder that searches their text.

use std::collections::BTreeSet;

use twyx_core::keys::{Chord, Key};

use crate::client::{FileList, FoundText, Hit, ToDaemon};

/// One visible row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// The file's or directory's path, relative to the root.
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub directory: bool,
    pub open: bool,
}

/// The request that opens a file: a Markdown file as a document, any
/// other in Neovim.
pub fn open(path: String) -> ToDaemon {
    if twyx_core::markdown::is_markdown(&path) {
        ToDaemon::OpenDocument { path }
    } else {
        ToDaemon::OpenInEditor { path, line: None }
    }
}

/// What a key in the tree asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum TreeKey {
    Stay,
    /// Give the keys back to the panes.
    Leave,
    Send(ToDaemon),
}

/// The tree of a workspace's files: which directories are open and which
/// row the cursor is on.
#[derive(Debug, Clone, Default)]
pub struct FileTree {
    pub list: Option<FileList>,
    open: BTreeSet<String>,
    pub cursor: usize,
}

impl FileTree {
    /// Takes a new list; a list for another workspace closes every
    /// directory.
    pub fn answer(&mut self, list: FileList) {
        if self
            .list
            .as_ref()
            .is_some_and(|known| known.workspace != list.workspace)
        {
            self.open.clear();
            self.cursor = 0;
        }
        self.list = Some(list);
    }

    /// The rows to show: every top entry, and the entries of each open
    /// directory under it, directories first, then files, each by name.
    pub fn rows(&self) -> Vec<TreeRow> {
        let Some(list) = &self.list else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        self.add_rows(&list.paths, "", 0, &mut rows);
        rows
    }

    fn add_rows(&self, paths: &[String], prefix: &str, depth: usize, rows: &mut Vec<TreeRow>) {
        let mut directories: Vec<&str> = Vec::new();
        let mut files: Vec<&str> = Vec::new();
        for path in paths {
            let Some(rest) = path.strip_prefix(prefix) else {
                continue;
            };
            match rest.split_once('/') {
                Some((directory, _)) => {
                    if directories.last() != Some(&directory) {
                        directories.push(directory);
                    }
                }
                None => files.push(rest),
            }
        }
        directories.dedup();
        for directory in directories {
            let path = format!("{prefix}{directory}");
            let open = self.open.contains(&path);
            rows.push(TreeRow {
                name: directory.to_owned(),
                path: path.clone(),
                depth,
                directory: true,
                open,
            });
            if open {
                let inner = format!("{path}/");
                let start = paths.partition_point(|each| each.as_str() < inner.as_str());
                let end =
                    start + paths[start..].partition_point(|each| each.starts_with(inner.as_str()));
                self.add_rows(&paths[start..end], &inner, depth + 1, rows);
            }
        }
        for file in files {
            rows.push(TreeRow {
                name: file.to_owned(),
                path: format!("{prefix}{file}"),
                depth,
                directory: false,
                open: false,
            });
        }
    }

    /// Up and down move; enter opens a file in Neovim or opens and closes
    /// a directory; right opens a directory, left closes it or goes to its
    /// parent; esc gives the keys back.
    pub fn key(&mut self, chord: Chord) -> TreeKey {
        let rows = self.rows();
        self.cursor = self.cursor.min(rows.len().saturating_sub(1));
        let typed = match chord.key {
            Key::Char(ch) if !chord.ctrl && !chord.alt => Some(ch),
            _ => None,
        };
        let Some(row) = rows.get(self.cursor).cloned() else {
            return match chord.key {
                Key::Escape => TreeKey::Leave,
                _ => TreeKey::Stay,
            };
        };
        match (chord.key, typed) {
            (Key::Escape, _) => return TreeKey::Leave,
            (Key::Down, _) | (_, Some('j')) => {
                self.cursor = (self.cursor + 1).min(rows.len() - 1);
            }
            (Key::Up, _) | (_, Some('k')) => self.cursor = self.cursor.saturating_sub(1),
            (Key::Enter, _) | (_, Some(' ')) if !row.directory => {
                return TreeKey::Send(open(row.path));
            }
            (Key::Enter, _) | (_, Some(' ')) => self.toggle(&row.path),
            (Key::Right, _) | (_, Some('l')) if row.directory => {
                self.open.insert(row.path);
            }
            (Key::Left, _) | (_, Some('h')) => {
                if row.directory && row.open {
                    self.open.remove(&row.path);
                } else if let Some((parent, _)) = row.path.rsplit_once('/')
                    && let Some(at) = rows.iter().position(|each| each.path == parent)
                {
                    self.cursor = at;
                }
            }
            _ => {}
        }
        TreeKey::Stay
    }

    /// Opens a closed directory, or closes an open one.
    pub fn toggle(&mut self, directory: &str) {
        if !self.open.remove(directory) {
            self.open.insert(directory.to_owned());
        }
    }
}

/// What a key in the finder asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum FindKey {
    Stay,
    Close,
    Send(ToDaemon),
}

/// The search of a workspace's text while it is open: the query, the
/// results of the latest query the daemon answered, and the choice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Finder {
    pub workspace: u64,
    pub query: String,
    pub selected: usize,
    pub found: Option<FoundText>,
}

impl Finder {
    pub fn new(workspace: u64) -> Self {
        Self {
            workspace,
            ..Self::default()
        }
    }

    /// Takes an answer for this workspace. An answer to an older query
    /// still shows until the newest one comes.
    pub fn answer(&mut self, found: FoundText) {
        if found.workspace != self.workspace {
            return;
        }
        let newest = found.query == self.query;
        if newest || self.found.is_none() {
            self.found = Some(found);
            self.selected = 0;
        }
    }

    /// The hits of the shown results.
    pub fn hits(&self) -> &[Hit] {
        self.found
            .as_ref()
            .map_or(&[], |found| found.hits.as_slice())
    }

    /// Typing changes the query and asks again; up and down move; enter
    /// opens the line in Neovim; esc closes.
    pub fn key(&mut self, chord: Chord) -> FindKey {
        let typed = match chord.key {
            Key::Char(ch) if !chord.ctrl && !chord.alt => Some(ch),
            _ => None,
        };
        match (chord.key, typed) {
            (Key::Escape, _) => FindKey::Close,
            (Key::Enter, _) => match self.hits().get(self.selected) {
                Some(hit) => FindKey::Send(ToDaemon::OpenInEditor {
                    path: hit.path.clone(),
                    line: Some(hit.line),
                }),
                None => FindKey::Stay,
            },
            (Key::Up, _) => {
                self.selected = self.selected.saturating_sub(1);
                FindKey::Stay
            }
            (Key::Down, _) => {
                self.selected = (self.selected + 1).min(self.hits().len().saturating_sub(1));
                FindKey::Stay
            }
            (Key::Backspace, _) => {
                self.query.pop();
                self.ask()
            }
            (_, Some(ch)) => {
                self.query.push(ch);
                self.ask()
            }
            _ => FindKey::Stay,
        }
    }

    /// Pasted text goes into the query.
    pub fn paste(&mut self, text: &str) -> FindKey {
        self.query.push_str(text);
        self.ask()
    }

    fn ask(&mut self) -> FindKey {
        FindKey::Send(ToDaemon::FindInFiles {
            workspace: self.workspace,
            query: self.query.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[&str]) -> FileTree {
        let mut tree = FileTree::default();
        tree.answer(FileList {
            workspace: 1,
            root: "/r".to_owned(),
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            truncated: false,
            error: None,
        });
        tree
    }

    fn names(tree: &FileTree) -> Vec<String> {
        tree.rows()
            .iter()
            .map(|row| format!("{}{}", "  ".repeat(row.depth), row.name))
            .collect()
    }

    #[test]
    fn the_tree_shows_directories_first_and_opens_them_on_enter() {
        let mut tree = tree(&["README", "src/a.rs", "src/deep/b.rs", "zz/c"]);
        assert_eq!(names(&tree), ["src", "zz", "README"]);
        let key = |ch| Chord::plain(Key::Char(ch));
        assert_eq!(tree.key(Chord::plain(Key::Enter)), TreeKey::Stay);
        assert_eq!(names(&tree), ["src", "  deep", "  a.rs", "zz", "README"]);
        tree.key(key('j'));
        tree.key(key('l'));
        assert_eq!(
            names(&tree),
            ["src", "  deep", "    b.rs", "  a.rs", "zz", "README"]
        );
        tree.key(key('j'));
        assert_eq!(
            tree.key(Chord::plain(Key::Enter)),
            TreeKey::Send(ToDaemon::OpenInEditor {
                path: "src/deep/b.rs".to_owned(),
                line: None,
            })
        );
        // Left on a file goes to its directory; left again closes it.
        tree.key(key('h'));
        tree.key(key('h'));
        assert_eq!(names(&tree), ["src", "  deep", "  a.rs", "zz", "README"]);
    }

    #[test]
    fn the_finder_shows_older_results_until_the_newest_come() {
        let mut finder = Finder::new(1);
        let found = |query: &str, path: &str| FoundText {
            workspace: 1,
            query: query.to_owned(),
            root: "/r".to_owned(),
            hits: vec![Hit {
                path: path.to_owned(),
                line: 2,
                text: String::new(),
            }],
            truncated: false,
            error: None,
        };
        finder.key(Chord::plain(Key::Char('a')));
        finder.key(Chord::plain(Key::Char('b')));
        finder.answer(found("a", "old"));
        assert_eq!(finder.hits()[0].path, "old");
        finder.answer(found("ab", "new"));
        finder.answer(found("a", "late"));
        assert_eq!(
            finder.hits()[0].path,
            "new",
            "a late answer to an older query is dropped"
        );
        assert_eq!(
            finder.key(Chord::plain(Key::Enter)),
            FindKey::Send(ToDaemon::OpenInEditor {
                path: "new".to_owned(),
                line: Some(2),
            })
        );
    }
}
