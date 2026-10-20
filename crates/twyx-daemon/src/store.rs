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

use twyx_core::agent::{AgentState, Source};
use twyx_core::check::Outcome;
use twyx_core::inbox::Reason;
use twyx_core::layout::{Layout, LeafId};
use twyx_core::pane::PaneId;
use twyx_protocol::client::{CheckRun, Launch, NoteState, PaneState};

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
    "
    CREATE TABLE trust (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        project INTEGER NOT NULL REFERENCES projects(id),
        path TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        granted_at INTEGER NOT NULL,
        UNIQUE (project, path, content_hash)
    );
",
    "
    CREATE TABLE agent_sessions (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        pane INTEGER NOT NULL REFERENCES panes(id) ON DELETE CASCADE,
        kind TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        ended_at INTEGER,
        reference TEXT,
        state TEXT NOT NULL,
        source TEXT NOT NULL,
        observed_at INTEGER NOT NULL,
        pid INTEGER
    );
",
    "
    CREATE TABLE inbox_items (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        pane INTEGER NOT NULL REFERENCES panes(id) ON DELETE CASCADE,
        agent_session INTEGER REFERENCES agent_sessions(id) ON DELETE SET NULL,
        kind TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        read_at INTEGER,
        dismissed_at INTEGER
    );
",
    "
    CREATE TABLE review_notes (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        worktree INTEGER NOT NULL REFERENCES worktrees(id),
        path TEXT NOT NULL,
        blob TEXT NOT NULL,
        line_start INTEGER NOT NULL,
        line_end INTEGER NOT NULL,
        body TEXT NOT NULL,
        state TEXT NOT NULL,
        sent_at INTEGER
    );
",
    "
    CREATE TABLE check_runs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        worktree INTEGER NOT NULL REFERENCES worktrees(id),
        name TEXT NOT NULL,
        command TEXT NOT NULL,
        pane INTEGER REFERENCES panes(id) ON DELETE SET NULL,
        key_before TEXT NOT NULL,
        key_after TEXT,
        exit INTEGER,
        result TEXT,
        output_tail TEXT NOT NULL DEFAULT '',
        timeout_ms INTEGER,
        started_at INTEGER NOT NULL,
        ended_at INTEGER
    );
    CREATE INDEX check_runs_by_worktree ON check_runs (worktree, name);
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
    pub env: Vec<(String, String)>,
    pub launch: Launch,
    pub state: PaneState,
    pub title: String,
    pub cols: u16,
    pub rows: u16,
    pub history: bool,
    /// The agent session that ran in the pane when it was last written.
    pub agent: Option<SavedAgent>,
}

/// One `agent_sessions` row. `ended_at` is set when the agent ends and its
/// pane lives on; a session that ran when its pane ended keeps it empty, so
/// it can be resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedAgent {
    pub row: i64,
    pub kind: String,
    pub reference: Option<String>,
    pub state: AgentState,
    pub source: Source,
    pub observed_ms: u64,
    pub pid: Option<u32>,
}

/// One `review_notes` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedNote {
    pub id: u64,
    pub path: String,
    pub blob: String,
    pub lines: (u32, u32),
    pub body: String,
    pub state: NoteState,
}

