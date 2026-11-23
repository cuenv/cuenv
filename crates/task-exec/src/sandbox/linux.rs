//! Linux unprivileged namespace sandbox for strict hermetic execution.
//!
//! All paths, C strings, and mount targets are prepared **before fork** so the
//! child's `pre_exec` hook performs strictly async-signal-safe syscalls with
//! zero heap allocation, zero locking, and zero logging.

pub(super) mod launcher;
mod runtime;
mod supervisor;

use self::runtime::{MountKind, RuntimeSource, VerifiedEntry};
use super::{
    NetworkPolicy, NixSandboxContext, StrictSandboxOptions, StrictSandboxProof,
    compute_policy_digest,
};
use cuenv_core::{Error, Result};
use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tokio::process::Command;

/// Minimal `/dev` character devices bind-mounted into the sandbox.
const LINUX_DEV_NODES: &[&str] = &["/dev/null", "/dev/zero", "/dev/urandom", "/dev/random"];

type ProbeCache = Mutex<HashMap<String, std::result::Result<(), String>>>;

static PROBE_CACHE: OnceLock<ProbeCache> = OnceLock::new();

/// Probe whether this Linux host can enter and supervise a strict sandbox.
pub fn probe(
    network: NetworkPolicy,
    nix_context: Option<&NixSandboxContext>,
) -> std::result::Result<StrictSandboxProof, String> {
    let network_allowed = network == NetworkPolicy::Allowed;
    let runtime = runtime::Runtime::resolve(nix_context)?;
    let (runtime_key, runtime_value) = runtime.identity();
    let cache_key = if runtime_key == "host.invocation" {
        "host".to_string()
    } else {
        format!("{runtime_key}={runtime_value}")
    };
    let support = {
        let cache = PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let cached = cache
            .lock()
            .ok()
            .and_then(|guard| guard.get(&cache_key).cloned());
        if let Some(result) = cached {
            result
        } else {
            let result = probe_runtime(&runtime);
            if let Ok(mut guard) = cache.lock() {
                guard.insert(cache_key, result.clone());
            }
            result
        }
    };
    support?;

    let network_enforced = !network_allowed;
    let policy_digest = compute_policy_digest(&[
        ("platform", "linux-namespaces-v2".to_string()),
        (
            "network",
            if network_allowed {
                "allowed"
            } else {
                "disabled"
            }
            .to_string(),
        ),
        (runtime_key, runtime_value.to_string()),
        (
            "uid_gid_mapping",
            format!(
                "{}:{}",
                rustix::process::getuid().as_raw(),
                rustix::process::getgid().as_raw()
            ),
        ),
    ]);

    Ok(StrictSandboxProof {
        platform: "linux-namespaces",
        policy_digest,
        network_enforced,
        publish_eligible: network_enforced && runtime.is_publishable(),
    })
}

fn probe_runtime(runtime: &runtime::Runtime) -> std::result::Result<(), String> {
    // Some nested sandboxes can create namespaces but cannot bind runtime
    // roots that contain locked mounts. Probe with the same mount layout as a task.
    let root =
        tempfile::tempdir().map_err(|error| format!("create sandbox probe root: {error}"))?;
    let work = root.path().join("work");
    let scratch = root.path().join("scratch");
    let home = root.path().join("home");
    for directory in [&work, &scratch, &home] {
        std::fs::create_dir(directory)
            .map_err(|error| format!("create sandbox probe directory: {error}"))?;
    }
    let plan = LinuxSandboxPlan::with_runtime(
        StrictSandboxOptions {
            exec_root: &work,
            workdir: &work,
            task_tmp: &scratch,
            task_home: &home,
            network: NetworkPolicy::Disabled,
            nix_context: None,
        },
        runtime,
    )
    .map_err(|error| error.to_string())?;
    launcher::probe_support(&plan)
}

