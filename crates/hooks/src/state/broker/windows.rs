//! Same-user named-pipe transport for hook results.

use std::ffi::c_void;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{self, NonNull};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_NO_TOKEN, ERROR_PIPE_BUSY, GetLastError, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, IsValidSid, OWNER_SECURITY_INFORMATION, PSID, RevertToSelf,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeServerProcessId, ImpersonateNamedPipeClient};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, GetProcessId, OpenProcess, OpenProcessToken,
    OpenThreadToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::state::HookRunIdentity;
use crate::{Error, Result};

const GREETING: u8 = 0x43;
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
static AUTH_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(32);

pub(super) type ServerStream = NamedPipeServer;

pub(super) struct Listener {
    pipe_name: String,
    owner: String,
    pending: NamedPipeServer,
}

enum PipeInstance {
    First,
    Subsequent,
}

fn pipe_name(identity: &HookRunIdentity) -> Result<String> {
    if identity.run_id.get_version() != Some(uuid::Version::Random) {
        return Err(Error::configuration("Invalid hook broker run identifier"));
    }
    Ok(format!(r"\\.\pipe\cuenv-hook-{}", identity.run_id.simple()))
}

pub(super) fn bind(_state_dir: &Path, identity: &HookRunIdentity) -> Result<Listener> {
    let pipe_name = pipe_name(identity)?;
    let owner = current_user_sid()?;
    let pending = create_instance(&pipe_name, &owner, PipeInstance::First)?;
    Ok(Listener {
        pipe_name,
        owner,
        pending,
    })
}

impl Listener {
    pub(super) async fn accept(&mut self) -> Result<NamedPipeServer> {
        self.pending
            .connect()
            .await
            .map_err(|_| Error::configuration("Cannot accept hook broker connection"))?;
        let replacement = create_instance(&self.pipe_name, &self.owner, PipeInstance::Subsequent)?;
        Ok(std::mem::replace(&mut self.pending, replacement))
    }
}

pub(super) async fn connect(
    _state_dir: &Path,
    identity: &HookRunIdentity,
) -> Result<Option<NamedPipeClient>> {
    let name = pipe_name(identity)?;
    let expected = current_user_sid()?;
    let Some(mut client) = tokio::time::timeout(AUTH_TIMEOUT, open_available_pipe(&name))
        .await
        .map_err(|_| Error::configuration("Hook broker connection timed out"))??
    else {
        return Ok(None);
    };
    let handle = client
        .as_handle()
        .try_clone_to_owned()
        .map_err(|_| Error::configuration("Cannot inspect hook broker connection"))?;
    tokio::time::timeout(
        AUTH_TIMEOUT,
        run_security_check(move || authenticate_remote_server(&handle, &expected)),
    )
    .await
    .map_err(|_| Error::configuration("Hook broker authentication timed out"))??;
    tokio::time::timeout(AUTH_TIMEOUT, client.write_all(&[GREETING]))
        .await
        .map_err(|_| Error::configuration("Hook broker handshake timed out"))?
        .map_err(|_| Error::configuration("Hook broker handshake failed"))?;
    Ok(Some(client))
}

async fn open_available_pipe(name: &str) -> Result<Option<NamedPipeClient>> {
    loop {
        match ClientOptions::new().open(name) {
            Ok(client) => return Ok(Some(client)),
            Err(error) if error.raw_os_error() == i32::try_from(ERROR_PIPE_BUSY).ok() => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) if error.raw_os_error() == i32::try_from(ERROR_FILE_NOT_FOUND).ok() => {
                return Ok(None);
            }
            Err(_) => return Err(Error::configuration("Cannot connect to hook broker")),
        }
    }
}

/// Recheck that an exact endpoint is absent before releasing its lease.
///
/// Named pipes leave no filesystem artifact; a busy or inaccessible pipe must
/// retain the lease because neither condition proves that its server stopped.
pub(super) fn remove_stale(
    _state_dir: &Path,
    identity: &HookRunIdentity,
) -> impl std::future::Future<Output = Result<()>> {
    std::future::ready(remove_stale_now(identity))
}

