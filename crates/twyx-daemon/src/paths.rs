//! The paths twyx uses, and the rules the runtime directory must keep.

use std::ffi::OsString;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use twyx_core::pane::PaneId;

/// The same layout on macOS and Linux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub runtime: PathBuf,
    pub state: PathBuf,
    pub config: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        let uid = rustix::process::geteuid().as_raw();
        let runtime = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("twyx"),
            _ => PathBuf::from(format!("/tmp/twyx-{uid}")),
        };
        Ok(Self {
            runtime,
            state: xdg("XDG_STATE_HOME", ".local/state")?.join("twyx"),
            config: xdg("XDG_CONFIG_HOME", ".config")?.join("twyx"),
        })
    }

    /// Creates the runtime directory with mode 0700 and refuses one that
    /// another user owns or that this user left open.
    pub fn prepare_runtime(&self) -> Result<()> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(&self.runtime)
            .with_context(|| format!("create {}", self.runtime.display()))?;
        let metadata = std::fs::symlink_metadata(&self.runtime)
            .with_context(|| format!("read {}", self.runtime.display()))?;
        let uid = rustix::process::geteuid().as_raw();
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != uid {
            bail!(
                "{} belongs to uid {}, not {uid}",
                self.runtime.display(),
                metadata.uid()
            );
        }
        let mode = metadata.permissions().mode() & 0o777;
        if mode != 0o700 {
            bail!(
                "{} has mode {mode:o}, and twyx needs 0700",
                self.runtime.display()
            );
        }
        Ok(())
    }

    pub fn socket(&self) -> PathBuf {
        self.runtime.join("twyx.sock")
    }

    pub fn pane_socket(&self, pane: PaneId) -> PathBuf {
        self.runtime.join("panes").join(format!("{pane}.sock"))
    }

    pub fn panes_dir(&self) -> PathBuf {
        self.runtime.join("panes")
    }
}

fn xdg(variable: &str, fallback: &str) -> Result<PathBuf> {
    match std::env::var_os(variable) {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir)),
        _ => {
            let home: OsString = std::env::var_os("HOME").context("HOME is not set")?;
            Ok(Path::new(&home).join(fallback))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "twyx-paths-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    fn paths(runtime: PathBuf) -> Paths {
        let state = runtime.join("state");
        let config = runtime.join("config");
        Paths {
            runtime,
            state,
            config,
        }
    }

    #[test]
    fn invariant_16_a_new_runtime_directory_has_mode_0700_and_this_owner() {
        let root = scratch("new");
        let paths = paths(root.join("twyx"));
        paths
            .prepare_runtime()
            .expect("prepare the runtime directory");
        let metadata = std::fs::metadata(&paths.runtime).expect("read it back");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invariant_16_an_open_runtime_directory_is_refused() {
        let root = scratch("open");
        let paths = paths(root.join("twyx"));
        std::fs::create_dir_all(&paths.runtime).expect("create the directory");
        std::fs::set_permissions(&paths.runtime, std::fs::Permissions::from_mode(0o755))
            .expect("open it up");
        let error = paths.prepare_runtime().expect_err("refuse the directory");
        assert!(error.to_string().contains("0700"), "{error}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pane_socket_stays_inside_the_socket_path_limit() {
        let paths = paths(PathBuf::from("/tmp/twyx-501"));
        let socket = paths.pane_socket(PaneId::from_raw(u64::MAX));
        assert!(socket.as_os_str().len() < 104, "{}", socket.display());
    }
}