pub(super) fn probe_plan(plan: &LinuxSandboxPlan) -> std::result::Result<(), String> {
    let launcher_parent = rustix::process::getpid().as_raw_pid();

    #[expect(
        unsafe_code,
        reason = "Required to probe Linux unprivileged namespace support"
    )]
    // SAFETY: The child only performs async-signal-safe namespace setup and
    // exits without running application code.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!("fork failed while probing namespaces: {err}"));
    }

    if pid == 0 {
        #[expect(
            unsafe_code,
            reason = "Required to probe unprivileged Linux sandbox setup"
        )]
        // SAFETY: Child setup uses only stack data and prepared mapping bytes.
        unsafe {
            let result = enter_linux_sandbox(plan, launcher_parent);
            let code = match result {
                Ok(()) => 0,
                Err(error) => error
                    .raw_os_error()
                    .filter(|errno| (1..126).contains(errno))
                    .unwrap_or(1),
            };
            libc::_exit(code);
        }
    }

    let mut status: libc::c_int = 0;
    // SAFETY: Waiting on the specific child PID we just forked.
    #[expect(unsafe_code, reason = "Required to reap namespace probe child")]
    let waited = unsafe { libc::waitpid(pid, &raw mut status, 0) };
    if waited < 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!("waitpid failed while probing namespaces: {err}"));
    }

    if libc::WIFEXITED(status) {
        let code = libc::WEXITSTATUS(status);
        if code == 0 {
            return Ok(());
        }
        let err = std::io::Error::from_raw_os_error(code);
        return Err(format!("unprivileged Linux sandbox setup failed: {err}"));
    }

    Err("namespace probe child terminated abnormally".to_string())
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct BindMount {
    source: CString,
    target: CString,
    read_only: bool,
    image_identity: Option<MountIdentity>,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
struct MountIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

/// Pre-fork prepared Linux namespace plan.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct LinuxSandboxPlan {
    unshare_flags: libc::c_int,
    uid_map: Vec<u8>,
    gid_map: Vec<u8>,
    new_root: CString,
    put_old: CString,
    workdir: CString,
    mounts: Vec<BindMount>,
}

impl LinuxSandboxPlan {
    pub(crate) fn prepare(options: StrictSandboxOptions<'_>) -> Result<(Self, StrictSandboxProof)> {
        let proof = probe(options.network, options.nix_context).map_err(Error::configuration)?;
        let runtime =
            runtime::Runtime::resolve(options.nix_context).map_err(Error::configuration)?;
        Ok((Self::with_runtime(options, &runtime)?, proof))
    }

