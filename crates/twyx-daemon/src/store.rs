//! The database: projects, worktrees, workspaces, tabs with their layouts,
//! and panes.
//!
//! Only the daemon opens it. The daemon writes its tabs and panes after each
//! change, in one transaction, so a crash leaves the last complete state.
//! Projects, worktrees and workspaces are written as they change.

use std::collections::BTreeMap;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use twyx_core::layout::{Layout, LeafId};
use twyx_core::pane::PaneId;
use twyx_protocol::client::{Launch, PaneState};

/// Each entry moves the schema one version on. Never edit one; add another.
const MIGRATIONS: &[&str] = &[
    "
    CREATE TABLE workspaces (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        directory TEXT NOT NULL UNIQUE,
        worktree TEXT,
        name TEXT NOT NULL,
        active_tab INTEGER
    );
    CREATE TABLE tabs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        workspace INTEGER NOT NULL REFERENCES workspaces(id),
        position INTEGER NOT NULL,
        name TEXT NOT NULL,
        layout TEXT NOT NULL,
        focus INTEGER
    );
    CREATE TABLE panes (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        workspace INTEGER NOT NULL REFERENCES workspaces(id),
        spawn_dir TEXT NOT NULL,
        spawn_worktree TEXT,
        command TEXT NOT NULL,
        env_overrides TEXT NOT NULL DEFAULT '[]',
        process_state TEXT NOT NULL,
        exit_code INTEGER,
        signal TEXT,
        ended_at INTEGER,
        last_title TEXT NOT NULL DEFAULT '',
        holder_major INTEGER NOT NULL,
        cols INTEGER NOT NULL,
        rows INTEGER NOT NULL
    );
    CREATE TABLE counters (
        name TEXT PRIMARY KEY,
        value INTEGER NOT NULL
    );
",
    "
    ALTER TABLE panes ADD COLUMN history INTEGER NOT NULL DEFAULT 1;
",
    "
    CREATE TABLE projects (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        kind TEXT NOT NULL,
        path TEXT NOT NULL UNIQUE,
        name TEXT NOT NULL,
        base_branch TEXT
    );
    CREATE TABLE worktrees (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        project INTEGER NOT NULL REFERENCES projects(id),
        path TEXT NOT NULL,
        admin_name TEXT NOT NULL,
        branch TEXT,
        removed_at INTEGER
    );
    CREATE UNIQUE INDEX worktrees_live_path ON worktrees (path) WHERE removed_at IS NULL;
    ALTER TABLE workspaces DROP COLUMN worktree;
    ALTER TABLE workspaces ADD COLUMN worktree INTEGER REFERENCES worktrees(id);
    ALTER TABLE workspaces ADD COLUMN project INTEGER REFERENCES projects(id);
    ALTER TABLE panes DROP COLUMN spawn_worktree;
    ALTER TABLE panes ADD COLUMN spawn_worktree INTEGER REFERENCES worktrees(id);
",
];

#[derive(Debug)]
pub(crate) struct Store {
    db: Connection,
}

#[derive(Debug, Clone)]
pub(crate) struct SavedTab {
    pub id: u64,
    pub workspace: u64,
    pub name: String,
    pub layout: Layout,
    pub focus: Option<LeafId>,
}

