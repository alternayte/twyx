//! The pane's PTY.
//!
//! The child runs in a new session, and the PTY is its controlling terminal.
//! Nothing in twyx talks to a PTY outside this module.

use std::io::{Read, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

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
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl std::fmt::Debug for Pty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pty")
            .field("tty", &self.master.tty_name())
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

        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("spawn {program}"))?;
        // Dropping the slave end is what lets the master see EOF after exit.
        drop(pair.slave);

        Ok(Self {
            master: pair.master,
            child,
        })
    }

    pub fn reader(&self) -> Result<Box<dyn Read + Send>> {
        self.master
            .try_clone_reader()
            .context("clone the pty reader")
    }

    pub fn writer(&self) -> Result<Box<dyn Write + Send>> {
        self.master.take_writer().context("take the pty writer")
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .resize(size(cols, rows))
            .context("resize the pty")
    }

    /// Blocks until the child exits.
    pub fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().context("wait for the pane child")
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child.try_wait().context("poll the pane child")
    }

    pub fn kill(&mut self) -> Result<()> {
        self.child.kill().context("kill the pane child")
    }

    pub fn child_pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    pub fn tty_name(&self) -> Option<PathBuf> {
        self.master.tty_name()
    }

    /// The local process group or session leader of the child.
    pub fn session_leader(&self) -> Option<i32> {
        self.master.process_group_leader()
    }
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}
