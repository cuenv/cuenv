//! Runtime roots that may be exposed to strict sandbox tasks on Linux.

use super::MountIdentity;
use sha2::{Digest as _, Sha256};
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Explicit read-only locations exposed inside the sandbox.
pub(super) const ROOTS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/etc/alternatives",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/ssl",
    "/etc/ca-certificates",
    "/nix/store",
];

const RUNTIME_ROOT_ENV: &str = "CUENV_SANDBOX_RUNTIME_ROOT";
static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

pub(super) enum Runtime {
    /// An unfingerprinted host toolchain may only reuse results within this run.
    Host { invocation: String },
    /// The mount is intrinsically read-only, with no mutable nested mounts.
    Image {
        mount_path: PathBuf,
        verification: Verification,
        _anchor: File,
    },
}

pub(super) struct Verification {
    digest: String,
    roots: Vec<VerifiedRoot>,
}

struct VerifiedRoot {
    pathname: &'static str,
    entry: VerifiedEntry,
}

enum VerifiedEntry {
    Link(PathBuf),
    Bind {
        kind: MountKind,
        identity: MountIdentity,
    },
}

#[derive(Clone, Copy)]
pub(super) enum MountKind {
    Directory,
    File,
}

impl From<&fs::Metadata> for MountKind {
    fn from(metadata: &fs::Metadata) -> Self {
        if metadata.is_dir() {
            Self::Directory
        } else {
            Self::File
        }
    }
}

pub(super) enum RuntimeSource<'runtime> {
    Link(&'runtime Path),
    Bind {
        path: PathBuf,
        kind: MountKind,
        identity: Option<MountIdentity>,
    },
}

impl Runtime {
    pub(super) fn resolve() -> Result<&'static Self, String> {
        RUNTIME
            .get_or_init(|| Self::load().map_err(|error| format!("{RUNTIME_ROOT_ENV}: {error}")))
            .as_ref()
            .map_err(Clone::clone)
    }

    fn load() -> io::Result<Self> {
        let Some(configured) = std::env::var_os(RUNTIME_ROOT_ENV) else {
            let mut random = [0_u8; 32];
            File::open("/dev/urandom")?.read_exact(&mut random)?;
            return Ok(Self::Host {
                invocation: hex::encode(random),
            });
        };
        let path = Path::new(&configured);
        if !path.is_absolute() {
            return Err(io::Error::other(
                "the runtime root must be an absolute path",
            ));
        }
        let mount_path = path.canonicalize()?;
        let anchor = File::open(&mount_path)?;
        if !anchor.metadata()?.is_dir() {
            return Err(io::Error::other("the runtime root must be a directory"));
        }
        verify_image_fs(&anchor)?;
        let root = PathBuf::from(format!("/proc/self/fd/{}", anchor.as_raw_fd()));
        let verification = verify_roots(&root)?;
        Ok(Self::Image {
            mount_path,
            verification,
            _anchor: anchor,
        })
    }

    pub(super) fn source(&self, pathname: &'static str) -> io::Result<Option<RuntimeSource<'_>>> {
        match self {
            Self::Host { .. } => {
                let path = PathBuf::from(pathname);
                match path.metadata() {
                    Ok(metadata) => Ok(Some(RuntimeSource::Bind {
                        path,
                        kind: MountKind::from(&metadata),
                        identity: None,
                    })),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(error),
                }
            }
            Self::Image {
                mount_path,
                verification,
                ..
            } => Ok(verification
                .roots
                .iter()
                .find(|root| root.pathname == pathname)
                .map(|root| match &root.entry {
                    VerifiedEntry::Link(link) => RuntimeSource::Link(link),
                    VerifiedEntry::Bind { kind, identity } => RuntimeSource::Bind {
                        path: mount_path.join(pathname.trim_start_matches('/')),
                        kind: *kind,
                        identity: Some(*identity),
                    },
                })),
        }
    }

    pub(super) fn identity(&self) -> (&'static str, &str) {
        match self {
            Self::Host { invocation } => ("host.invocation", invocation),
            Self::Image { verification, .. } => ("image.digest", &verification.digest),
        }
    }

    pub(super) fn is_image(&self) -> bool {
        matches!(self, Self::Image { .. })
    }
}