    fn with_runtime(options: StrictSandboxOptions<'_>, runtime: &runtime::Runtime) -> Result<Self> {
        let StrictSandboxOptions {
            exec_root,
            workdir,
            task_tmp,
            task_home,
            network,
            nix_context: _,
        } = options;
        let network_allowed = network == NetworkPolicy::Allowed;
        let exec_root = canonicalize(exec_root)?;
        let workdir = canonicalize(workdir)?;
        let task_tmp = canonicalize(task_tmp)?;
        let task_home = canonicalize(task_home)?;

        let (uid_map, gid_map) = namespace_mappings();

        let root_dir = task_tmp.join("sandbox-root");
        std::fs::create_dir(&root_dir)
            .map_err(|e| Error::io_with_path("create sandbox root", root_dir.clone(), e))?;
        let put_old_dir = root_dir.join(".old_root");
        std::fs::create_dir(&put_old_dir)
            .map_err(|e| Error::io_with_path("create old root target", put_old_dir.clone(), e))?;

        let mut mounts = Vec::new();

        if let Some(closure) = runtime.nix_closure() {
            // Expose only the verified Nix closure under /nix/store, plus
            // /bin/sh and /usr/bin/env pointing into that closure. No host
            // /usr, /bin, /lib, /lib64, or /etc trees are mounted.
            for item in &closure.entries {
                let target = root_dir.join(strip_leading_slash(&item.store_path));
                match &item.entry {
                    VerifiedEntry::Link(link) => {
                        create_mount_parent(&target)?;
                        symlink(link, &target).map_err(|error| {
                            Error::io_with_path("stage Nix store link", target, error)
                        })?;
                    }
                    VerifiedEntry::Bind { kind, identity } => {
                        prepare_mount_target_kind(*kind, &target)?;
                        mounts.push(BindMount {
                            source: path_to_cstring(&item.store_path)?,
                            target: path_to_cstring(&target)?,
                            read_only: true,
                            image_identity: Some(*identity),
                        });
                    }
                }
            }
            if let Some(sh) = &closure.bin_sh {
                let target = root_dir.join("bin/sh");
                create_mount_parent(&target)?;
                symlink(sh, &target)
                    .map_err(|error| Error::io_with_path("stage Nix /bin/sh", target, error))?;
            }
            if let Some(env_bin) = &closure.usr_bin_env {
                let target = root_dir.join("usr/bin/env");
                create_mount_parent(&target)?;
                symlink(env_bin, &target).map_err(|error| {
                    Error::io_with_path("stage Nix /usr/bin/env", target, error)
                })?;
            }
        } else {
            // Mount only the validated runtime roots. Preserve links inside the
            // image so absolute links resolve inside the sandbox after pivot_root.
            for &pathname in runtime::ROOTS {
                let source = runtime
                    .source(pathname)
                    .map_err(|error| Error::io(format!("read runtime source {pathname}"), error))?;
                let target = root_dir.join(pathname.trim_start_matches('/'));
                match source {
                    Some(RuntimeSource::Link(link)) => {
                        create_mount_parent(&target)?;
                        symlink(link, &target).map_err(|error| {
                            Error::io_with_path("stage runtime link", target, error)
                        })?;
                    }
                    Some(RuntimeSource::Bind {
                        path,
                        kind,
                        identity,
                    }) => {
                        prepare_mount_target_kind(kind, &target)?;
                        mounts.push(BindMount {
                            source: path_to_cstring(&path)?,
                            target: path_to_cstring(&target)?,
                            read_only: true,
                            image_identity: identity,
                        });
                    }
                    None => {}
                }
            }
        }

        // Minimal /dev nodes.
        for dev_str in LINUX_DEV_NODES {
            let dev_path = Path::new(dev_str);
            if !dev_path.exists() {
                continue;
            }
            let rel = dev_str.trim_start_matches('/');
            let target = root_dir.join(rel);
            prepare_mount_target(dev_path, &target)?;
            mounts.push(BindMount {
                source: path_to_cstring(dev_path)?,
                target: path_to_cstring(&target)?,
                read_only: false,
                image_identity: None,
            });
        }

        // Private writable /tmp.
        let private_tmp = task_tmp.join("tmp");
        std::fs::create_dir(&private_tmp)
            .map_err(|e| Error::io(format!("create sandbox tmp {}", private_tmp.display()), e))?;
        let tmp_target = root_dir.join("tmp");
        std::fs::create_dir_all(&tmp_target)
            .map_err(|e| Error::io(format!("create sandbox /tmp {}", tmp_target.display()), e))?;
        mounts.push(BindMount {
            source: path_to_cstring(&private_tmp)?,
            target: path_to_cstring(&tmp_target)?,
            read_only: false,
            image_identity: None,
        });

        // Verified actions use a stable absolute path even when their local
        // cache roots differ. Host-backed tasks retain the original path.
        let workspace = if runtime.is_publishable() {
            Path::new("/workspace")
        } else {
            &exec_root
        };
        let relative_workdir = workdir.strip_prefix(&exec_root).map_err(|_| {
            Error::configuration(format!(
                "strict sandbox workdir is outside the execution root: {}",
                workdir.display()
            ))
        })?;
        let sandbox_workdir = workspace.join(relative_workdir);
        let exec_root_target = root_dir.join(strip_leading_slash(workspace));
        // A target beneath /tmp must exist in the private tree before it covers /tmp.
        let staged_exec_root = sandbox_mount_target(workspace, &root_dir, &private_tmp);
        std::fs::create_dir_all(&staged_exec_root).map_err(|e| {
            Error::io(
                format!(
                    "create sandbox exec_root target {}",
                    staged_exec_root.display()
                ),
                e,
            )
        })?;
        mounts.push(BindMount {
            source: path_to_cstring(&exec_root)?,
            target: path_to_cstring(&exec_root_target)?,
            read_only: false,
            image_identity: None,
        });

        // Expose the staged home at the path assigned to HOME.
        let home_target = root_dir.join("cuenv-home");
        prepare_mount_target(&task_home, &home_target)?;
        mounts.push(BindMount {
            source: path_to_cstring(&task_home)?,
            target: path_to_cstring(&home_target)?,
            read_only: false,
            image_identity: None,
        });

        let mut unshare_flags = libc::CLONE_NEWUSER
            | libc::CLONE_NEWPID
            | libc::CLONE_NEWNS
            | libc::CLONE_NEWIPC
            | libc::CLONE_NEWUTS;
        if !network_allowed {
            unshare_flags |= libc::CLONE_NEWNET;
        }

        Ok(Self {
            unshare_flags,
            uid_map,
            gid_map,
            new_root: path_to_cstring(&root_dir)?,
            put_old: path_to_cstring(&put_old_dir)?,
            workdir: path_to_cstring(&sandbox_workdir)?,
            mounts,
        })
    }