#[derive(Debug, Clone)]
pub(crate) struct SavedPane {
    pub id: PaneId,
    pub workspace: u64,
    pub cwd: String,
    pub spawn_worktree: Option<u64>,
    pub launch: Launch,
    pub state: PaneState,
    pub title: String,
    pub cols: u16,
    pub rows: u16,
    pub history: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedProject {
    pub id: u64,
    pub git: bool,
    /// The common Git dir, or the directory.
    pub path: String,
    pub name: String,
    pub base_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedWorktree {
    pub id: u64,
    pub project: u64,
    pub path: String,
    pub admin: String,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedWorkspace {
    pub id: u64,
    pub directory: String,
    pub name: String,
    pub project: Option<u64>,
    pub worktree: Option<u64>,
    pub active_tab: Option<u64>,
}

/// Everything the daemon last wrote. Removed worktrees are left out.
#[derive(Debug, Clone, Default)]
pub(crate) struct Saved {
    pub default_workspace: u64,
    /// The workspace a new client shows.
    pub active_workspace: u64,
    pub projects: Vec<SavedProject>,
    pub worktrees: Vec<SavedWorktree>,
    pub workspaces: Vec<SavedWorkspace>,
    pub tabs: Vec<SavedTab>,
    pub panes: Vec<SavedPane>,
    pub next_leaf: u64,
}

impl Store {
    /// Opens the database, creating it with mode 0600 in a 0700 directory.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("create {}", path.display()))?;
        let db = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        db.pragma_update(None, "journal_mode", "WAL")
            .context("use the write-ahead log")?;
        db.pragma_update(None, "foreign_keys", true)
            .context("turn on foreign keys")?;
        let mut store = Self { db };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: usize = self
            .db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .context("read the schema version")?;
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(version) {
            let transaction = self.db.transaction().context("start a migration")?;
            transaction
                .execute_batch(migration)
                .with_context(|| format!("migrate to schema {}", index + 1))?;
            transaction
                .pragma_update(None, "user_version", index + 1)
                .context("record the schema version")?;
            transaction.commit().context("commit a migration")?;
        }
        Ok(())
    }

    /// Loads everything, creating the default workspace at `home` on the
    /// first start.
    pub(crate) fn load(&self, home: &str) -> Result<Saved> {
        self.db
            .execute(
                "INSERT INTO workspaces (directory, name) VALUES (?1, 'default')
                 ON CONFLICT (directory) DO NOTHING",
                params![home],
            )
            .context("create the default workspace")?;
        let default_workspace: u64 = self
            .db
            .query_row(
                "SELECT id FROM workspaces WHERE directory = ?1",
                params![home],
                |row| row.get(0),
            )
            .context("read the default workspace")?;

        let projects = self.rows(
            "SELECT id, kind, path, name, base_branch FROM projects ORDER BY id",
            |row| {
                Ok(SavedProject {
                    id: row.get(0)?,
                    git: row.get::<_, String>(1)? == "git",
                    path: row.get(2)?,
                    name: row.get(3)?,
                    base_branch: row.get(4)?,
                })
            },
        )?;
        let worktrees = self.rows(
            "SELECT id, project, path, admin_name, branch FROM worktrees
             WHERE removed_at IS NULL ORDER BY id",
            |row| {
                Ok(SavedWorktree {
                    id: row.get(0)?,
                    project: row.get(1)?,
                    path: row.get(2)?,
                    admin: row.get(3)?,
                    branch: row.get(4)?,
                })
            },
        )?;
        let workspaces = self.rows(
            "SELECT id, directory, name, project, worktree, active_tab FROM workspaces ORDER BY id",
            |row| {
                Ok(SavedWorkspace {
                    id: row.get(0)?,
                    directory: row.get(1)?,
                    name: row.get(2)?,
                    project: row.get(3)?,
                    worktree: row.get(4)?,
                    active_tab: row.get(5)?,
                })
            },
        )?;

        let tabs = self
            .rows(
                "SELECT id, workspace, name, layout, focus FROM tabs ORDER BY workspace, position",
                |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<u64>>(4)?,
                    ))
                },
            )?
            .into_iter()
            .filter_map(|(id, workspace, name, layout, focus)| {
                // A layout this build cannot read loses its tab, not the daemon.
                match serde_json::from_str(&layout) {
                    Ok(layout) => Some(SavedTab {
                        id,
                        workspace,
                        name,
                        layout,
                        focus: focus.map(LeafId),
                    }),
                    Err(error) => {
                        eprintln!("twyx daemon: skip tab {id}: {error}");
                        None
                    }
                }
            })
            .collect();