fn remove_stale_now(identity: &HookRunIdentity) -> Result<()> {
    let name = pipe_name(identity)?;
    match ClientOptions::new().open(&name) {
        Err(error) if error.raw_os_error() == i32::try_from(ERROR_FILE_NOT_FOUND).ok() => Ok(()),
        _ => Err(Error::configuration(
            "Hook broker endpoint is still present or unavailable",
        )),
    }
}

pub(super) async fn authenticate_server(server: &mut NamedPipeServer) -> Result<()> {
    let mut greeting = [0_u8; 1];
    tokio::time::timeout(AUTH_TIMEOUT, server.read_exact(&mut greeting))
        .await
        .map_err(|_| Error::configuration("Hook broker handshake timed out"))?
        .map_err(|_| Error::configuration("Hook broker handshake failed"))?;
    if greeting[0] != GREETING {
        return Err(Error::configuration("Invalid hook broker handshake"));
    }
    let expected = current_user_sid()?;
    let handle = server
        .as_handle()
        .try_clone_to_owned()
        .map_err(|_| Error::configuration("Cannot inspect hook broker connection"))?;
    tokio::time::timeout(
        AUTH_TIMEOUT,
        run_security_check(move || authenticate_remote_client(&handle, &expected)),
    )
    .await
    .map_err(|_| Error::configuration("Hook broker authentication timed out"))?
}

async fn run_security_check<F>(check: F) -> Result<()>
where
    F: FnOnce() -> Result<()> + Send + 'static,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let permit = AUTH_SLOTS
        .try_acquire()
        .map_err(|_| Error::configuration("Hook broker authentication is busy"))?;
    // A dedicated thread cannot leak an impersonation token into Tokio's pool.
    // The duplicated handle outlives a cancelled authentication future.
    std::thread::Builder::new()
        .name("cuenv-hook-pipe-auth".to_string())
        .spawn(move || {
            let _permit = permit;
            let _ = sender.send(check());
        })
        .map_err(|_| Error::configuration("Cannot start hook broker authentication"))?;
    receiver
        .await
        .map_err(|_| Error::configuration("Hook broker authentication failed"))?
}

fn create_instance(name: &str, owner: &str, instance: PipeInstance) -> Result<NamedPipeServer> {
    let descriptor = security_descriptor(owner)?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
            .map_err(|_| Error::configuration("Invalid hook broker security attributes"))?,
        lpSecurityDescriptor: descriptor.0.as_ptr(),
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(matches!(instance, PipeInstance::First))
        .reject_remote_clients(true);
    // SAFETY: attributes and its descriptor live for the synchronous create call.
    #[expect(
        unsafe_code,
        reason = "Windows named-pipe security attributes have no safe wrapper"
    )]
    unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) }
        .map_err(|_| Error::configuration("Cannot create private hook broker pipe"))
}

struct LocalAllocation(NonNull<c_void>);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: Windows allocated this pointer for LocalFree, exactly once.
        #[expect(unsafe_code, reason = "Windows LocalFree has no safe wrapper")]
        unsafe {
            LocalFree(self.0.as_ptr());
        }
    }
}

fn security_descriptor(owner: &str) -> Result<LocalAllocation> {
    let sddl: Vec<u16> = format!("O:{owner}D:P(A;;GA;;;{owner})")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut descriptor = ptr::null_mut();
    // SAFETY: SDDL is NUL-terminated and the output pointer is writable.
    #[expect(
        unsafe_code,
        reason = "Windows security descriptor creation has no safe wrapper"
    )]
    let success = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &raw mut descriptor,
            ptr::null_mut(),
        )
    };
    if success == 0 {
        return Err(Error::configuration(
            "Cannot create hook broker security descriptor",
        ));
    }
    NonNull::new(descriptor)
        .map(LocalAllocation)
        .ok_or_else(|| Error::configuration("Invalid hook broker security descriptor"))
}

