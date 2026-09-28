//! A daemon's hold on its root: one daemon per root, and the socket the CLI
//! reaches it on.

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use std::fs::File;
use std::path::{Path, PathBuf};

/// Room for a socket path, NUL excluded: `sun_path` is 104 bytes on macOS
/// and 108 on Linux, and a root must work on both.
const SOCKET_PATH_LIMIT: usize = 103;

#[must_use]
pub fn socket_path(root: &Path) -> PathBuf {
    root.join("daemon.sock")
}

fn lock_path(root: &Path) -> PathBuf {
    root.join("daemon.lock")
}

/// This daemon's exclusive hold on its root, released when dropped — or by
/// the kernel, when the process dies without dropping it.
#[derive(Debug)]
pub struct RootLock {
    _held: Flock<File>,
}

/// Why a daemon cannot have the root it was given.
#[derive(Debug)]
pub enum RootUnavailable {
    Taken {
        root: PathBuf,
    },
    SocketPathTooLong {
        path: PathBuf,
        bytes: usize,
        limit: usize,
    },
    /// `path` is the root itself, its lock file, or a dead daemon's socket.
    Unpreparable {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for RootUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Taken { root } => write!(
                f,
                "another daemon holds {} — stop it, or give this one its own --root",
                root.display()
            ),
            Self::SocketPathTooLong { path, bytes, limit } => write!(
                f,
                "the socket path {} is {bytes} bytes, past the {limit} a Unix socket allows — \
                 use a shorter --root",
                path.display()
            ),
            Self::Unpreparable { path, source } => write!(
                f,
                "cannot prepare {}: {source} — the root must be a directory you can write; fix \
                 it, or give the daemon another --root",
                path.display()
            ),
        }
    }
}

/// Take `root` for this daemon: check its socket path fits, create it, lock
/// it, and clear a dead daemon's socket.
///
/// # Errors
///
/// When the socket path is too long (checked before anything is created),
/// another live daemon holds the root, or the root, its lock or a dead
/// daemon's socket cannot be prepared.
pub fn hold_root(root: &Path) -> Result<RootLock, RootUnavailable> {
    let socket = socket_path(root);
    let bytes = socket.as_os_str().len();
    if bytes > SOCKET_PATH_LIMIT {
        return Err(RootUnavailable::SocketPathTooLong {
            path: socket,
            bytes,
            limit: SOCKET_PATH_LIMIT,
        });
    }
    let unpreparable = |path: &Path| {
        let path = path.to_path_buf();
        move |source| RootUnavailable::Unpreparable { path, source }
    };
    std::fs::create_dir_all(root).map_err(unpreparable(root))?;
    let lock = lock_path(root);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock)
        .map_err(unpreparable(&lock))?;
    let held = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(held) => held,
        Err((_, Errno::EWOULDBLOCK)) => {
            return Err(RootUnavailable::Taken {
                root: root.to_path_buf(),
            });
        }
        Err((_, errno)) => return Err(unpreparable(&lock)(errno.into())),
    };
    // The lock is ours, so a socket still here is a dead daemon's.
    match std::fs::remove_file(&socket) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(unpreparable(&socket)(e)),
        _ => Ok(RootLock { _held: held }),
    }
}
