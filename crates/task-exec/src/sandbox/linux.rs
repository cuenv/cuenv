//! Linux unprivileged namespace sandbox for strict hermetic execution.
//!
//! All paths, C strings, and mount targets are prepared **before fork** so the
//! child's `pre_exec` hook performs strictly async-signal-safe syscalls with
//! zero heap allocation, zero locking, and zero logging.

use super::{StrictSandboxProof, compute_policy_digest};
use cuenv_core::{Error, Result};
use sha2::{Digest as _, Sha256};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::process::Command;

/// Explicit read-only host runtime roots exposed inside the Linux sandbox.
///
/// Only entries that actually exist on the host are mounted, and the exact set
/// plus `/etc/os-release` identity is hashed into `cuenv.sandbox.policy`.
const LINUX_RUNTIME_ROOTS: &[&str] = &[
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

/// Minimal `/dev` character devices bind-mounted into the sandbox.
const LINUX_DEV_NODES: &[&str] = &["/dev/null", "/dev/zero", "/dev/urandom", "/dev/random"];

static PROBE_CACHE: OnceLock<std::result::Result<(), String>> = OnceLock::new();

/// Probe whether this Linux host permits unprivileged user + mount + IPC +
/// UTS + network namespaces.
pub fn probe(network_allowed: bool) -> std::result::Result<StrictSandboxProof, String> {
    let support = PROBE_CACHE.get_or_init(probe_unprivileged_namespaces);
    if let Err(reason) = support {
        return Err(reason.clone());
    }

    let policy_digest = compute_linux_policy_digest(network_allowed);
    let network_enforced = !network_allowed;

    Ok(StrictSandboxProof {
        platform: "linux-namespaces",
        policy_digest,
        network_enforced,
        publish_eligible: network_enforced,
    })
}

fn compute_linux_policy_digest(network_allowed: bool) -> String {
    let mut entries = Vec::new();
    entries.push(("platform", "linux-namespaces".to_string()));
    entries.push((
        "network",
        if network_allowed {
            "allowed".to_string()
        } else {
            "disabled".to_string()
        },
    ));

    let os_release_hash = std::fs::read("/etc/os-release").map_or_else(
        |_| "missing".to_string(),
        |bytes| hex::encode(Sha256::digest(&bytes)),
    );
    entries.push(("os_release", os_release_hash));

    let visible_roots: Vec<&str> = LINUX_RUNTIME_ROOTS
        .iter()
        .copied()
        .filter(|path| Path::new(path).exists())
        .collect();
    entries.push(("runtime_roots", visible_roots.join(",")));

    compute_policy_digest(&entries)
}

fn probe_unprivileged_namespaces() -> std::result::Result<(), String> {
    let flags = libc::CLONE_NEWUSER
        | libc::CLONE_NEWNS
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWUTS
        | libc::CLONE_NEWNET;

    // SAFETY: `fork()` followed immediately by `unshare()` and `_exit()` in
    // the child performs only async-signal-safe syscalls.
    #[expect(
        unsafe_code,
        reason = "Required to probe Linux unprivileged namespace support"
    )]
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let err = std::io::Error::last_os_error();
        return Err(format!("fork failed while probing namespaces: {err}"));
    }

    if pid == 0 {
        // SAFETY: Child process calls only async-signal-safe syscalls and exits via _exit.
        #[expect(
            unsafe_code,
            reason = "Required to probe Linux unshare in forked child"
        )]
        unsafe {
            let rc = libc::unshare(flags);
            if rc == 0 {
                libc::_exit(0);
            }
            let errno = *libc::__errno_location();
            let code = if errno > 0 && errno < 126 { errno } else { 1 };
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
        return Err(format!("unprivileged namespace unshare failed: {err}"));
    }

    Err("namespace probe child terminated abnormally".to_string())
}

#[derive(Clone)]
struct BindMount {
    source: CString,
    target: CString,
    read_only: bool,
}

