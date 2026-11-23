//! Runtime roots that may be exposed to strict sandbox tasks on Linux.

use super::super::{NixSandboxContext, is_valid_nix_store_basename};
use super::MountIdentity;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

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
const NIX_STORE_DIR: &str = "/nix/store";

static BASE_RUNTIME: OnceLock<Result<Arc<Runtime>, String>> = OnceLock::new();
static NIX_CLOSURES: OnceLock<Mutex<HashMap<NixClosureCacheKey, Arc<Runtime>>>> = OnceLock::new();

#[derive(Clone, PartialEq, Eq, Hash)]
struct NixClosureCacheKey {
    lock_digest: String,
    store_roots: Vec<PathBuf>,
    path_entries: Vec<PathBuf>,
}

pub(super) enum Runtime {
    /// An unfingerprinted host toolchain may only reuse results within this run.
    Host { invocation: String },
    /// The mount is intrinsically read-only, with no mutable nested mounts.
    Image {
        mount_path: PathBuf,
        verification: Verification,
        _anchor: File,
    },
    /// A pinned Nix store closure bind-mounted read-only from `/nix/store`,
    /// exposing no host `/usr`, `/bin`, `/lib`, `/lib64`, or `/etc`.
    NixClosure(NixClosureRuntime),
}

pub(super) struct NixClosureRuntime {
    pub(super) digest: String,
    pub(super) entries: Vec<VerifiedNixEntry>,
    pub(super) bin_sh: Option<PathBuf>,
    pub(super) usr_bin_env: Option<PathBuf>,
}

pub(super) struct VerifiedNixEntry {
    pub(super) store_path: PathBuf,
    pub(super) entry: VerifiedEntry,
}

pub(super) struct Verification {
    digest: String,
    roots: Vec<VerifiedRoot>,
}

struct VerifiedRoot {
    pathname: &'static str,
    entry: VerifiedEntry,
}

pub(super) enum VerifiedEntry {
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
    pub(super) fn resolve(nix_context: Option<&NixSandboxContext>) -> Result<Arc<Self>, String> {
        // An explicit operator-provisioned SquashFS/EROFS image always wins.
        if std::env::var_os(RUNTIME_ROOT_ENV).is_some() {
            return Self::resolve_base();
        }

        if let Some(ctx) = nix_context
            && Path::new(NIX_STORE_DIR).is_dir()
        {
            match Self::resolve_nix_closure(ctx) {
                Ok(runtime) => return Ok(runtime),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "could not verify Nix closure for strict sandbox; falling back to host runtime"
                    );
                }
            }
        }

        Self::resolve_base()
    }

    fn resolve_base() -> Result<Arc<Self>, String> {
        BASE_RUNTIME
            .get_or_init(|| {
                Self::load_base()
                    .map(Arc::new)
                    .map_err(|error| format!("{RUNTIME_ROOT_ENV}: {error}"))
            })
            .as_ref()
            .map(Arc::clone)
            .map_err(Clone::clone)
    }

    fn resolve_nix_closure(ctx: &NixSandboxContext) -> io::Result<Arc<Self>> {
        let key = NixClosureCacheKey {
            lock_digest: ctx.lock_digest.clone(),
            store_roots: ctx.store_roots.clone(),
            path_entries: ctx.path_entries.clone(),
        };
        let cache = NIX_CLOSURES.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(guard) = cache.lock()
            && let Some(existing) = guard.get(&key)
        {
            return Ok(Arc::clone(existing));
        }

        let closure_paths = query_nix_closure(&ctx.store_roots)?;
        let closure = verify_nix_closure(ctx, &closure_paths, Path::new(NIX_STORE_DIR))?;
        let runtime = Arc::new(Self::NixClosure(closure));
        if let Ok(mut guard) = cache.lock() {
            guard.insert(key, Arc::clone(&runtime));
        }
        Ok(runtime)
    }

    fn load_base() -> io::Result<Self> {
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
            Self::NixClosure(_) => Ok(None),
        }
    }

    pub(super) fn nix_closure(&self) -> Option<&NixClosureRuntime> {
        match self {
            Self::NixClosure(closure) => Some(closure),
            Self::Host { .. } | Self::Image { .. } => None,
        }
    }

    pub(super) fn identity(&self) -> (&'static str, &str) {
        match self {
            Self::Host { invocation } => ("host.invocation", invocation),
            Self::Image { verification, .. } => ("image.digest", &verification.digest),
            Self::NixClosure(closure) => ("nix.closure.digest", &closure.digest),
        }
    }

    pub(super) fn is_publishable(&self) -> bool {
        matches!(self, Self::Image { .. } | Self::NixClosure(_))
    }
}