fn current_user_sid() -> Result<String> {
    let mut raw = ptr::null_mut();
    // SAFETY: the pseudo-handle identifies the current thread and the output
    // pointer is writable. Only an owned returned handle is closed.
    #[expect(unsafe_code, reason = "Windows access token APIs have no safe wrapper")]
    let thread_opened =
        unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut raw) };
    if thread_opened == 0 {
        #[expect(unsafe_code, reason = "Windows last-error API has no safe wrapper")]
        let error = unsafe { GetLastError() };
        if error != ERROR_NO_TOKEN {
            return Err(Error::configuration(
                "Cannot inspect current Windows identity",
            ));
        }
        // SAFETY: the pseudo-handle identifies this process; raw is writable.
        #[expect(unsafe_code, reason = "Windows access token APIs have no safe wrapper")]
        let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut raw) };
        if opened == 0 {
            return Err(Error::configuration(
                "Cannot inspect current Windows identity",
            ));
        }
    }
    token_user_sid(&owned_handle(raw)?)
}

fn impersonated_user_sid() -> Result<String> {
    let mut raw = ptr::null_mut();
    // SAFETY: the pseudo-handle identifies this thread and the output is valid.
    #[expect(unsafe_code, reason = "Windows access token APIs have no safe wrapper")]
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut raw) };
    if opened == 0 {
        return Err(Error::configuration(
            "Cannot inspect hook broker client identity",
        ));
    }
    token_user_sid(&owned_handle(raw)?)
}

fn owned_handle(raw: *mut c_void) -> Result<OwnedHandle> {
    if raw.is_null() {
        return Err(Error::configuration("Invalid Windows security handle"));
    }
    // SAFETY: callers pass a freshly returned owning handle, never a pseudo-
    // handle. OwnedHandle closes it exactly once.
    #[expect(
        unsafe_code,
        reason = "Owning Windows handles requires raw-handle conversion"
    )]
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

fn token_user_sid(token: &OwnedHandle) -> Result<String> {
    let mut required = 0_u32;
    // SAFETY: a null buffer requests the size; the output pointer is valid.
    #[expect(unsafe_code, reason = "Windows token inspection has no safe wrapper")]
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &raw mut required,
        );
    }
    let length = usize::try_from(required)
        .map_err(|_| Error::configuration("Invalid Windows identity size"))?;
    if length < std::mem::size_of::<TOKEN_USER>() || length > 64 * 1024 {
        return Err(Error::configuration("Invalid Windows identity size"));
    }
    let mut storage = vec![0_u64; length.div_ceil(std::mem::size_of::<u64>())];
    // SAFETY: storage is large enough and aligned for TOKEN_USER; it remains
    // alive while the returned SID pointer is used.
    #[expect(unsafe_code, reason = "Windows token inspection has no safe wrapper")]
    let success = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            required,
            &raw mut required,
        )
    };
    if success == 0 {
        return Err(Error::configuration("Cannot inspect Windows identity"));
    }
    #[expect(unsafe_code, reason = "Windows token inspection has no safe wrapper")]
    let sid = unsafe { (*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    sid_string(sid)
}

fn sid_string(sid: PSID) -> Result<String> {
    // SAFETY: callers keep the token or descriptor owning the SID alive.
    #[expect(unsafe_code, reason = "Windows SID inspection has no safe wrapper")]
    let valid = unsafe { !sid.is_null() && IsValidSid(sid) != 0 };
    if !valid {
        return Err(Error::configuration("Invalid Windows identity"));
    }
    let mut string = ptr::null_mut();
    // SAFETY: the SID is valid and the output pointer is writable.
    #[expect(unsafe_code, reason = "Windows SID conversion has no safe wrapper")]
    let success = unsafe { ConvertSidToStringSidW(sid, &raw mut string) };
    if success == 0 {
        return Err(Error::configuration("Cannot inspect Windows identity"));
    }
    let allocation = NonNull::new(string.cast())
        .map(LocalAllocation)
        .ok_or_else(|| Error::configuration("Invalid Windows identity"))?;
    let mut length = 0_usize;
    // SAFETY: Windows guarantees this allocation contains a NUL-terminated SID.
    #[expect(unsafe_code, reason = "Windows returns a NUL-terminated SID string")]
    unsafe {
        while length < 256 && *string.add(length) != 0 {
            length += 1;
        }
    }
    if length == 256 {
        return Err(Error::configuration("Invalid Windows identity"));
    }
    #[expect(unsafe_code, reason = "Windows returns a NUL-terminated SID string")]
    let units = unsafe { std::slice::from_raw_parts(string, length) };
    let result =
        String::from_utf16(units).map_err(|_| Error::configuration("Invalid Windows identity"));
    drop(allocation);
    result
}

fn authenticate_remote_server(pipe: &OwnedHandle, expected: &str) -> Result<()> {
    verify_pipe_owner(pipe, expected)?;
    let mut pid = 0_u32;
    // SAFETY: the pipe handle and PID output storage remain valid.
    #[expect(
        unsafe_code,
        reason = "Windows named-pipe peer inspection has no safe wrapper"
    )]
    let success = unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &raw mut pid) };
    if success == 0 || pid == 0 {
        return Err(Error::configuration("Cannot inspect hook broker server"));
    }
    // SAFETY: OpenProcess returns an owning handle or null.
    #[expect(unsafe_code, reason = "Windows process inspection has no safe wrapper")]
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    let process = owned_handle(raw)?;
    #[expect(unsafe_code, reason = "Windows process inspection has no safe wrapper")]
    let actual_pid = unsafe { GetProcessId(process.as_raw_handle()) };
    if actual_pid != pid {
        return Err(Error::configuration("Hook broker server identity changed"));
    }
    let mut raw_token = ptr::null_mut();
    // SAFETY: process is a live handle and the output pointer is writable.
    #[expect(unsafe_code, reason = "Windows access token APIs have no safe wrapper")]
    let opened =
        unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &raw mut raw_token) };
    if opened == 0 {
        return Err(Error::configuration(
            "Cannot inspect hook broker server identity",
        ));
    }
    if token_user_sid(&owned_handle(raw_token)?)? != expected {
        return Err(Error::configuration(
            "Hook broker server has a different user",
        ));
    }
    let mut confirmed_pid = 0_u32;
    #[expect(
        unsafe_code,
        reason = "Windows named-pipe peer inspection has no safe wrapper"
    )]
    let confirmed =
        unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle(), &raw mut confirmed_pid) };
    if confirmed == 0 || confirmed_pid != pid {
        return Err(Error::configuration("Hook broker server identity changed"));
    }
    Ok(())
}

