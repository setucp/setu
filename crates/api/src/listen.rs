//! Where `setu serve` listens (spec §8): a unix socket behind Caddy
//! (`listen = "unix:/run/setu/http.sock"`) or TCP (`127.0.0.1:7000`).
//!
//! The unix socket is created mode `0660` (the umask is narrowed around
//! `bind`, so it never exists with wider permissions) and handed to group
//! `caddy` when that group exists, else it keeps the process group. A
//! stale socket left by a previous process is removed first; any other
//! file at the path is refused. Each connection records its [`Peer`] via
//! axum's `connect_info`, which [`crate::auth::client_ip`] reads.

use std::io;
use std::net::IpAddr;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};

use axum::extract::connect_info::Connected;
use axum::serve::IncomingStream;
use axum::Router;
use setu_common::Listen;
use tokio::net::{TcpListener, UnixListener};

/// Socket mode: owner and group (Caddy) read/write, nothing for others.
pub const SOCKET_MODE: u32 = 0o660;
/// The group that may connect to the unix socket, when it exists.
pub const SOCKET_GROUP: &str = "caddy";

/// The other end of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    /// The unix socket: Caddy by construction.
    Unix,
    /// A TCP client (or proxy) at this address.
    Tcp(IpAddr),
}

impl Connected<IncomingStream<'_, TcpListener>> for Peer {
    fn connect_info(stream: IncomingStream<'_, TcpListener>) -> Self {
        Peer::Tcp(stream.remote_addr().ip())
    }
}

impl Connected<IncomingStream<'_, UnixListener>> for Peer {
    fn connect_info(_stream: IncomingStream<'_, UnixListener>) -> Self {
        Peer::Unix
    }
}

/// A bound listener.
#[derive(Debug)]
pub enum Bound {
    Tcp(TcpListener),
    Unix(UnixListener, PathBuf),
}

impl Bound {
    /// Human-readable address for the boot log.
    pub fn describe(&self) -> String {
        match self {
            Bound::Tcp(l) => l
                .local_addr()
                .map(|a| a.to_string())
                .unwrap_or_else(|_| "tcp".to_string()),
            Bound::Unix(_, path) => format!("unix:{}", path.display()),
        }
    }
}

/// Bind `listen` (see the module docs for the unix socket rules).
pub async fn bind(listen: &Listen) -> io::Result<Bound> {
    match listen {
        Listen::Tcp(addr) => Ok(Bound::Tcp(TcpListener::bind(addr).await?)),
        Listen::Unix(path) => bind_unix(path).map(|l| Bound::Unix(l, path.clone())),
    }
}

fn bind_unix(path: &Path) -> io::Result<UnixListener> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    // Narrow the umask for the bind only, so the socket is created
    // 0660 rather than chmod-ed after a window with wider permissions.
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o117));
    let bound = UnixListener::bind(path);
    nix::sys::stat::umask(previous);
    let listener = bound?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCKET_MODE))?;
    match nix::unistd::Group::from_name(SOCKET_GROUP) {
        Ok(Some(group)) => {
            if let Err(e) = nix::unistd::chown(path, None, Some(group.gid)) {
                tracing::warn!(
                    "cannot give {} to group {SOCKET_GROUP} ({e}); Caddy may be unable to connect",
                    path.display()
                );
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("group lookup for {SOCKET_GROUP} failed: {e}"),
    }
    Ok(listener)
}

/// Serve `app` on the bound listener until the process ends.
pub async fn serve(bound: Bound, app: Router) -> io::Result<()> {
    match bound {
        Bound::Tcp(listener) => {
            axum::serve(listener, app.into_make_service_with_connect_info::<Peer>()).await
        }
        Bound::Unix(listener, _) => {
            axum::serve(listener, app.into_make_service_with_connect_info::<Peer>()).await
        }
    }
}