    pub fn build_command(&self, program: &str, args: &[String]) -> Command {
        let mut command = Command::new(program);
        command.args(args).kill_on_drop(true);

        attach(self, command.as_std_mut());
        command
    }
}

pub(super) fn attach(plan: &LinuxSandboxPlan, command: &mut std::process::Command) {
    let plan = plan.clone();
    let launcher_parent = rustix::process::getpid().as_raw_pid();
    #[expect(
        unsafe_code,
        reason = "Required to enter Linux namespaces in child pre_exec"
    )]
    // SAFETY: All paths and byte slices in the plan were prepared before
    // fork. The closure uses only async-signal-safe Linux syscalls.
    unsafe {
        command.pre_exec(move || enter_linux_sandbox(&plan, launcher_parent));
    }
}

fn canonicalize(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .map_err(|error| Error::io_with_path("resolve sandbox path", path.to_path_buf(), error))
}

fn prepare_mount_target(source: &Path, target: &Path) -> Result<()> {
    let meta = std::fs::metadata(source)
        .map_err(|e| Error::io(format!("stat sandbox source {}", source.display()), e))?;
    prepare_mount_target_kind(MountKind::from(&meta), target)
}

fn prepare_mount_target_kind(kind: MountKind, target: &Path) -> Result<()> {
    match kind {
        MountKind::Directory => std::fs::create_dir_all(target).map_err(|error| {
            Error::io_with_path("create mount dir", target.to_path_buf(), error)
        })?,
        MountKind::File => {
            create_mount_parent(target)?;
            if !target.exists() {
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(target)
                    .map_err(|error| {
                        Error::io_with_path("create mount file", target.to_path_buf(), error)
                    })?;
            }
        }
    }
    Ok(())
}

fn create_mount_parent(target: &Path) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::io_with_path("create mount parent", parent.to_path_buf(), error)
        })?;
    }
    Ok(())
}

fn sandbox_mount_target(path: &Path, root: &Path, private_tmp: &Path) -> PathBuf {
    match path.strip_prefix("/tmp") {
        Ok(relative) => private_tmp.join(relative),
        Err(_) => root.join(strip_leading_slash(path)),
    }
}

fn strip_leading_slash(path: &Path) -> PathBuf {
    path.strip_prefix("/").unwrap_or(path).to_path_buf()
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        Error::configuration(format!("path contains interior NUL: {}", path.display()))
    })
}

#[derive(Clone, Copy)]
enum RootSwitch {
    PivotWithChrootFallback,
    #[cfg(test)]
    ChrootOnly,
}