fn verify_pipe_owner(pipe: &OwnedHandle, expected: &str) -> Result<()> {
    let mut owner = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    // SAFETY: the handle and output pointers are valid. The owner is borrowed
    // from the returned descriptor, which remains live while it is inspected.
    #[expect(
        unsafe_code,
        reason = "Windows security descriptor inspection has no safe wrapper"
    )]
    let status = unsafe {
        GetSecurityInfo(
            pipe.as_raw_handle(),
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &raw mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(Error::configuration("Cannot inspect hook broker owner"));
    }
    let descriptor = NonNull::new(descriptor)
        .map(LocalAllocation)
        .ok_or_else(|| Error::configuration("Invalid hook broker owner"))?;
    let matches = sid_string(owner)? == expected;
    drop(descriptor);
    if !matches {
        return Err(Error::configuration("Hook broker has a different owner"));
    }
    Ok(())
}

fn authenticate_remote_client(pipe: &OwnedHandle, expected: &str) -> Result<()> {
    // The fixed greeting permits Windows to attach the client's security token.
    // Tokio's SECURITY_IDENTIFICATION mode allows identity queries but not use
    // of the client's rights.
    #[expect(
        unsafe_code,
        reason = "Windows pipe client authentication has no safe wrapper"
    )]
    let impersonated = unsafe { ImpersonateNamedPipeClient(pipe.as_raw_handle()) };
    if impersonated == 0 {
        return Err(Error::configuration(
            "Cannot inspect hook broker client identity",
        ));
    }
    let identity = impersonated_user_sid();
    // SAFETY: this dedicated OS thread has just impersonated the client, and no
    // await or other token mutation occurs between impersonation and reversion.
    #[expect(
        unsafe_code,
        reason = "Windows impersonation reversion has no safe wrapper"
    )]
    let reverted = unsafe { RevertToSelf() };
    if reverted == 0 {
        // This dedicated thread terminates rather than rejoining a worker pool.
        return Err(Error::configuration(
            "Cannot revert hook broker client identity",
        ));
    }
    if identity? != expected {
        return Err(Error::configuration(
            "Hook broker client has a different user",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
