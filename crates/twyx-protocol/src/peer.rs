//! Who may talk to whom on a twyx socket: both ends run as one user.

use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Refuses a peer that runs as another user.
///
/// The OS user is the boundary: every process of this user has full rights on
/// the socket, agents in panes included.
pub fn accept(stream: &UnixStream) -> Result<()> {
    let peer = peer_uid(stream)?;
    let ours = rustix::process::geteuid().as_raw();
    if peer != ours {
        bail!("refusing a peer that runs as uid {peer}, not {ours}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> Result<u32> {
    let credentials = rustix::net::sockopt::socket_peercred(stream)
        .with_context(|| "read the peer credentials")?;
    Ok(credentials.uid.as_raw())
}

#[cfg(not(target_os = "linux"))]
fn peer_uid(stream: &UnixStream) -> Result<u32> {
    let (uid, _) = nix::unistd::getpeereid(stream).with_context(|| "read the peer credentials")?;
    Ok(uid.as_raw())
}

/// The process at the other end of a socket, if the OS says.
#[cfg(target_os = "linux")]
pub fn pid(stream: &UnixStream) -> Option<u32> {
    rustix::net::sockopt::socket_peercred(stream)
        .ok()
        .map(|credentials| credentials.pid.as_raw_nonzero().get() as u32)
}

/// The process at the other end of a socket, if the OS says.
#[cfg(not(target_os = "linux"))]
pub fn pid(stream: &UnixStream) -> Option<u32> {
    nix::sys::socket::getsockopt(stream, nix::sys::socket::sockopt::LocalPeerPid)
        .ok()
        .and_then(|pid| u32::try_from(pid).ok())
}

/// Connects to a twyx socket only when its directory is this user's alone
/// and the process listening runs as this user. Another user can create
/// `/tmp/twyx-<uid>` first and listen there.
pub fn dial(socket: &Path) -> Result<UnixStream> {
    let dir = socket.parent().unwrap_or(Path::new("/"));
    let metadata =
        std::fs::symlink_metadata(dir).with_context(|| format!("read {}", dir.display()))?;
    let ours = rustix::process::geteuid().as_raw();
    if !metadata.is_dir() || metadata.uid() != ours || metadata.mode() & 0o077 != 0 {
        bail!(
            "{} must be a directory of uid {ours} with mode 0700; it is not, so twyx will not use the socket in it",
            dir.display()
        );
    }
    let stream =
        UnixStream::connect(socket).with_context(|| format!("connect to {}", socket.display()))?;
    accept(&stream)?;
    Ok(stream)
}