/// Async-signal-safe child-side namespace entry.
///
/// Performs zero heap allocations, zero mutex acquisitions, and zero logging.
fn enter_linux_sandbox(
    plan: &LinuxSandboxPlan,
    launcher_parent: libc::pid_t,
) -> std::io::Result<()> {
    enter_linux_sandbox_with_root_switch(plan, launcher_parent, RootSwitch::PivotWithChrootFallback)
}

#[expect(
    unsafe_code,
    reason = "Required for async-signal-safe Linux namespace setup"
)]
fn enter_linux_sandbox_with_root_switch(
    plan: &LinuxSandboxPlan,
    launcher_parent: libc::pid_t,
    root_switch: RootSwitch,
) -> std::io::Result<()> {
    // SAFETY: The child uses live plan-owned C strings and only Linux syscalls.
    unsafe {
        // The launcher is the process managed by Tokio and the task registry.
        // Place it in its own process group before creating namespace init.
        if libc::setpgid(0, 0) != 0
            || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if libc::getppid() != launcher_parent {
            libc::_exit(125);
        }

        enter_namespaces(plan.unshare_flags, &plan.uid_map, &plan.gid_map)?;

        // Bind-mount new_root onto itself so pivot_root accepts it as a mount point.
        if libc::mount(
            plan.new_root.as_ptr(),
            plan.new_root.as_ptr(),
            std::ptr::null(),
            libc::MS_BIND | libc::MS_REC,
            std::ptr::null(),
        ) != 0
        {
            return Err(std::io::Error::last_os_error());
        }

        for m in &plan.mounts {
            bind_mount(m)?;
            if m.read_only {
                let rc = libc::mount(
                    std::ptr::null(),
                    m.target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND
                        | libc::MS_REMOUNT
                        | libc::MS_RDONLY
                        | libc::MS_NOSUID
                        | libc::MS_NODEV,
                    std::ptr::null(),
                );
                if rc != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }

        match root_switch {
            RootSwitch::PivotWithChrootFallback => {
                switch_root(plan.new_root.as_ptr(), plan.put_old.as_ptr())?;
            }
            #[cfg(test)]
            RootSwitch::ChrootOnly => {
                switch_root_chroot(plan.new_root.as_ptr())?;
            }
        }

        if libc::chdir(plan.workdir.as_ptr()) != 0 {
            return Err(std::io::Error::last_os_error());
        }

        supervisor::drop_setup_privileges()?;
        supervisor::run()
    }
}

#[expect(
    unsafe_code,
    reason = "Required to switch the sandbox root filesystem inside the mount namespace"
)]
unsafe fn switch_root(
    new_root: *const libc::c_char,
    put_old: *const libc::c_char,
) -> std::io::Result<()> {
    // SAFETY: Both C paths are NUL-terminated and live for the syscall.
    // Runs in a single-threaded fork child after entering private user and
    // mount namespaces and establishing all bind mounts under `new_root`.
    unsafe {
        if libc::syscall(libc::SYS_pivot_root, new_root, put_old) == 0 {
            if libc::chdir(c"/".as_ptr()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::umount2(c"/.old_root".as_ptr(), libc::MNT_DETACH) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            return Ok(());
        }

        let pivot_err = std::io::Error::last_os_error();
        if !matches!(
            pivot_err.raw_os_error(),
            Some(libc::EPERM | libc::ENOSYS | libc::EINVAL)
        ) {
            return Err(pivot_err);
        }

        switch_root_chroot(new_root)
    }
}

/// Container-compatible root switch used when a container seccomp filter blocks
/// `pivot_root` (`EPERM`/`ENOSYS`) or `/` is a non-pivotable initramfs (`EINVAL`).
///
/// Deliberately uses plain `chroot(new_root)` + `chdir("/")` without `MS_MOVE`:
/// overmounting `/` would make `fs->root` equal the mount-namespace root, causing
/// Linux's `current_chrooted()` check in `create_user_ns()` to return `false` and
/// allowing a confined task to `unshare(CLONE_NEWUSER)` to regain `CAP_SYS_CHROOT`
/// and escape into the overmounted host root. Keeping `fs->root != ns_root`
/// ensures the kernel blocks nested `CLONE_NEWUSER` with `EPERM`, while
/// `supervisor::drop_setup_privileges()` zeroes all capabilities, locks
/// `SECBIT_NOROOT_LOCKED`, and sets `PR_SET_NO_NEW_PRIVS` and `PR_SET_DUMPABLE = 0`.
#[expect(
    unsafe_code,
    reason = "Required to chroot into the prepared sandbox root when pivot_root is blocked"
)]
unsafe fn switch_root_chroot(new_root: *const libc::c_char) -> std::io::Result<()> {
    // SAFETY: `new_root` is a NUL-terminated C string live for the syscall.
    unsafe {
        if libc::chroot(new_root) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::chdir(c"/".as_ptr()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[expect(
    unsafe_code,
    reason = "Required to open and bind a verified source in the cloned namespace"
)]
unsafe fn bind_mount(mount: &BindMount) -> std::io::Result<()> {
    // SAFETY: The plan owns both C strings; a verified source uses a
    // descriptor local to the cloned namespace and valid until mount returns.
    unsafe {
        let Some(expected) = &mount.image_identity else {
            return bind(mount.source.as_ptr(), mount.target.as_ptr());
        };
        let fd = libc::open(
            mount.source.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        );
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let result = (|| {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            if libc::fstat(fd, stat.as_mut_ptr()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let stat = stat.assume_init();
            if stat.st_dev != expected.device || stat.st_ino != expected.inode {
                return Err(std::io::Error::from_raw_os_error(libc::ESTALE));
            }
            let source = proc_fd_path(fd.cast_unsigned());
            bind(source.as_ptr().cast(), mount.target.as_ptr())
        })();
        libc::close(fd);
        result
    }
}

#[expect(unsafe_code, reason = "Required to establish a bind mount")]
unsafe fn bind(source: *const libc::c_char, target: *const libc::c_char) -> std::io::Result<()> {
    // SAFETY: The C paths are NUL-terminated and live for the syscall.
    if unsafe {
        libc::mount(
            source,
            target,
            std::ptr::null(),
            libc::MS_BIND,
            std::ptr::null(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn proc_fd_path(mut fd: u32) -> [u8; 32] {
    let prefix = b"/proc/self/fd/";
    let mut digits = [0; 10];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + u8::try_from(fd % 10).unwrap_or_default();
        fd /= 10;
        if fd == 0 {
            break;
        }
    }
    let digits = &digits[start..];
    let mut path = [0; 32];
    path[..prefix.len()].copy_from_slice(prefix);
    path[prefix.len()..prefix.len() + digits.len()].copy_from_slice(digits);
    path
}

fn namespace_mappings() -> (Vec<u8>, Vec<u8>) {
    let uid = rustix::process::getuid().as_raw();
    let gid = rustix::process::getgid().as_raw();
    (
        format!("0 {uid} 1\n").into_bytes(),
        format!("0 {gid} 1\n").into_bytes(),
    )
}

#[expect(unsafe_code, reason = "Required for async-signal-safe namespace setup")]
unsafe fn enter_namespaces(
    flags: libc::c_int,
    uid_map: &[u8],
    gid_map: &[u8],
) -> std::io::Result<()> {
    // SAFETY: This runs in a single-threaded fork child. No host mounts may
    // propagate once the private mount namespace is established.
    unsafe {
        if libc::unshare(flags) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        write_proc_file(c"/proc/self/setgroups".as_ptr(), b"deny")?;
        write_proc_file(c"/proc/self/uid_map".as_ptr(), uid_map)?;
        write_proc_file(c"/proc/self/gid_map".as_ptr(), gid_map)?;
        if libc::sethostname(c"cuenv".as_ptr(), c"cuenv".to_bytes().len()) != 0
            || libc::setdomainname(c"".as_ptr(), 0) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        ) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[expect(
    unsafe_code,
    reason = "Required for async-signal-safe /proc/self/* writes"
)]
unsafe fn write_proc_file(path: *const libc::c_char, data: &[u8]) -> std::io::Result<()> {
    // SAFETY: The caller provides a valid C string, and the slice stays live through write.
    unsafe {
        let fd = libc::open(path, libc::O_WRONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let written = libc::write(fd, data.as_ptr().cast::<libc::c_void>(), data.len());
        let saved_err = if written < 0 {
            Some(std::io::Error::last_os_error())
        } else {
            None
        };
        let _ = libc::close(fd);
        if let Some(err) = saved_err {
            return Err(err);
        }
        if written < 0 || written.cast_unsigned() != data.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "short write to procfs map",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestSandboxDirs {
        root: PathBuf,
        work: PathBuf,
        ro_source: PathBuf,
        private_tmp: PathBuf,
    }

    fn test_plan(dirs: &TestSandboxDirs) -> std::io::Result<LinuxSandboxPlan> {
        let sandbox_root = dirs.root.join("sandbox-root");
        let put_old = sandbox_root.join(".old_root");
        let ro_target = sandbox_root.join("ro");
        let tmp_target = sandbox_root.join("tmp");
        let workspace_target = sandbox_root.join("workspace");
        for dir in [
            &sandbox_root,
            &put_old,
            &ro_target,
            &tmp_target,
            &workspace_target,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        let (uid_map, gid_map) = namespace_mappings();
        let to_c = |path: &Path| {
            CString::new(path.as_os_str().as_bytes())
                .map_err(|_| std::io::Error::other("path contains interior NUL"))
        };
        Ok(LinuxSandboxPlan {
            unshare_flags: libc::CLONE_NEWUSER
                | libc::CLONE_NEWPID
                | libc::CLONE_NEWNS
                | libc::CLONE_NEWIPC
                | libc::CLONE_NEWUTS
                | libc::CLONE_NEWNET,
            uid_map,
            gid_map,
            new_root: to_c(&sandbox_root)?,
            put_old: to_c(&put_old)?,
            workdir: CString::new("/workspace")
                .map_err(|_| std::io::Error::other("invalid workdir"))?,
            mounts: vec![
                BindMount {
                    source: to_c(&dirs.ro_source)?,
                    target: to_c(&ro_target)?,
                    read_only: true,
                    image_identity: None,
                },
                BindMount {
                    source: to_c(&dirs.private_tmp)?,
                    target: to_c(&tmp_target)?,
                    read_only: false,
                    image_identity: None,
                },
                BindMount {
                    source: to_c(&dirs.work)?,
                    target: to_c(&workspace_target)?,
                    read_only: false,
                    image_identity: None,
                },
            ],
        })
    }

    #[expect(
        unsafe_code,
        reason = "Verifies Linux chroot fallback confinement invariants inside a forked child"
    )]
    #[test]
    fn chroot_fallback_confines_filesystem_and_blocks_nested_userns() -> std::io::Result<()> {
        let tree = tempfile::tempdir()?;
        let root = tree.path().canonicalize()?;
        let dirs = TestSandboxDirs {
            work: root.join("work"),
            ro_source: root.join("ro-source"),
            private_tmp: root.join("private-tmp"),
            root,
        };
        let host_sentinel = dirs.root.join("host-sentinel.txt");
        for dir in [&dirs.work, &dirs.ro_source, &dirs.private_tmp] {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(dirs.ro_source.join("readonly.txt"), b"immutable")?;
        std::fs::write(&host_sentinel, b"host-secret")?;

        let plan = test_plan(&dirs)?;
        if let Err(reason) = probe_plan(&plan) {
            if std::env::var_os("CUENV_REQUIRE_STRICT_TEST").is_some() {
                return Err(std::io::Error::other(reason));
            }
            return Ok(());
        }

        let sentinel_c = CString::new(host_sentinel.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::other("sentinel path contains NUL"))?;
        let launcher_parent = rustix::process::getpid().as_raw_pid();

        // SAFETY: The child executes only async-signal-safe syscalls and exits
        // with a status code identifying any failed confinement invariant.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if pid == 0 {
            // SAFETY: Single-threaded fork child using pre-allocated CStrings and stack state.
            unsafe {
                if enter_linux_sandbox_with_root_switch(
                    &plan,
                    launcher_parent,
                    RootSwitch::ChrootOnly,
                )
                .is_err()
                {
                    libc::_exit(10);
                }

                // 1. Host sentinel outside the sandbox must not be visible.
                let host_fd = libc::open(sentinel_c.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
                if host_fd >= 0 {
                    libc::close(host_fd);
                    libc::_exit(11);
                }
                if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) {
                    libc::_exit(12);
                }

                // 2. `/` and `/..` must resolve to the same root directory.
                let mut root_stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                let mut parent_stat = std::mem::MaybeUninit::<libc::stat>::uninit();
                if libc::stat(c"/".as_ptr(), root_stat.as_mut_ptr()) != 0
                    || libc::stat(c"/..".as_ptr(), parent_stat.as_mut_ptr()) != 0
                {
                    libc::_exit(13);
                }
                let root_stat = root_stat.assume_init();
                let parent_stat = parent_stat.assume_init();
                if root_stat.st_dev != parent_stat.st_dev || root_stat.st_ino != parent_stat.st_ino
                {
                    libc::_exit(14);
                }

                // 3. `current_chrooted()` must block nested CLONE_NEWUSER with EPERM.
                if libc::unshare(libc::CLONE_NEWUSER) == 0
                    || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM)
                {
                    libc::_exit(15);
                }

                // 4. Dropped capabilities must block chroot() with EPERM.
                if libc::chroot(c".".as_ptr()) == 0
                    || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM)
                {
                    libc::_exit(16);
                }

                // 5. Read-only bind mount must be readable and reject writes.
                let ro_read = libc::open(
                    c"/ro/readonly.txt".as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC,
                );
                if ro_read < 0 {
                    libc::_exit(17);
                }
                libc::close(ro_read);

                let ro_write = libc::open(
                    c"/ro/readonly.txt".as_ptr(),
                    libc::O_WRONLY | libc::O_CLOEXEC,
                );
                if ro_write >= 0 {
                    libc::close(ro_write);
                    libc::_exit(18);
                }
                if !matches!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EROFS | libc::EACCES)
                ) {
                    libc::_exit(19);
                }

                // 6. Workdir (`/workspace`) and `/tmp` must be writable.
                let ws_fd = libc::open(
                    c"ok.txt".as_ptr(),
                    libc::O_CREAT | libc::O_WRONLY | libc::O_TRUNC | libc::O_CLOEXEC,
                    0o600,
                );
                if ws_fd < 0 {
                    libc::_exit(20);
                }
                libc::close(ws_fd);

                let tmp_fd = libc::open(
                    c"/tmp/ok.txt".as_ptr(),
                    libc::O_CREAT | libc::O_WRONLY | libc::O_TRUNC | libc::O_CLOEXEC,
                    0o600,
                );
                if tmp_fd < 0 {
                    libc::_exit(21);
                }
                libc::close(tmp_fd);

                libc::_exit(0);
            }
        }

        let mut status: libc::c_int = 0;
        // SAFETY: Waiting on the specific child PID just forked above.
        let waited = unsafe { libc::waitpid(pid, &raw mut status, 0) };
        if waited < 0 {
            return Err(std::io::Error::last_os_error());
        }
        assert!(
            libc::WIFEXITED(status),
            "chroot fallback child terminated abnormally (status={status})"
        );
        let code = libc::WEXITSTATUS(status);
        assert_eq!(code, 0, "chroot fallback invariant failed with code {code}");
        assert!(dirs.work.join("ok.txt").is_file());
        assert!(dirs.private_tmp.join("ok.txt").is_file());
        Ok(())
    }
}