#[expect(
    unsafe_code,
    reason = "Required to identify an intrinsically read-only filesystem"
)]
fn verify_image_fs(root: &File) -> io::Result<()> {
    // Linux magic values for intrinsically read-only image filesystems.
    const SQUASHFS_MAGIC: u32 = 0x7371_7368;
    const EROFS_MAGIC: u32 = 0xe0f5_e1e2;

    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: The descriptor is live and fstatfs initializes stat on success.
    let filesystem_type = unsafe {
        if libc::fstatfs(root.as_raw_fd(), stat.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        stat.assume_init().f_type
    };
    let filesystem_type = u32::try_from(filesystem_type)
        .or_else(|_| i32::try_from(filesystem_type).map(i32::cast_unsigned))
        .map_err(io::Error::other)?;
    if !matches!(filesystem_type, SQUASHFS_MAGIC | EROFS_MAGIC) {
        return Err(io::Error::other(
            "the runtime root must be on an intrinsically read-only SquashFS or EROFS filesystem",
        ));
    }
    Ok(())
}

fn verify_roots(root: &Path) -> io::Result<Verification> {
    // Appending "." ensures that statx sees the anchored directory, not
    // the procfs symlink to its open file descriptor.
    let image_mount = mount_id(&root.join("."))?;
    let mut digest = Sha256::new();
    let mut roots = Vec::new();
    digest.update(b"cuenv.linux.runtime.v1\0");
    for &pathname in ROOTS {
        hash_field(&mut digest, pathname.as_bytes());
        let source = root.join(pathname.trim_start_matches('/'));
        if !image_root_present(root, &source)? {
            digest.update(b"missing\0");
            continue;
        }
        for entry in walkdir::WalkDir::new(&source)
            .follow_root_links(false)
            .sort_by_file_name()
        {
            let entry = entry?;
            if mount_id(entry.path())? != image_mount {
                return Err(io::Error::other(format!(
                    "nested mount beneath the runtime image: {}",
                    entry.path().display()
                )));
            }
            let verified = hash_entry(&mut digest, root, &entry)?;
            if entry.depth() == 0 {
                roots.push(VerifiedRoot {
                    pathname,
                    entry: verified,
                });
            }
        }
        digest.update(b"end\0");
    }
    Ok(Verification {
        digest: hex::encode(digest.finalize()),
        roots,
    })
}

fn image_root_present(root: &Path, source: &Path) -> io::Result<bool> {
    // Symlinks within a mounted tree are resolved after pivot_root. Reject a
    // symlink in an allowlisted root's parent path because mounting its child
    // on the host would instead resolve the link outside the image.
    let mut parents = source
        .ancestors()
        .skip(1)
        .take_while(|parent| *parent != root);
    for parent in &mut parents {
        match fs::symlink_metadata(parent) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "runtime mount path has a non-directory ancestor: {}",
                    parent.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    match fs::symlink_metadata(source) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn hash_entry(
    digest: &mut Sha256,
    root: &Path,
    entry: &walkdir::DirEntry,
) -> io::Result<VerifiedEntry> {
    let path = entry.path();
    let relative = path.strip_prefix(root).map_err(io::Error::other)?;
    let metadata = entry.metadata()?;
    let kind = entry.file_type();
    hash_field(digest, relative.as_os_str().as_bytes());
    digest.update(metadata.mode().to_le_bytes());
    digest.update(metadata.uid().to_le_bytes());
    digest.update(metadata.gid().to_le_bytes());
    digest.update(metadata.mtime().to_le_bytes());
    digest.update(metadata.mtime_nsec().to_le_bytes());
    if kind.is_symlink() {
        let link = fs::read_link(path)?;
        hash_field(digest, link.as_os_str().as_bytes());
        return Ok(VerifiedEntry::Link(link));
    }
    if kind.is_file() {
        digest.update(metadata.len().to_le_bytes());
        let mut file = File::open(path)?;
        let mut buffer = [0_u8; 8192];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
    } else if !kind.is_dir() {
        return Err(io::Error::other(format!(
            "special file in runtime image: {}",
            path.display()
        )));
    }
    Ok(VerifiedEntry::Bind {
        kind: MountKind::from(&metadata),
        identity: MountIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
    })
}

fn hash_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

#[expect(
    unsafe_code,
    reason = "Required to reject nested mounts inside a runtime image"
)]
fn mount_id(path: &Path) -> io::Result<u64> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    let mut statx = std::mem::MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: statx writes only to the supplied buffer. The mask indicates
    // whether the running kernel returned mount identity.
    let statx = unsafe {
        if libc::syscall(
            libc::SYS_statx,
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::AT_NO_AUTOMOUNT | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            statx.as_mut_ptr(),
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
        statx.assume_init()
    };
    if statx.stx_mask & libc::STATX_MNT_ID == 0 {
        return Err(io::Error::other(
            "the kernel did not report runtime mount identity",
        ));
    }
    Ok(statx.stx_mnt_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_digest_changes_when_executable_contents_change() -> io::Result<()> {
        let tree = tempfile::tempdir()?;
        let root = tree.path();
        let bin = root.join("usr/bin");
        fs::create_dir_all(&bin)?;
        std::os::unix::fs::symlink("/usr/bin", root.join("bin"))?;
        let tool = bin.join("tool");
        fs::write(&tool, b"one")?;
        let modified = fs::metadata(&tool)?.modified()?;
        let original = verify_roots(root)?.digest;

        fs::write(&tool, b"two")?;
        File::options()
            .write(true)
            .open(tool)?
            .set_times(fs::FileTimes::new().set_modified(modified))?;
        assert_ne!(original, verify_roots(root)?.digest);
        Ok(())
    }

    #[test]
    fn staged_runtime_sources_come_from_the_hashed_roots() -> io::Result<()> {
        let tree = tempfile::tempdir()?;
        let root = tree.path();
        let usr = root.join("usr");
        let bin = root.join("bin");
        fs::create_dir(&usr)?;
        std::os::unix::fs::symlink("/usr/bin", &bin)?;
        let original = usr.metadata()?;
        let runtime = Runtime::Image {
            mount_path: root.to_path_buf(),
            verification: verify_roots(root)?,
            _anchor: File::open(root)?,
        };

        fs::rename(&usr, root.join("original-usr"))?;
        fs::create_dir(&usr)?;
        fs::remove_file(&bin)?;
        std::os::unix::fs::symlink("/different", &bin)?;

        let Some(RuntimeSource::Bind {
            identity: Some(expected),
            ..
        }) = runtime.source("/usr")?
        else {
            return Err(io::Error::other(
                "the verified directory identity must be retained",
            ));
        };
        assert_eq!(expected.device, original.dev());
        assert_eq!(expected.inode, original.ino());
        assert_ne!(expected.inode, usr.metadata()?.ino());

        let Some(RuntimeSource::Link(link)) = runtime.source("/bin")? else {
            return Err(io::Error::other("the verified link must be retained"));
        };
        assert_eq!(link, Path::new("/usr/bin"));
        Ok(())
    }

    #[test]
    fn rejects_symlink_in_host_path_to_image_root() -> io::Result<()> {
        let tree = tempfile::tempdir()?;
        let root = tree.path();
        std::os::unix::fs::symlink("/etc", root.join("etc"))?;
        let Err(error) = verify_roots(root) else {
            return Err(io::Error::other("image path must stay in the source image"));
        };
        assert!(error.to_string().contains("non-directory ancestor"));
        Ok(())
    }

    #[test]
    fn writable_runtime_tree_cannot_authorize_publication() -> io::Result<()> {
        let tree = tempfile::tempdir()?;
        let root = File::open(tree.path())?;
        assert!(verify_image_fs(&root).is_err());
        Ok(())
    }
}
