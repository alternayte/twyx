//! Who may talk to the daemon.

use std::os::unix::net::UnixStream;

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