        let panes = self.rows(
            "SELECT id, workspace, spawn_dir, spawn_worktree, command, process_state, exit_code,
                    signal, last_title, cols, rows, history
             FROM panes ORDER BY id",
            |row| {
                let state: String = row.get(5)?;
                let command: String = row.get(4)?;
                Ok(SavedPane {
                    id: PaneId::from_raw(row.get(0)?),
                    workspace: row.get(1)?,
                    cwd: row.get(2)?,
                    spawn_worktree: row.get(3)?,
                    launch: serde_json::from_str(&command).unwrap_or(Launch::Unknown),
                    state: match state.as_str() {
                        "running" => PaneState::Running,
                        "exited" => PaneState::Exited {
                            code: row.get::<_, Option<u32>>(6)?.unwrap_or(0),
                        },
                        "killed" => PaneState::Killed {
                            signal: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                        },
                        _ => PaneState::Lost,
                    },
                    title: row.get(8)?,
                    cols: row.get(9)?,
                    rows: row.get(10)?,
                    history: row.get(11)?,
                })
            },
        )?;

        let counter = |name: &str| {
            self.db
                .query_row(
                    "SELECT value FROM counters WHERE name = ?1",
                    params![name],
                    |row| row.get::<_, u64>(0),
                )
                .optional()
                .with_context(|| format!("read the {name} counter"))
        };
        let next_leaf = counter("leaf")?.unwrap_or(1);
        let active_workspace = counter("workspace")?
            .filter(|id| workspaces.iter().any(|workspace| workspace.id == *id))
            .unwrap_or(default_workspace);