/// One `inbox_items` row that is not dismissed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedItem {
    pub id: u64,
    pub pane: PaneId,
    pub reason: Reason,
    pub created_ms: u64,
    pub read: bool,
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
        // A database or directory left from before keeps no looser mode:
        // the database holds the last lines of check output.
        use std::os::unix::fs::PermissionsExt;
        if let Some(parent) = path.parent() {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("set the mode of {}", parent.display()))?;
        }
        for file in [
            path.to_path_buf(),
            path.with_extension("db-wal"),
            path.with_extension("db-shm"),
        ] {
            if file.exists() {
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
                    .with_context(|| format!("set the mode of {}", file.display()))?;
            }
        }
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
                    signal, last_title, cols, rows, history, env_overrides
             FROM panes ORDER BY id",
            |row| {
                let state: String = row.get(5)?;
                let command: String = row.get(4)?;
                Ok(SavedPane {
                    id: PaneId::from_raw(row.get(0)?),
                    workspace: row.get(1)?,
                    cwd: row.get(2)?,
                    spawn_worktree: row.get(3)?,
                    env: serde_json::from_str(&row.get::<_, String>(12)?).unwrap_or_default(),
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
                    agent: None,
                })
            },
        )?;
        // A daemon that died between two sessions in one pane can leave an
        // older one open; only the last one can still run.
        self.db
            .execute(
                "UPDATE agent_sessions SET ended_at = unixepoch()
                 WHERE ended_at IS NULL
                   AND id NOT IN (SELECT max(id) FROM agent_sessions
                                  WHERE ended_at IS NULL GROUP BY pane)",
                [],
            )
            .context("end stale agent sessions")?;
        let agents = self.rows(
            "SELECT pane, id, kind, reference, state, source, observed_at, pid
             FROM agent_sessions WHERE ended_at IS NULL",
            |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    SavedAgent {
                        row: row.get(1)?,
                        kind: row.get(2)?,
                        reference: row.get(3)?,
                        state: AgentState::from_name(&row.get::<_, String>(4)?)
                            .unwrap_or(AgentState::Unknown),
                        source: Source::from_name(&row.get::<_, String>(5)?)
                            .unwrap_or(Source::Process),
                        observed_ms: row.get(6)?,
                        pid: row.get(7)?,
                    },
                ))
            },
        )?;
        let mut panes = panes;
        for (pane, agent) in agents {
            if let Some(saved) = panes.iter_mut().find(|saved| saved.id.raw() == pane) {
                saved.agent = Some(agent);
            }
        }

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

    /// Whether a file of a project is trusted with exactly this content.
    pub(crate) fn trusted(&self, project: u64, path: &str, hash: &str) -> Result<bool> {
        self.db
            .query_row(
                "SELECT 1 FROM trust WHERE project = ?1 AND path = ?2 AND content_hash = ?3",
                params![project, path, hash],
                |_| Ok(()),
            )
            .optional()
            .map(|found| found.is_some())
            .context("read the trust store")
    }

    pub(crate) fn trust(&self, project: u64, path: &str, hash: &str) -> Result<()> {
        self.db
            .execute(
                "INSERT INTO trust (project, path, content_hash, granted_at)
                 VALUES (?1, ?2, ?3, unixepoch())
                 ON CONFLICT DO NOTHING",
                params![project, path, hash],
            )
            .context("record trust")?;
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
                                    process_state, holder_major, cols, rows, history,
                                    env_overrides)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7, ?8, ?9, ?10)",
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
                    serde_json::to_string(&pane.env).context("write the variables")?,
                ],
            )
            .context("reserve a pane")?;
        Ok(PaneId::from_raw(self.db.last_insert_rowid() as u64))
    }

    pub(crate) fn notes(&self, worktree: u64) -> Result<Vec<SavedNote>> {
        self.rows(
            &format!(
                "SELECT id, path, blob, line_start, line_end, body, state FROM review_notes
                 WHERE worktree = {worktree} ORDER BY id"
            ),
            |row| {
                Ok(SavedNote {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    blob: row.get(2)?,
                    lines: (row.get(3)?, row.get(4)?),
                    body: row.get(5)?,
                    state: match row.get::<_, String>(6)?.as_str() {
                        "sent" => NoteState::Sent,
                        "outdated" => NoteState::Outdated,
                        _ => NoteState::Open,
                    },
                })
            },
        )
    }

    pub(crate) fn add_note(&self, worktree: u64, note: &SavedNote) -> Result<()> {
        self.db
            .execute(
                "INSERT INTO review_notes (worktree, path, blob, line_start, line_end, body, state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'open')",
                params![
                    worktree,
                    note.path,
                    note.blob,
                    note.lines.0,
                    note.lines.1,
                    note.body
                ],
            )
            .context("add a review note")?;
        Ok(())
    }

    /// Moves a note to new lines of a new blob, or marks it outdated.
    pub(crate) fn move_note(&self, id: u64, moved: Option<(&str, (u32, u32))>) -> Result<()> {
        match moved {
            Some((blob, (start, end))) => self.db.execute(
                "UPDATE review_notes SET blob = ?2, line_start = ?3, line_end = ?4 WHERE id = ?1",
                params![id, blob, start, end],
            ),
            None => self.db.execute(
                "UPDATE review_notes SET state = 'outdated' WHERE id = ?1",
                params![id],
            ),
        }
        .context("move a review note")?;
        Ok(())
    }

    pub(crate) fn notes_sent(&self, ids: &[u64]) -> Result<()> {
        for id in ids {
            self.db
                .execute(
                    "UPDATE review_notes SET state = 'sent', sent_at = unixepoch()
                     WHERE id = ?1 AND state = 'open'",
                    params![id],
                )
                .context("mark a review note sent")?;
        }
        Ok(())
    }

    /// The inbox items not dismissed, oldest first.
    /// Records a check run as it starts.
    pub(crate) fn start_check(&self, run: &CheckRun, timeout_ms: Option<u64>) -> Result<u64> {
        self.db
            .execute(
                "INSERT INTO check_runs
                     (worktree, name, command, pane, key_before, timeout_ms, started_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    run.worktree,
                    run.name,
                    run.command,
                    run.pane,
                    run.key_before,
                    timeout_ms,
                    run.started_ms
                ],
            )
            .context("record a check run")?;
        Ok(self.db.last_insert_rowid() as u64)
    }

    pub(crate) fn end_check(&self, id: u64, run: &CheckRun) -> Result<()> {
        self.db
            .execute(
                "UPDATE check_runs SET key_after = ?2, exit = ?3, result = ?4,
                     output_tail = ?5, ended_at = ?6
                 WHERE id = ?1",
                params![
                    id,
                    run.key_after,
                    run.exit,
                    run.outcome.map(Outcome::name),
                    run.tail,
                    run.ended_ms
                ],
            )
            .context("record a check result")?;
        Ok(())
    }

    /// The last run of each check of a worktree.
    pub(crate) fn last_checks(&self, worktree: u64) -> Result<Vec<CheckRun>> {
        Ok(self
            .check_runs(
                "WHERE id IN (SELECT MAX(id) FROM check_runs WHERE worktree = ?1 GROUP BY name)",
                params![worktree],
            )?
            .into_iter()
            .map(|(_, run, _)| run)
            .collect())
    }

    /// The worktree and check whose last run `pane` ran.
    pub(crate) fn check_of_pane(&self, pane: PaneId) -> Result<Option<(u64, String)>> {
        self.db
            .query_row(
                "SELECT worktree, name FROM check_runs WHERE pane = ?1 ORDER BY id DESC LIMIT 1",
                params![pane.raw()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("find a pane's check")
    }

    /// Runs with no result yet, with their row and timeout.
    pub(crate) fn running_checks(&self) -> Result<Vec<(u64, CheckRun, Option<u64>)>> {
        self.check_runs("WHERE result IS NULL", params![])
    }

    fn check_runs(
        &self,
        filter: &str,
        values: impl rusqlite::Params,
    ) -> Result<Vec<(u64, CheckRun, Option<u64>)>> {
        let sql = format!(
            "SELECT id, worktree, name, command, pane, key_before, key_after, exit, result,
                 output_tail, started_at, ended_at, timeout_ms
             FROM check_runs {filter} ORDER BY id"
        );
        let mut statement = self
            .db
            .prepare(&sql)
            .with_context(|| format!("read: {sql}"))?;
        statement
            .query_map(values, |row| {
                Ok((
                    row.get(0)?,
                    CheckRun {
                        worktree: row.get(1)?,
                        name: row.get(2)?,
                        command: row.get(3)?,
                        pane: row.get(4)?,
                        key_before: row.get(5)?,
                        key_after: row.get(6)?,
                        exit: row.get(7)?,
                        outcome: row
                            .get::<_, Option<String>>(8)?
                            .and_then(|name| Outcome::from_name(&name)),
                        tail: row.get(9)?,
                        started_ms: row.get(10)?,
                        ended_ms: row.get(11)?,
                        stale: false,
                    },
                    row.get(12)?,
                ))
            })
            .with_context(|| format!("read: {sql}"))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .with_context(|| format!("read: {sql}"))
    }

    pub(crate) fn inbox(&self) -> Result<Vec<SavedItem>> {
        self.rows(
            "SELECT id, pane, kind, created_at, read_at IS NOT NULL FROM inbox_items
             WHERE dismissed_at IS NULL ORDER BY id",
            |row| {
                Ok(SavedItem {
                    id: row.get(0)?,
                    pane: PaneId::from_raw(row.get(1)?),
                    reason: Reason::from_name(&row.get::<_, String>(2)?)
                        .unwrap_or(Reason::TurnFinished),
                    created_ms: row.get(3)?,
                    read: row.get(4)?,
                })
            },
        )
    }

    /// Writes an item: a new row, or `id`'s row with a new reason, unread.
    pub(crate) fn raise_item(
        &self,
        id: Option<u64>,
        pane: PaneId,
        session: Option<i64>,
        reason: Reason,
        created_ms: u64,
    ) -> Result<u64> {
        match id {
            Some(id) => {
                self.db
                    .execute(
                        "UPDATE inbox_items SET kind = ?2, created_at = ?3, read_at = NULL,
                             agent_session = ?4
                         WHERE id = ?1",
                        params![id, reason.name(), created_ms, session],
                    )
                    .context("update an inbox item")?;
                Ok(id)
            }
            None => {
                self.db
                    .execute(
                        "INSERT INTO inbox_items (pane, agent_session, kind, created_at)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![pane.raw(), session, reason.name(), created_ms],
                    )
                    .context("add an inbox item")?;
                Ok(self.db.last_insert_rowid() as u64)
            }
        }
    }

    pub(crate) fn read_item(&self, id: u64, read: bool) -> Result<()> {
        self.db
            .execute(
                "UPDATE inbox_items SET read_at = CASE WHEN ?2 THEN unixepoch() END
                 WHERE id = ?1",
                params![id, read],
            )
            .context("mark an inbox item")?;
        Ok(())
    }

    pub(crate) fn dismiss_item(&self, id: u64) -> Result<()> {
        self.db
            .execute(
                "UPDATE inbox_items SET dismissed_at = unixepoch() WHERE id = ?1",
                params![id],
            )
            .context("dismiss an inbox item")?;
        Ok(())
    }

    /// Starts an agent session row for a pane.
    pub(crate) fn start_agent(&self, pane: PaneId, kind: &str) -> Result<i64> {
        self.db
            .execute(
                "INSERT INTO agent_sessions (pane, kind, started_at, state, source, observed_at)
                 VALUES (?1, ?2, unixepoch(), 'unknown', 'process', unixepoch() * 1000)",
                params![pane.raw(), kind],
            )
            .context("start an agent session")?;
        Ok(self.db.last_insert_rowid())
    }

    /// Ends an agent session whose pane lives on.
    pub(crate) fn end_agent(&self, row: i64) -> Result<()> {
        self.db
            .execute(
                "UPDATE agent_sessions SET ended_at = unixepoch() WHERE id = ?1",
                params![row],
            )
            .context("end an agent session")?;
        Ok(())
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
            if let Some(agent) = &pane.agent {
                transaction
                    .execute(
                        "UPDATE agent_sessions SET reference = ?2, state = ?3, source = ?4,
                             observed_at = ?5, pid = ?6
                         WHERE id = ?1",
                        params![
                            agent.row,
                            agent.reference,
                            agent.state.name(),
                            agent.source.name(),
                            agent.observed_ms,
                            agent.pid,
                        ],
                    )
                    .context("save an agent session")?;
            }
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