/// Pre-fork prepared Linux namespace plan.
#[derive(Clone)]
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
    pub(crate) fn prepare(
        exec_root: &Path,
        workdir: &Path,
        task_tmp: &Path,
        task_home: &Path,
        network_allowed: bool,
    ) -> Result<(Self, StrictSandboxProof)> {
        let proof = probe(network_allowed).map_err(Error::configuration)?;

        let uid = rustix::process::getuid().as_raw();
        let gid = rustix::process::getgid().as_raw();
        let uid_map = format!("0 {uid} 1\n").into_bytes();
        let gid_map = format!("0 {gid} 1\n").into_bytes();

        let root_dir = task_tmp.join("sandbox-root");
        let put_old_dir = root_dir.join(".old_root");
        std::fs::create_dir_all(&put_old_dir)
            .map_err(|e| Error::io(format!("create sandbox root {}", put_old_dir.display()), e))?;

        let mut mounts = Vec::new();

        // Read-only runtime roots.
        for host_path_str in LINUX_RUNTIME_ROOTS {
            let host_path = Path::new(host_path_str);
            if !host_path.exists() {
                continue;
            }
            let rel = host_path_str.trim_start_matches('/');
            let target = root_dir.join(rel);
            prepare_mount_target(host_path, &target)?;
            mounts.push(BindMount {
                source: path_to_cstring(host_path)?,
                target: path_to_cstring(&target)?,
                read_only: true,
            });
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
            });
        }

        // Writable exec root mounted at its exact host path so relative/absolute
        // paths inside the exec root stay consistent.
        let exec_root_rel = strip_leading_slash(exec_root);
        let exec_root_target = root_dir.join(&exec_root_rel);
        std::fs::create_dir_all(&exec_root_target).map_err(|e| {
            Error::io(
                format!(
                    "create sandbox exec_root target {}",
                    exec_root_target.display()
                ),
                e,
            )
        })?;
        mounts.push(BindMount {
            source: path_to_cstring(exec_root)?,
            target: path_to_cstring(&exec_root_target)?,
            read_only: false,
        });

        // Private writable /tmp.
        let private_tmp = task_tmp.join("tmp");
        std::fs::create_dir_all(&private_tmp)
            .map_err(|e| Error::io(format!("create sandbox tmp {}", private_tmp.display()), e))?;
        let tmp_target = root_dir.join("tmp");
        std::fs::create_dir_all(&tmp_target)
            .map_err(|e| Error::io(format!("create sandbox /tmp {}", tmp_target.display()), e))?;
        mounts.push(BindMount {
            source: path_to_cstring(&private_tmp)?,
            target: path_to_cstring(&tmp_target)?,
            read_only: false,
        });

        // Private writable task HOME at its configured path.
        if task_home.is_absolute() && !task_home.starts_with(exec_root) {
            std::fs::create_dir_all(task_home)
                .map_err(|e| Error::io(format!("create task home {}", task_home.display()), e))?;
            let home_target = root_dir.join(strip_leading_slash(task_home));
            std::fs::create_dir_all(&home_target).map_err(|e| {
                Error::io(
                    format!("create sandbox home target {}", home_target.display()),
                    e,
                )
            })?;
            mounts.push(BindMount {
                source: path_to_cstring(task_home)?,
                target: path_to_cstring(&home_target)?,
                read_only: false,
            });
        }

        let mut unshare_flags =
            libc::CLONE_NEWUSER | libc::CLONE_NEWNS | libc::CLONE_NEWIPC | libc::CLONE_NEWUTS;
        if !network_allowed {
            unshare_flags |= libc::CLONE_NEWNET;
        }

        let plan = Self {
            unshare_flags,
            uid_map,
            gid_map,
            new_root: path_to_cstring(&root_dir)?,
            put_old: path_to_cstring(&put_old_dir)?,
            workdir: path_to_cstring(workdir)?,
            mounts,
        };

        Ok((plan, proof))
    }

    pub fn build_command(&self, program: &str, args: &[String]) -> Command {
        let mut command = Command::new(program);
        command.args(args);

        let plan = self.clone();
        // SAFETY: Every CString and byte slice inside `plan` was allocated
        // before fork. Inside the closure we invoke only async-signal-safe
        // Linux syscalls (`prctl`, `unshare`, `open`, `write`, `close`,
        // `mount`, `syscall(SYS_pivot_root)`, `chdir`, `umount2`).
        #[expect(
            unsafe_code,
            reason = "Required to enter Linux namespaces in child pre_exec"
        )]
        unsafe {
            command.pre_exec(move || enter_linux_sandbox(&plan));
        }

        command
    }
}

fn prepare_mount_target(source: &Path, target: &Path) -> Result<()> {
    let meta = std::fs::metadata(source)
        .map_err(|e| Error::io(format!("stat sandbox source {}", source.display()), e))?;
    if meta.is_dir() {
        std::fs::create_dir_all(target)
            .map_err(|e| Error::io(format!("create mount dir {}", target.display()), e))?;
    } else {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::io(format!("create mount parent {}", parent.display()), e))?;
        }
        if !target.exists() {
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(target)
                .map_err(|e| Error::io(format!("create mount file {}", target.display()), e))?;
        }
    }
    Ok(())
}

fn strip_leading_slash(path: &Path) -> PathBuf {
    path.strip_prefix("/").unwrap_or(path).to_path_buf()
}

fn path_to_cstring(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        Error::configuration(format!("path contains interior NUL: {}", path.display()))
    })
}

/// Async-signal-safe child-side namespace entry.
///
/// Performs zero heap allocations, zero mutex acquisitions, and zero logging.
#[expect(
    unsafe_code,
    reason = "Required for async-signal-safe Linux namespace setup"
)]
fn enter_linux_sandbox(plan: &LinuxSandboxPlan) -> std::io::Result<()> {
    unsafe {
        // Ensure the child dies if the parent dies unexpectedly.
        let _ = libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);

        if libc::unshare(plan.unshare_flags) != 0 {
            return Err(std::io::Error::last_os_error());
        }

        // Write setgroups ("deny"), uid_map, and gid_map before mounting.
        write_proc_file(c"/proc/self/setgroups".as_ptr(), b"deny")?;
        write_proc_file(c"/proc/self/uid_map".as_ptr(), &plan.uid_map)?;
        write_proc_file(c"/proc/self/gid_map".as_ptr(), &plan.gid_map)?;

        // Make mount propagation private so host mounts are unaffected.
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
            if libc::mount(
                m.source.as_ptr(),
                m.target.as_ptr(),
                std::ptr::null(),
                libc::MS_BIND | libc::MS_REC,
                std::ptr::null(),
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if m.read_only {
                let rc = libc::mount(
                    std::ptr::null(),
                    m.target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY | libc::MS_REC,
                    std::ptr::null(),
                );
                if rc != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }

        if libc::syscall(
            libc::SYS_pivot_root,
            plan.new_root.as_ptr(),
            plan.put_old.as_ptr(),
        ) != 0
        {
            return Err(std::io::Error::last_os_error());
        }

        if libc::chdir(c"/".as_ptr()) != 0 {
            return Err(std::io::Error::last_os_error());
        }

        if libc::umount2(c"/.old_root".as_ptr(), libc::MNT_DETACH) != 0 {
            return Err(std::io::Error::last_os_error());
        }

        if libc::chdir(plan.workdir.as_ptr()) != 0 {
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
