//! Authenticated local transport for a single hook-run broker.

#[cfg(not(target_os = "linux"))]
use std::fs::{self, DirBuilder, OpenOptions};
#[cfg(not(target_os = "linux"))]
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use tokio::net::{UnixListener, UnixStream};

use crate::state::HookRunIdentity;
use crate::{Error, Result};

pub(super) type ServerStream = UnixStream;

/// A listener owns one run-specific socket until it is dropped.
pub(super) struct Listener {
    socket: UnixListener,
    #[cfg(not(target_os = "linux"))]
    path: PathBuf,
    #[cfg(not(target_os = "linux"))]
    device: u64,
    #[cfg(not(target_os = "linux"))]
    inode: u64,
}

#[cfg(not(target_os = "linux"))]
fn socket_directory() -> Result<PathBuf> {
    // The fixed short parent avoids platform socket-path length limits even
    // when the configured state directory is long or contains non-UTF-8 bytes.
    let parent = fs::canonicalize("/tmp")
        .map_err(|_| Error::configuration("Hook broker temporary directory is unavailable"))?;
    let parent_metadata = fs::metadata(&parent)
        .map_err(|_| Error::configuration("Cannot inspect hook broker temporary directory"))?;
    let uid = rustix::process::geteuid().as_raw();
    if !parent_metadata.is_dir()
        || (parent_metadata.uid() != 0 && parent_metadata.uid() != uid)
        || (parent_metadata.mode() & 0o022 != 0 && parent_metadata.mode() & 0o1000 == 0)
    {
        return Err(Error::configuration(
            "Hook broker temporary directory is not trusted",
        ));
    }

    let directory = parent.join(format!("cuenv-hook-ipc-{uid}"));
    match DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(Error::configuration("Cannot create hook broker directory")),
    }
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&directory)
        .map_err(|_| Error::configuration("Cannot open hook broker directory"))?;
    let metadata = handle
        .metadata()
        .map_err(|_| Error::configuration("Cannot inspect hook broker directory"))?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(Error::configuration(
            "Hook broker directory must be private and owned by the current user",
        ));
    }
    Ok(directory)
}

#[cfg(not(target_os = "linux"))]
fn socket_path(identity: &HookRunIdentity) -> Result<PathBuf> {
    if identity.run_id.is_nil() {
        return Err(Error::configuration("Hook broker run identity is invalid"));
    }
    Ok(socket_directory()?.join(format!("{}.sock", identity.run_id.simple())))
}

#[cfg(target_os = "linux")]
fn socket_path(identity: &HookRunIdentity) -> Result<PathBuf> {
    if identity.run_id.is_nil() {
        return Err(Error::configuration("Hook broker run identity is invalid"));
    }
    let uid = rustix::process::geteuid().as_raw();
    Ok(PathBuf::from(format!(
        "\0cuenv-hook-ipc-{uid}-{}",
        identity.run_id.simple()
    )))
}

fn authenticate(stream: &UnixStream) -> Result<()> {
    let peer = stream
        .peer_cred()
        .map_err(|_| Error::configuration("Cannot authenticate hook broker peer"))?;
    if peer.uid() != rustix::process::geteuid().as_raw() {
        return Err(Error::configuration(
            "Hook broker peer is not the current user",
        ));
    }
    Ok(())
}

pub(super) fn bind(_state_dir: &Path, identity: &HookRunIdentity) -> Result<Listener> {
    let path = socket_path(identity)?;
    let socket = UnixListener::bind(&path)
        .map_err(|_| Error::configuration("Cannot bind hook broker endpoint"))?;
    #[cfg(target_os = "linux")]
    {
        Ok(Listener { socket })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| Error::configuration("Cannot inspect hook broker endpoint"))?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(Error::configuration("Invalid hook broker endpoint"));
        }
        Ok(Listener {
            socket,
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

impl Listener {
    pub(super) async fn accept(&mut self) -> Result<UnixStream> {
        let (stream, _) = self
            .socket
            .accept()
            .await
            .map_err(|_| Error::configuration("Cannot accept hook broker connection"))?;
        Ok(stream)
    }
}

pub(super) fn authenticate_server(
    stream: &mut UnixStream,
) -> impl std::future::Future<Output = Result<()>> + '_ {
    std::future::ready(authenticate(stream))
}

pub(super) async fn connect(
    _state_dir: &Path,
    identity: &HookRunIdentity,
) -> Result<Option<UnixStream>> {
    let path = socket_path(identity)?;
    #[cfg(not(target_os = "linux"))]
    {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::configuration("Cannot inspect hook broker endpoint")),
        };
        if !metadata.file_type().is_socket()
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(Error::configuration("Invalid hook broker endpoint"));
        }
    }
    let stream = match UnixStream::connect(path).await {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(_) => return Err(Error::configuration("Cannot connect to hook broker")),
    };
    authenticate(&stream)?;
    Ok(Some(stream))
}

pub(super) async fn remove_stale(_state_dir: &Path, identity: &HookRunIdentity) -> Result<()> {
    let path = socket_path(identity)?;
    #[cfg(target_os = "linux")]
    {
        match UnixStream::connect(&path).await {
            Ok(_) => Err(Error::configuration("Hook broker is still running")),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                Ok(())
            }
            Err(_) => Err(Error::configuration(
                "Cannot inspect stale hook broker endpoint",
            )),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => {
                return Err(Error::configuration(
                    "Cannot inspect stale hook broker endpoint",
                ));
            }
        };
        if !metadata.file_type().is_socket()
            || metadata.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(Error::configuration("Invalid stale hook broker endpoint"));
        }
        match UnixStream::connect(&path).await {
            Ok(_) => return Err(Error::configuration("Hook broker is still running")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(_) => {
                return Err(Error::configuration(
                    "Cannot inspect stale hook broker endpoint",
                ));
            }
        }
        let current = fs::symlink_metadata(&path)
            .map_err(|_| Error::configuration("Hook broker endpoint changed during cleanup"))?;
        if !current.file_type().is_socket()
            || current.dev() != metadata.dev()
            || current.ino() != metadata.ino()
        {
            return Err(Error::configuration(
                "Hook broker endpoint changed during cleanup",
            ));
        }
        fs::remove_file(path)
            .map_err(|_| Error::configuration("Cannot remove stale hook broker endpoint"))
    }
}

#[cfg(not(target_os = "linux"))]
impl Drop for Listener {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}