fn query_nix_closure(store_roots: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let existing_roots: Vec<&PathBuf> = store_roots.iter().filter(|p| p.exists()).collect();
    if existing_roots.is_empty() {
        return Err(io::Error::other(
            "none of the Nix store roots referenced in the environment exist",
        ));
    }

    let output = Command::new("nix-store")
        .arg("--query")
        .arg("--requisites")
        .args(&existing_roots)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "nix-store --query --requisites failed ({}): {}",
            output.status,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8(output.stdout).map_err(io::Error::other)?;
    let paths: BTreeSet<PathBuf> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();
    if paths.is_empty() {
        return Err(io::Error::other("nix-store returned an empty closure"));
    }
    Ok(paths.into_iter().collect())
}

fn verify_nix_closure(
    ctx: &NixSandboxContext,
    closure_paths: &[PathBuf],
    store_root: &Path,
) -> io::Result<NixClosureRuntime> {
    let mut basenames = BTreeSet::new();
    for path in closure_paths {
        let relative = path.strip_prefix(store_root).map_err(|_| {
            io::Error::other(format!(
                "Nix closure path is outside {}: {}",
                store_root.display(),
                path.display()
            ))
        })?;
        let mut components = relative.components();
        let Some(std::path::Component::Normal(name)) = components.next() else {
            return Err(io::Error::other(format!(
                "invalid Nix store path: {}",
                path.display()
            )));
        };
        if components.next().is_some() {
            return Err(io::Error::other(format!(
                "Nix closure entry must be a top-level store path: {}",
                path.display()
            )));
        }
        let name_str = name
            .to_str()
            .filter(|s| is_valid_nix_store_basename(s))
            .ok_or_else(|| {
                io::Error::other(format!(
                    "invalid Nix store basename: {}",
                    name.to_string_lossy()
                ))
            })?;
        basenames.insert(name_str.to_string());
    }

    if basenames.is_empty() {
        return Err(io::Error::other("Nix closure is empty"));
    }

    let mut entries = Vec::with_capacity(basenames.len());
    for basename in &basenames {
        let path = store_root.join(basename);
        let metadata = fs::symlink_metadata(&path)?;
        let kind = metadata.file_type();
        let entry = if kind.is_symlink() {
            let link = fs::read_link(&path)?;
            if !link.starts_with(store_root) && link.is_absolute() {
                return Err(io::Error::other(format!(
                    "Nix store symlink escapes store root: {} -> {}",
                    path.display(),
                    link.display()
                )));
            }
            VerifiedEntry::Link(link)
        } else if kind.is_dir() || kind.is_file() {
            if metadata.mode() & 0o222 != 0 {
                return Err(io::Error::other(format!(
                    "Nix store entry is writable (mode {:o}): {}",
                    metadata.mode() & 0o777,
                    path.display()
                )));
            }
            VerifiedEntry::Bind {
                kind: MountKind::from(&metadata),
                identity: MountIdentity {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                },
            }
        } else {
            return Err(io::Error::other(format!(
                "special file in Nix store closure: {}",
                path.display()
            )));
        };
        entries.push(VerifiedNixEntry {
            store_path: path,
            entry,
        });
    }

    let closure_view = NixStoreClosureView {
        store_root,
        basenames: &basenames,
    };
    let bin_sh = find_closure_binary(&ctx.path_entries, &["sh", "bash"], closure_view);
    let usr_bin_env = find_closure_binary(&ctx.path_entries, &["env"], closure_view);

    let mut digest = Sha256::new();
    digest.update(b"cuenv.linux.nix-closure.v1\0");
    hash_field(&mut digest, ctx.lock_digest.as_bytes());
    match &bin_sh {
        Some(target) => hash_field(&mut digest, target.as_os_str().as_bytes()),
        None => digest.update(b"missing\0"),
    }
    match &usr_bin_env {
        Some(target) => hash_field(&mut digest, target.as_os_str().as_bytes()),
        None => digest.update(b"missing\0"),
    }
    for basename in &basenames {
        hash_field(&mut digest, basename.as_bytes());
    }

    Ok(NixClosureRuntime {
        digest: hex::encode(digest.finalize()),
        entries,
        bin_sh,
        usr_bin_env,
    })
}

