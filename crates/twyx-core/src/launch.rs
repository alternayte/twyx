//! How the daemon and the holders start.
//!
//! On Linux with a systemd user manager, the daemon runs as a transient
//! user unit and each holder in a transient scope of its own, so stopping
//! the daemon's unit or ending an SSH session does not end a pane. Elsewhere
//! both start as plain processes in their own sessions.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// The environment the daemon keeps: a systemd unit starts with almost none.
const KEPT: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "LANG",
    "LC_ALL",
    "XDG_RUNTIME_DIR",
    "XDG_STATE_HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "TWYX_BINARY",
];

/// The command that starts the daemon from `binary`.
pub fn daemon(binary: &Path) -> Command {
    match systemd_run() {
        Some(systemd_run) => {
            let mut command = Command::new(systemd_run);
            command.args(["--user", "--collect", "--quiet", "--unit=twyx-daemon"]);
            for name in KEPT {
                if let Some(value) = std::env::var_os(name) {
                    let mut setting = std::ffi::OsString::from(format!("--setenv={name}="));
                    setting.push(value);
                    command.arg(setting);
                }
            }
            command.arg("--").arg(binary).args(["daemon", "run"]);
            command
        }
        None => {
            let mut command = Command::new(binary);
            command.args(["daemon", "run"]);
            command
        }
    }
}

/// The command that starts a holder for `pane` from `binary`; the caller
/// adds the holder's arguments.
pub fn holder(binary: &Path, pane: &str) -> Command {
    match systemd_run() {
        Some(systemd_run) => {
            let mut command = Command::new(systemd_run);
            // A scope keeps the holder as this process tree's child but out
            // of the daemon's unit.
            let unit = format!("--unit=twyx-pane-{pane}-{}", stamp());
            command.args(["--user", "--scope", "--collect", "--quiet"]);
            command.arg(unit).arg("--").arg(binary).arg("holder");
            command
        }
        None => {
            let mut command = Command::new(binary);
            command.arg("holder");
            command
        }
    }
}

/// `systemd-run`, when a systemd user manager runs for this user.
fn systemd_run() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    if !Path::new(&runtime).join("systemd/private").exists() {
        return None;
    }
    find(OsStr::new("systemd-run"))
}

fn find(program: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
}

fn stamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos())
}
