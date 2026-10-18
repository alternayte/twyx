//! The pane's PTY.
//!
//! The child runs in a new session, and the PTY is its controlling terminal.
//! Nothing in twyx talks to a PTY outside this module.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// What to start in a pane.
#[derive(Debug, Clone)]
pub struct Spawn {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

/// One PTY and its child process.
pub struct Pty {
    master: Master,
    child: Box<dyn Child + Send + Sync>,
}

impl std::fmt::Debug for Pty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pty")
            .field("tty", &self.master.tty_name)
            .field("pid", &self.child.process_id())
            .finish()
    }
}

impl Pty {
    pub fn spawn(spec: &Spawn) -> Result<Self> {
        let (program, args) = spec.argv.split_first().context("pane command is empty")?;
        let pair = native_pty_system()
            .openpty(size(spec.cols, spec.rows))
            .context("open a pty")?;

        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.cwd(&spec.cwd);
        for (key, value) in &spec.env {
            command.env(key, value);
        }

        let master = Master {
            tty_name: pair.master.tty_name(),
            session_leader: pair.master.process_group_leader(),
            inner: pair.master,
        };

        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("spawn {program}"))?;
        // Dropping the slave end is what lets the master see EOF after exit.
        drop(pair.slave);

        Ok(Self { master, child })
    }

    /// Splits the handle so one thread can wait for the child while another
    /// reads and writes the terminal.
    pub fn split(self) -> (Master, Box<dyn Child + Send + Sync>) {
        (self.master, self.child)
    }
}

/// The terminal end of a pane.
pub struct Master {
    inner: Box<dyn MasterPty + Send>,
    tty_name: Option<PathBuf>,
    session_leader: Option<i32>,
}

impl std::fmt::Debug for Master {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Master")
            .field("tty", &self.tty_name)
            .field("session_leader", &self.session_leader)
            .finish()
    }
}

impl Master {
    pub fn reader(&self) -> Result<Box<dyn Read + Send>> {
        self.inner
            .try_clone_reader()
            .context("clone the pty reader")
    }

    pub fn writer(&self) -> Result<Box<dyn Write + Send>> {
        self.inner.take_writer().context("take the pty writer")
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.inner
            .resize(size(cols, rows))
            .context("resize the pty")
    }

    pub fn tty_name(&self) -> Option<&std::path::Path> {
        self.tty_name.as_deref()
    }

    pub fn session_leader(&self) -> Option<i32> {
        self.session_leader
    }
}

/// A handle that can end the pane's process tree from another thread.
pub fn killer(child: &(dyn Child + Send + Sync)) -> Box<dyn ChildKiller + Send + Sync> {
    child.clone_killer()
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}
