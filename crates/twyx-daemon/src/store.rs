//! The database: workspaces, tabs with their layouts, and panes.
//!
//! Only the daemon opens it. The daemon writes its whole workspace after each
//! change, in one transaction, so a crash leaves the last complete state.

use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use twyx_core::layout::{Layout, LeafId};
use twyx_core::pane::PaneId;
use twyx_protocol::client::{Launch, PaneState};

/// Each entry moves the schema one version on. Never edit one; add another.
const MIGRATIONS: &[&str] = &["
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
"];

#[derive(Debug)]
pub(crate) struct Store {
    db: Connection,
}

#[derive(Debug, Clone)]
pub(crate) struct SavedTab {
    pub id: u64,
    pub name: String,
    pub layout: Layout,
    pub focus: Option<LeafId>,
}

#[derive(Debug, Clone)]
pub(crate) struct SavedPane {
    pub id: PaneId,
    pub cwd: String,
    pub launch: Launch,
    pub state: PaneState,
    pub title: String,
    pub cols: u16,
    pub rows: u16,
}

/// The default workspace, as the daemon last wrote it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Saved {
    pub workspace: u64,
    pub active_tab: Option<u64>,
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

    /// Loads the default workspace at `home`, creating it on the first start.
    pub(crate) fn load(&self, home: &str) -> Result<Saved> {
        self.db
            .execute(
                "INSERT INTO workspaces (directory, name) VALUES (?1, 'default')
                 ON CONFLICT (directory) DO NOTHING",
                params![home],
            )
            .context("create the default workspace")?;
        let (workspace, active_tab): (u64, Option<u64>) = self
            .db
            .query_row(
                "SELECT id, active_tab FROM workspaces WHERE directory = ?1",
                params![home],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .context("read the default workspace")?;

        let mut statement = self
            .db
            .prepare(
                "SELECT id, name, layout, focus FROM tabs WHERE workspace = ?1 ORDER BY position",
            )
            .context("read the tabs")?;
        let tabs = statement
            .query_map(params![workspace], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<u64>>(3)?,
                ))
            })
            .context("read the tabs")?
            .filter_map(|row| {
                let (id, name, layout, focus) = match row.context("read a tab") {
                    Ok(row) => row,
                    Err(error) => return Some(Err(error)),
                };
                // A layout this build cannot read loses its tab, not the daemon.
                let layout = match serde_json::from_str(&layout) {
                    Ok(layout) => layout,
                    Err(error) => {
                        eprintln!("twyx daemon: skip tab {id}: {error}");
                        return None;
                    }
                };
                Some(Ok(SavedTab {
                    id,
                    name,
                    layout,
                    focus: focus.map(LeafId),
                }))
            })
            .collect::<Result<Vec<_>>>()?;

        let mut statement = self
            .db
            .prepare(
                "SELECT id, spawn_dir, command, process_state, exit_code, signal, last_title,
                        cols, rows
                 FROM panes WHERE workspace = ?1 ORDER BY id",
            )
            .context("read the panes")?;
        let panes = statement
            .query_map(params![workspace], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<u32>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, u16>(7)?,
                    row.get::<_, u16>(8)?,
                ))
            })
            .context("read the panes")?
            .map(|row| {
                let (id, cwd, command, state, code, signal, title, cols, rows) =
                    row.context("read a pane")?;
                Ok(SavedPane {
                    id: PaneId::from_raw(id),
                    cwd,
                    launch: serde_json::from_str(&command).unwrap_or(Launch::Unknown),
                    state: match state.as_str() {
                        "running" => PaneState::Running,
                        "exited" => PaneState::Exited {
                            code: code.unwrap_or(0),
                        },
                        "killed" => PaneState::Killed {
                            signal: signal.unwrap_or_default(),
                        },
                        _ => PaneState::Lost,
                    },
                    title,
                    cols,
                    rows,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let next_leaf = self
            .db
            .query_row(
                "SELECT value FROM counters WHERE name = 'leaf'",
                [],
                |row| row.get::<_, u64>(0),
            )
            .optional()
            .context("read the leaf counter")?
            .unwrap_or(1);

        Ok(Saved {
            workspace,
            active_tab,
            tabs,
            panes,
            next_leaf,
        })
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
    pub(crate) fn new_pane(
        &self,
        workspace: u64,
        pane: &SavedPane,
        id: Option<PaneId>,
    ) -> Result<PaneId> {
        self.db
            .execute(
                "INSERT INTO panes (id, workspace, spawn_dir, command, process_state,
                                    holder_major, cols, rows)
                 VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7)",
                params![
                    id.map(PaneId::raw),
                    workspace,
                    pane.cwd,
                    serde_json::to_string(&pane.launch).context("write a command")?,
                    twyx_protocol::holder::MAJOR,
                    pane.cols,
                    pane.rows,
                ],
            )
            .context("reserve a pane")?;
        Ok(PaneId::from_raw(self.db.last_insert_rowid() as u64))
    }

    /// Writes the whole workspace: rows that are gone are deleted.
    pub(crate) fn save(&mut self, saved: &Saved) -> Result<()> {
        let transaction = self.db.transaction().context("start a save")?;
        transaction
            .execute(
                "UPDATE workspaces SET active_tab = ?1 WHERE id = ?2",
                params![saved.active_tab, saved.workspace],
            )
            .context("save the workspace")?;

        let tab_ids: Vec<String> = saved.tabs.iter().map(|tab| tab.id.to_string()).collect();
        transaction
            .execute(
                &format!(
                    "DELETE FROM tabs WHERE workspace = ?1 AND id NOT IN ({})",
                    tab_ids.join(",")
                ),
                params![saved.workspace],
            )
            .context("delete closed tabs")?;
        for (position, tab) in saved.tabs.iter().enumerate() {
            transaction
                .execute(
                    "INSERT INTO tabs (id, workspace, position, name, layout, focus)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT (id) DO UPDATE SET position = ?3, name = ?4, layout = ?5,
                                                    focus = ?6",
                    params![
                        tab.id,
                        saved.workspace,
                        position,
                        tab.name,
                        serde_json::to_string(&tab.layout).context("write a layout")?,
                        tab.focus.map(|leaf| leaf.0),
                    ],
                )
                .context("save a tab")?;
        }

        let pane_ids: Vec<String> = saved
            .panes
            .iter()
            .map(|pane| pane.id.raw().to_string())
            .collect();
        transaction
            .execute(
                &format!(
                    "DELETE FROM panes WHERE workspace = ?1 AND id NOT IN ({})",
                    pane_ids.join(",")
                ),
                params![saved.workspace],
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
                         last_title = ?5, cols = ?6, rows = ?7
                     WHERE id = ?1",
                    params![
                        pane.id.raw(),
                        state,
                        code,
                        signal,
                        pane.title,
                        pane.cols,
                        pane.rows
                    ],
                )
                .context("save a pane")?;
        }

        transaction
            .execute(
                "INSERT INTO counters (name, value) VALUES ('leaf', ?1)
                 ON CONFLICT (name) DO UPDATE SET value = ?1",
                params![saved.next_leaf],
            )
            .context("save the leaf counter")?;
        transaction.commit().context("commit a save")
    }
}