        Ok(Saved {
            default_workspace,
            active_workspace,
            projects,
            worktrees,
            workspaces,
            tabs,
            panes,
            next_leaf,
        })
    }

    fn rows<T>(
        &self,
        sql: &str,
        map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>> {
        let mut statement = self
            .db
            .prepare(sql)
            .with_context(|| format!("read: {sql}"))?;
        let rows = statement
            .query_map([], map)
            .with_context(|| format!("read: {sql}"))?
            .collect::<rusqlite::Result<Vec<T>>>()
            .with_context(|| format!("read: {sql}"))?;
        Ok(rows)
    }

    /// Records a project, or finds the one at the same path.
    pub(crate) fn project(&self, git: bool, path: &str, name: &str) -> Result<u64> {
        self.db
            .execute(
                "INSERT INTO projects (kind, path, name) VALUES (?1, ?2, ?3)
                 ON CONFLICT (path) DO NOTHING",
                params![if git { "git" } else { "dir" }, path, name],
            )
            .context("record a project")?;
        self.db
            .query_row(
                "SELECT id FROM projects WHERE path = ?1",
                params![path],
                |row| row.get(0),
            )
            .context("read a project")
    }

    /// Records a worktree that Git listed, or updates the live one at the
    /// same path.
    pub(crate) fn worktree(
        &self,
        project: u64,
        path: &str,
        admin: &str,
        branch: Option<&str>,
    ) -> Result<u64> {
        let existing: Option<u64> = self
            .db
            .query_row(
                "SELECT id FROM worktrees WHERE path = ?1 AND removed_at IS NULL",
                params![path],
                |row| row.get(0),
            )
            .optional()
            .context("read a worktree")?;
        match existing {
            Some(id) => {
                self.db
                    .execute(
                        "UPDATE worktrees SET project = ?2, admin_name = ?3, branch = ?4
                         WHERE id = ?1",
                        params![id, project, admin, branch],
                    )
                    .context("update a worktree")?;
                Ok(id)
            }
            None => {
                self.db
                    .execute(
                        "INSERT INTO worktrees (project, path, admin_name, branch)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![project, path, admin, branch],
                    )
                    .context("record a worktree")?;
                Ok(self.db.last_insert_rowid() as u64)
            }
        }
    }

    pub(crate) fn set_branch(&self, worktree: u64, branch: Option<&str>) -> Result<()> {
        self.db
            .execute(
                "UPDATE worktrees SET branch = ?2 WHERE id = ?1",
                params![worktree, branch],
            )
            .context("update a branch")?;
        Ok(())
    }

    /// Marks a worktree removed and lets go of its workspace.
    pub(crate) fn remove_worktree(&self, worktree: u64) -> Result<()> {
        self.db
            .execute(
                "UPDATE worktrees SET removed_at = unixepoch() WHERE id = ?1",
                params![worktree],
            )
            .context("mark a worktree removed")?;
        self.db
            .execute(
                "UPDATE workspaces SET worktree = NULL WHERE worktree = ?1",
                params![worktree],
            )
            .context("detach a workspace from its worktree")?;
        Ok(())
    }

    /// Records the workspace of a directory, or points the existing one at
    /// this project and worktree.
    pub(crate) fn workspace(
        &self,
        directory: &str,
        name: &str,
        project: Option<u64>,
        worktree: Option<u64>,
    ) -> Result<u64> {
        self.db
            .execute(
                "INSERT INTO workspaces (directory, name, project, worktree) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (directory) DO UPDATE SET name = ?2, project = ?3, worktree = ?4",
                params![directory, name, project, worktree],
            )
            .context("record a workspace")?;
        self.db
            .query_row(
                "SELECT id FROM workspaces WHERE directory = ?1",
                params![directory],
                |row| row.get(0),
            )
            .context("read a workspace")
    }

    /// Deletes a workspace whose tabs and panes are gone.
    pub(crate) fn delete_workspace(&self, workspace: u64) -> Result<()> {
        self.db
            .execute("DELETE FROM tabs WHERE workspace = ?1", params![workspace])
            .context("delete a workspace's tabs")?;
        self.db
            .execute("DELETE FROM workspaces WHERE id = ?1", params![workspace])
            .context("delete a workspace")?;
        Ok(())
    }

    /// Reserves a tab id that no earlier tab had.
    pub(crate) fn new_tab(&self, workspace: u64) -> Result<u64> {
        let empty = serde_json::to_string(&Layout::default()).context("write a layout")?;
        self.db
            .execute(
                "INSERT INTO tabs (workspace, position, name, layout) VALUES (?1, -1, '', ?2)",
                params![workspace, empty],
            )
            .context("reserve a tab")?;
        Ok(self.db.last_insert_rowid() as u64)
    }

    /// Reserves a pane id that no earlier pane had, or records an adopted
    /// pane under its own id.
    pub(crate) fn new_pane(&self, pane: &SavedPane, id: Option<PaneId>) -> Result<PaneId> {
        self.db
            .execute(
                "INSERT INTO panes (id, workspace, spawn_dir, spawn_worktree, command,
                                    process_state, holder_major, cols, rows, history)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7, ?8, ?9)",
                params![
                    id.map(PaneId::raw),
                    pane.workspace,
                    pane.cwd,
                    pane.spawn_worktree,
                    serde_json::to_string(&pane.launch).context("write a command")?,
                    twyx_protocol::holder::MAJOR,
                    pane.cols,
                    pane.rows,
                    pane.history,
                ],
            )
            .context("reserve a pane")?;
        Ok(PaneId::from_raw(self.db.last_insert_rowid() as u64))
    }

    /// Writes every workspace's tabs and panes: rows that are gone are
    /// deleted.
    pub(crate) fn save(&mut self, saved: &Saved) -> Result<()> {
        let transaction = self.db.transaction().context("start a save")?;
        for workspace in &saved.workspaces {
            transaction
                .execute(
                    "UPDATE workspaces SET active_tab = ?1 WHERE id = ?2",
                    params![workspace.active_tab, workspace.id],
                )
                .context("save a workspace")?;
        }

        let tab_ids: Vec<String> = saved.tabs.iter().map(|tab| tab.id.to_string()).collect();
        transaction
            .execute(
                &format!("DELETE FROM tabs WHERE id NOT IN ({})", tab_ids.join(",")),
                [],
            )
            .context("delete closed tabs")?;
        let mut positions: BTreeMap<u64, usize> = BTreeMap::new();
        for tab in &saved.tabs {
            let position = positions.entry(tab.workspace).or_default();
            transaction
                .execute(
                    "INSERT INTO tabs (id, workspace, position, name, layout, focus)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT (id) DO UPDATE SET workspace = ?2, position = ?3, name = ?4,
                                                    layout = ?5, focus = ?6",
                    params![
                        tab.id,
                        tab.workspace,
                        *position,
                        tab.name,
                        serde_json::to_string(&tab.layout).context("write a layout")?,
                        tab.focus.map(|leaf| leaf.0),
                    ],
                )
                .context("save a tab")?;
            *position += 1;
        }

        let pane_ids: Vec<String> = saved
            .panes
            .iter()
            .map(|pane| pane.id.raw().to_string())
            .collect();
        transaction
            .execute(
                &format!("DELETE FROM panes WHERE id NOT IN ({})", pane_ids.join(",")),
                [],
            )
            .context("delete closed panes")?;
        for pane in &saved.panes {
            let (state, code, signal) = match &pane.state {
                PaneState::Running => ("running", None, None),
                PaneState::Exited { code } => ("exited", Some(*code), None),
                PaneState::Killed { signal } => ("killed", None, Some(signal.as_str())),
                PaneState::Lost => ("lost", None, None),
            };
            transaction
                .execute(
                    "UPDATE panes SET process_state = ?2, exit_code = ?3, signal = ?4,
                         ended_at = CASE WHEN ?2 = 'running' THEN NULL
                                         ELSE coalesce(ended_at, unixepoch()) END,
                         last_title = ?5, cols = ?6, rows = ?7, workspace = ?8
                     WHERE id = ?1",
                    params![
                        pane.id.raw(),
                        state,
                        code,
                        signal,
                        pane.title,
                        pane.cols,
                        pane.rows,
                        pane.workspace,
                    ],
                )
                .context("save a pane")?;
        }

        for (name, value) in [
            ("leaf", saved.next_leaf),
            ("workspace", saved.active_workspace),
        ] {
            transaction
                .execute(
                    "INSERT INTO counters (name, value) VALUES (?1, ?2)
                     ON CONFLICT (name) DO UPDATE SET value = ?2",
                    params![name, value],
                )
                .with_context(|| format!("save the {name} counter"))?;
        }
        transaction.commit().context("commit a save")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_database_from_before_worktrees_keeps_its_tabs_and_panes() {
        let dir = std::env::temp_dir().join(format!("twyx-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("twyx.db");
        std::fs::create_dir_all(&dir).unwrap();
        {
            let db = Connection::open(&path).unwrap();
            for migration in &MIGRATIONS[..2] {
                db.execute_batch(migration).unwrap();
            }
            db.pragma_update(None, "user_version", 2).unwrap();
            db.execute_batch(
                "INSERT INTO workspaces (id, directory, worktree, name, active_tab)
                     VALUES (1, '/home/n', NULL, 'default', 5);
                 INSERT INTO tabs (id, workspace, position, name, layout)
                     VALUES (5, 1, 0, 'logs', '');
                 INSERT INTO panes (id, workspace, spawn_dir, command, process_state,
                                    holder_major, cols, rows)
                     VALUES (9, 1, '/home/n/src', '\"Shell\"', 'exited', 1, 80, 24);",
            )
            .unwrap();
            db.execute(
                "UPDATE tabs SET layout = ?1 WHERE id = 5",
                params![serde_json::to_string(&Layout::default()).unwrap()],
            )
            .unwrap();
        }
        let store = Store::open(&path).unwrap();
        let saved = store.load("/home/n").unwrap();
        assert_eq!(saved.default_workspace, 1);
        assert_eq!(saved.workspaces[0].active_tab, Some(5));
        assert_eq!(saved.workspaces[0].worktree, None);
        assert_eq!(saved.tabs[0].name, "logs");
        assert_eq!(saved.panes[0].cwd, "/home/n/src");
        assert_eq!(saved.panes[0].spawn_worktree, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