#[derive(Clone, Copy)]
struct NixStoreClosureView<'a> {
    store_root: &'a Path,
    basenames: &'a BTreeSet<String>,
}

fn find_closure_binary(
    path_entries: &[PathBuf],
    names: &[&str],
    closure: NixStoreClosureView<'_>,
) -> Option<PathBuf> {
    for dir in path_entries {
        let Ok(relative) = dir.strip_prefix(closure.store_root) else {
            continue;
        };
        let Some(std::path::Component::Normal(store_entry)) = relative.components().next() else {
            continue;
        };
        let Some(store_entry) = store_entry.to_str() else {
            continue;
        };
        if !closure.basenames.contains(store_entry) {
            continue;
        }
        for name in names {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
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

    #[test]
    fn nix_closure_verifies_readonly_store_entries_and_is_publishable() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;

        let store = tempfile::tempdir()?;
        let bash_dir = store
            .path()
            .join("0123456789abcdfghijklmnpqrsvwxyz-bash-5.2");
        let coreutils_dir = store
            .path()
            .join("1123456789abcdfghijklmnpqrsvwxyz-coreutils-9.5");
        fs::create_dir_all(bash_dir.join("bin"))?;
        fs::create_dir_all(coreutils_dir.join("bin"))?;
        fs::write(bash_dir.join("bin/sh"), b"#!/bin/sh\n")?;
        fs::write(coreutils_dir.join("bin/env"), b"#!/bin/sh\n")?;
        fs::set_permissions(&bash_dir, fs::Permissions::from_mode(0o555))?;
        fs::set_permissions(&coreutils_dir, fs::Permissions::from_mode(0o555))?;

        let ctx = NixSandboxContext {
            lock_digest: "sha256:lock123".to_string(),
            store_roots: vec![bash_dir.clone(), coreutils_dir.clone()],
            path_entries: vec![bash_dir.join("bin"), coreutils_dir.join("bin")],
        };

        let a = verify_nix_closure(
            &ctx,
            &[bash_dir.clone(), coreutils_dir.clone()],
            store.path(),
        )?;
        let b = verify_nix_closure(
            &ctx,
            &[coreutils_dir.clone(), bash_dir.clone()],
            store.path(),
        )?;

        // Restore write bit so tempdir cleanup succeeds.
        fs::set_permissions(&bash_dir, fs::Permissions::from_mode(0o755))?;
        fs::set_permissions(&coreutils_dir, fs::Permissions::from_mode(0o755))?;

        assert_eq!(a.digest, b.digest);
        assert_eq!(a.bin_sh, Some(bash_dir.join("bin/sh")));
        assert_eq!(a.usr_bin_env, Some(coreutils_dir.join("bin/env")));
        assert_eq!(a.entries.len(), 2);

        let runtime = Runtime::NixClosure(a);
        assert!(runtime.is_publishable());
        assert_eq!(runtime.identity().0, "nix.closure.digest");
        Ok(())
    }

    #[test]
    fn nix_closure_rejects_writable_store_entry() -> io::Result<()> {
        let store = tempfile::tempdir()?;
        let writable = store
            .path()
            .join("0123456789abcdfghijklmnpqrsvwxyz-bash-5.2");
        fs::create_dir_all(&writable)?;

        let ctx = NixSandboxContext {
            lock_digest: "sha256:lock123".to_string(),
            store_roots: vec![writable.clone()],
            path_entries: Vec::new(),
        };

        let err = verify_nix_closure(&ctx, &[writable], store.path())
            .err()
            .ok_or_else(|| io::Error::other("writable Nix store entry must be rejected"))?;
        assert!(err.to_string().contains("writable"));
        Ok(())
    }
}
