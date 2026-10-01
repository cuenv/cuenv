//! Launching Terraform provider binaries and speaking their gRPC protocol.
//!
//! Providers are HashiCorp `go-plugin` servers. The host starts the binary
//! with a magic cookie in the environment; the plugin replies with a single
//! handshake line on standard output describing where it listens:
//!
//! ```text
//! CORE-PROTOCOL-VERSION|APP-PROTOCOL-VERSION|NETWORK|ADDRESS|PROTOCOL|SERVER-CERT
//! 1|6|unix|/tmp/cuenv-plugin-1a2b/plugin123|grpc|
//! ```
//!
//! cuenv offers protocol versions 5 and 6 and speaks whichever the provider
//! selects. It does not request AutoMTLS, so providers serve plaintext gRPC
//! on a private unix socket, the same as Terraform with
//! `TF_DISABLE_PLUGIN_TLS`.
//!
//! Each provider gets a private, short socket directory (unix socket paths
//! are limited to about 108 bytes) that is removed when the provider stops
//! (or by [`crate::Cancellation::terminate_providers`] on a forced exit),
//! and never inherits the state store's credentials.
//!
//! Providers run in their own process group, so an interrupt typed at the
//! terminal reaches cuenv only; cuenv then asks them to stop or kills them
//! through [`crate::Cancellation`]. Killing a provider kills its whole
//! process group, so processes it started die with it. They are also
//! killed whenever their client is dropped.
//!
//! On Linux each provider also asks the kernel to kill it when its parent
//! goes away (`PR_SET_PDEATHSIG`), so even a cuenv killed with `SIGKILL`
//! leaves no provider behind. The kernel sends that signal when the
//! *thread* that spawned the provider exits, so providers must be launched
//! from a long-lived async runtime thread (as [`ProviderClient::launch`]
//! is, being async), never from `spawn_blocking` or `block_in_place`,
//! whose threads come and go. macOS has no equivalent; there a provider
//! outlives a cuenv killed with `SIGKILL`.
//!
//! Tracing records provider log metadata only. Serious lines have control
//! characters removed and reach error reports through the redacted event path.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use hyper_util::rt::TokioIo;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tonic::codegen::http::uri::PathAndQuery;
use tonic::transport::{Channel, Endpoint, Uri};

use crate::cancellation::Cancellation;
use crate::error::{
    InfrastructureError, Result, strip_control_characters, strip_control_characters_except_newlines,
};
use crate::protocol::{self, Diagnostic, DynamicValue};
use crate::schema::ProviderSchema;

/// Magic cookie Terraform providers require before they serve.
const MAGIC_COOKIE_KEY: &str = "TF_PLUGIN_MAGIC_COOKIE";
const MAGIC_COOKIE_VALUE: &str = "d602bf8f470bc67ca7faa0386276bbdd4330efaf76d1a219cb4d6991ca9872b2";

/// Version string cuenv reports to providers during configuration.
/// Providers gate behaviour on this, so report a modern Terraform.
const TERRAFORM_VERSION: &str = "1.9.0";

/// Terraform raises gRPC message limits to 256 MiB; large provider schemas
/// (AWS, Azure) exceed tonic's 4 MiB default.
const MAX_MESSAGE_BYTES: usize = 256 * 1024 * 1024;

/// How long to wait for a provider's handshake line.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

/// Longest handshake line accepted; real ones are well under 200 bytes.
const MAXIMUM_HANDSHAKE_BYTES: usize = 4096;

/// How long a stopping provider gets for each shutdown step.
const SHUTDOWN_STEP_TIMEOUT: Duration = Duration::from_secs(3);

/// Provider log lines kept for error reports.
const RETAINED_LOG_LINES: usize = 40;

/// Longest provider log line kept; the rest of a longer line is dropped.
const MAXIMUM_LOG_LINE_BYTES: usize = 2048;

/// How often a stopping provider is checked for exit.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// go-plugin's controller service, used for a graceful shutdown.
const CONTROLLER_SHUTDOWN_PATH: &str = "/plugin.GRPCController/Shutdown";

/// Plugin protocol major version negotiated with a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// `tfplugin5` — SDKv2 and most multiplexed providers.
    Version5,
    /// `tfplugin6` — Plugin Framework providers.
    Version6,
}

/// Provider RPCs cuenv issues.
#[derive(Debug, Clone, Copy)]
enum RemoteProcedure {
    GetSchema,
    ValidateProviderConfiguration,
    Configure,
    ValidateResourceConfiguration,
    UpgradeResourceState,
    ReadResource,
    PlanResourceChange,
    ApplyResourceChange,
    Stop,
}

impl RemoteProcedure {
    const fn path(self, protocol: Protocol) -> &'static str {
        match (protocol, self) {
            (Protocol::Version5, Self::GetSchema) => "/tfplugin5.Provider/GetSchema",
            (Protocol::Version5, Self::ValidateProviderConfiguration) => {
                "/tfplugin5.Provider/PrepareProviderConfig"
            }
            (Protocol::Version5, Self::Configure) => "/tfplugin5.Provider/Configure",
            (Protocol::Version5, Self::ValidateResourceConfiguration) => {
                "/tfplugin5.Provider/ValidateResourceTypeConfig"
            }
            (Protocol::Version5, Self::UpgradeResourceState) => {
                "/tfplugin5.Provider/UpgradeResourceState"
            }
            (Protocol::Version5, Self::ReadResource) => "/tfplugin5.Provider/ReadResource",
            (Protocol::Version5, Self::PlanResourceChange) => {
                "/tfplugin5.Provider/PlanResourceChange"
            }
            (Protocol::Version5, Self::ApplyResourceChange) => {
                "/tfplugin5.Provider/ApplyResourceChange"
            }
            (Protocol::Version5, Self::Stop) => "/tfplugin5.Provider/Stop",
            (Protocol::Version6, Self::GetSchema) => "/tfplugin6.Provider/GetProviderSchema",
            (Protocol::Version6, Self::ValidateProviderConfiguration) => {
                "/tfplugin6.Provider/ValidateProviderConfig"
            }
            (Protocol::Version6, Self::Configure) => "/tfplugin6.Provider/ConfigureProvider",
            (Protocol::Version6, Self::ValidateResourceConfiguration) => {
                "/tfplugin6.Provider/ValidateResourceConfig"
            }
            (Protocol::Version6, Self::UpgradeResourceState) => {
                "/tfplugin6.Provider/UpgradeResourceState"
            }
            (Protocol::Version6, Self::ReadResource) => "/tfplugin6.Provider/ReadResource",
            (Protocol::Version6, Self::PlanResourceChange) => {
                "/tfplugin6.Provider/PlanResourceChange"
            }
            (Protocol::Version6, Self::ApplyResourceChange) => {
                "/tfplugin6.Provider/ApplyResourceChange"
            }
            (Protocol::Version6, Self::Stop) => "/tfplugin6.Provider/StopProvider",
        }
    }
}

/// Parsed go-plugin handshake line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handshake {
    /// Negotiated plugin protocol.
    pub protocol: Protocol,
    /// Listener network (`unix` or `tcp`).
    pub network: String,
    /// Listener address.
    pub address: String,
}

impl Handshake {
    /// Parse a go-plugin handshake line.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Plugin`] for malformed lines, unsupported
    /// versions, non-gRPC plugins, or plugins demanding TLS.
    pub fn parse(line: &str) -> Result<Self> {
        let parts: Vec<&str> = line.trim().split('|').collect();
        if parts.len() < 5 {
            return Err(InfrastructureError::plugin(format!(
                "malformed plugin handshake: {line:?}"
            )));
        }
        if parts[0] != "1" {
            return Err(InfrastructureError::plugin(format!(
                "unsupported go-plugin core protocol version {}",
                parts[0]
            )));
        }
        let protocol = match parts[1] {
            "5" => Protocol::Version5,
            "6" => Protocol::Version6,
            other => {
                return Err(InfrastructureError::plugin(format!(
                    "provider selected unsupported plugin protocol {other}"
                )));
            }
        };
        if parts[4] != "grpc" {
            return Err(InfrastructureError::plugin(format!(
                "provider speaks '{}', only grpc is supported",
                parts[4]
            )));
        }
        if parts
            .get(5)
            .is_some_and(|certificate| !certificate.is_empty())
        {
            return Err(InfrastructureError::plugin(
                "provider requested TLS; cuenv only supports plaintext unix-socket plugins",
            ));
        }
        Ok(Self {
            protocol,
            network: parts[2].to_string(),
            address: parts[3].to_string(),
        })
    }
}

/// How to start a provider.
#[derive(Clone, Copy)]
pub struct LaunchOptions<'launch> {
    /// Provider executable.
    pub binary: &'launch Path,
    /// Environment variables the provider must not inherit, such as the
    /// state store's authentication token.
    pub withheld_environment_variables: &'launch [String],
    /// Resolved, policy-authorized Cuenv variables overlaid on the host
    /// environment before the withheld names are removed.
    pub provider_environment_variables: &'launch BTreeMap<String, String>,
    /// Interruption the provider process is registered with, so a stop
    /// request reaches it and termination kills it.
    pub cancellation: &'launch Cancellation,
}

impl fmt::Debug for LaunchOptions<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LaunchOptions")
            .field("binary", &self.binary)
            .field(
                "withheld_environment_variables",
                &self.withheld_environment_variables,
            )
            .field(
                "provider_environment_variable_names",
                &self.provider_environment_variables.keys(),
            )
            .finish_non_exhaustive()
    }
}

/// Variables an isolated provider still inherits from the host.
///
/// These are the ones any process needs to find its tools and home directory, reach the network
/// through the host's proxy and trust the host's certificate authorities.
/// `TMPDIR` is listed for completeness; cuenv always sets its own.
pub const ISOLATED_INHERITED_ENVIRONMENT_VARIABLES: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "all_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// The host variable names an isolated provider must not inherit.
///
/// That is every name in `ambient` that is neither on the
/// [`ISOLATED_INHERITED_ENVIRONMENT_VARIABLES`] allowlist nor passed by the
/// project (`provided`, which reaches the provider through
/// [`LaunchOptions::provider_environment_variables`]). Pass the result as
/// [`LaunchOptions::withheld_environment_variables`], together with the names
/// the project's policy withholds; the provider then sees the allowlist, the
/// project's values and cuenv's own handshake variables, and nothing else.
///
/// Names that are not valid unicode cannot be listed; they are never
/// inherited at all (see [`ProviderClient::launch`]), in either mode.
#[must_use]
#[tracing::instrument(level = "debug", skip_all, fields(provided = provided.len()))]
pub fn isolated_withheld_names(
    ambient: impl IntoIterator<Item = std::ffi::OsString>,
    provided: &[String],
) -> Vec<String> {
    let mut names: Vec<String> = ambient
        .into_iter()
        .filter_map(|name| name.into_string().ok())
        .filter(|name| {
            !ISOLATED_INHERITED_ENVIRONMENT_VARIABLES.contains(&name.as_str())
                && !provided.contains(name)
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The host variables a provider may inherit before names are withheld:
/// those whose names are valid unicode.
///
/// Withheld names are text, so a variable whose name is not valid unicode
/// could never be withheld, in `isolated` mode or any other. It is dropped
/// instead of being inherited by default.
fn listable_ambient_environment(
    ambient: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    ambient
        .into_iter()
        .filter(|(name, _)| name.to_str().is_some())
        .collect()
}

fn configure_provider_environment(
    command: &mut Command,
    options: &LaunchOptions<'_>,
    socket_directory: &Path,
) {
    // Keep the provider's existing access to host credentials and runtime
    // variables while making Cuenv precedence explicit. The environment is
    // built from nothing: a variable reaches the provider only by being
    // listed here, so one whose name cannot be listed (not valid unicode)
    // never does.
    command
        .env_clear()
        .envs(listable_ambient_environment(std::env::vars_os()))
        .envs(options.provider_environment_variables);
    // Remove withheld names, including names also present in the resolved
    // Cuenv environment.
    for name in options.withheld_environment_variables {
        command.env_remove(name);
    }
    // Set cuenv's own variables last: a policy that withholds one of these
    // names must not break the handshake or the private socket directory.
    command
        .env(MAGIC_COOKIE_KEY, MAGIC_COOKIE_VALUE)
        .env("PLUGIN_PROTOCOL_VERSIONS", "5,6")
        .env("PLUGIN_UNIX_SOCKET_DIR", socket_directory)
        .env("TMPDIR", socket_directory);
}

/// Redacts text a provider wrote, before anything else is done to it.
pub type LogRedactor = fn(&str) -> String;

static LOG_REDACTOR: OnceLock<LogRedactor> = OnceLock::new();

/// Install the function that replaces secrets in text read from providers.
///
/// It applies to their log lines and their error messages. The command that owns the
/// secret registry installs it once at startup; the first installation wins.
///
/// The redactor is process-wide on purpose. It is a view of the secret
/// registry, which is itself one per process, so a redactor chosen per
/// launch could only differ from it by hiding less; and the same function
/// serves every place provider text is read (log lines, gRPC messages,
/// diagnostics rendered by the engine), none of which is handed the options
/// of the launch that produced it. Installing the same function again (each
/// command run does) is expected and silent; installing a different one is
/// ignored and logged, so a second owner of the registry cannot silently
/// lose.
///
/// Provider text is redacted before control characters are stripped: a
/// secret that contains one would no longer match after the stripping.
pub fn install_log_redactor(redactor: LogRedactor) {
    if let Err(rejected) = LOG_REDACTOR.set(redactor)
        && LOG_REDACTOR
            .get()
            .is_some_and(|installed| !std::ptr::fn_addr_eq(*installed, rejected))
    {
        tracing::warn!("a different provider log redactor is already installed; keeping the first");
    }
}

/// `text` with the installed redactor applied; unchanged when none is
/// installed.
pub(crate) fn redact_provider_text(text: &str) -> String {
    LOG_REDACTOR
        .get()
        .map_or_else(|| text.to_string(), |redact| redact(text))
}

/// A provider's gRPC status message as it may be shown: redacted first and
/// only then stripped of control characters, because a secret that contains
/// one would no longer match once it was stripped.
fn displayable_provider_message(message: &str) -> String {
    strip_control_characters_except_newlines(&redact_provider_text(message))
}

/// A running provider process, shared with [`Cancellation`] so an
/// interrupt can ask it to stop or kill it from any thread.
#[derive(Debug)]
pub struct ProviderProcess {
    name: String,
    child: Mutex<Child>,
    connection: OnceLock<Connection>,
}

/// The gRPC connection to a provider, known once its handshake completes.
#[derive(Debug, Clone)]
struct Connection {
    protocol: Protocol,
    channel: Channel,
}

impl ProviderProcess {
    /// Send the process and its process group `SIGKILL` (or the platform
    /// equivalent) without waiting for it to exit.
    pub(crate) fn kill(&self) {
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        // The identifier is known only until the process is reaped, so the
        // group it leads cannot have been reused by then.
        #[cfg(unix)]
        if let Some(leader) = child.id() {
            kill_process_group(&self.name, leader);
        }
        if let Err(error) = child.start_kill() {
            tracing::debug!(provider = %self.name, %error, "provider kill failed; it may have exited already");
        }
    }

    /// Ask the provider to stop its in-flight operations.
    pub(crate) async fn request_stop(&self) {
        let Some(connection) = self.connection.get() else {
            return;
        };
        let stop = tokio::time::timeout(
            SHUTDOWN_STEP_TIMEOUT,
            unary::<_, protocol::StopResponse>(
                &connection.channel,
                RemoteProcedure::Stop.path(connection.protocol),
                protocol::Empty {},
            ),
        )
        .await;
        match stop {
            Ok(Ok(response)) if response.error.is_empty() => {
                tracing::debug!(provider = %self.name, "provider stopped its operations");
            }
            Ok(Ok(_)) => {
                tracing::debug!(provider = %self.name, "provider stop reported an error");
            }
            _ => tracing::debug!(provider = %self.name, "provider stop procedure did not complete"),
        }
    }

    fn has_exited(&self) -> bool {
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        matches!(child.try_wait(), Ok(Some(_)) | Err(_))
    }

    /// Wait up to `timeout` for the process to exit.
    async fn wait_for_exit(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, async {
            while !self.has_exited() {
                tokio::time::sleep(EXIT_POLL_INTERVAL).await;
            }
        })
        .await
        .is_ok()
    }
}

/// Send `SIGKILL` to the process group led by the provider process
/// `leader`, which [`ProviderClient::launch`] made a group leader.
#[cfg(unix)]
#[expect(unsafe_code, reason = "killpg has no safe standard library wrapper")]
fn kill_process_group(name: &str, leader: u32) {
    let Ok(group) = libc::pid_t::try_from(leader) else {
        return;
    };
    // SAFETY: killpg only sends a signal and touches no memory. `group` is
    // the identifier of a provider spawned as the leader of its own process
    // group (`process_group(0)`) and not yet reaped (the caller holds its
    // live identifier), so the group belongs to that provider and the
    // processes it started, never to an unrelated one.
    let result = unsafe { libc::killpg(group, libc::SIGKILL) };
    if result != 0 {
        tracing::debug!(
            provider = %name,
            error = %std::io::Error::last_os_error(),
            "provider process group kill failed; it may have exited already"
        );
    }
}

/// Ask the kernel to kill the provider when the thread that spawned it
/// exits (see the module documentation), and fail the spawn if cuenv is
/// already gone.
#[cfg(target_os = "linux")]
#[expect(
    unsafe_code,
    reason = "pre_exec and prctl have no safe standard library wrapper"
)]
fn kill_with_parent(command: &mut Command) {
    let parent = libc::pid_t::try_from(std::process::id()).unwrap_or(0);
    // SAFETY: the closure runs in the child between fork and exec. It only
    // calls prctl and getppid, both async-signal-safe, and builds errors
    // from raw codes without allocating, so it cannot deadlock on a lock
    // held by another thread of the parent at the time of the fork.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The parent died before the request took effect.
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

/// Recent serious provider log lines (hclog errors and warnings, and
/// anything that is not hclog JSON, such as a Go panic), attached to errors
/// so failures are diagnosable. Each line is bounded.
#[derive(Debug, Clone, Default)]
struct ProviderLog {
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl ProviderLog {
    fn record(&self, line: String) {
        let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        if lines.len() == RETAINED_LOG_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    fn render(&self) -> String {
        self.lines
            .lock()
            .map(|lines| {
                if lines.is_empty() {
                    String::new()
                } else {
                    let joined = lines.iter().cloned().collect::<Vec<_>>().join("\n  ");
                    format!("\nrecent provider log:\n  {joined}")
                }
            })
            .unwrap_or_default()
    }
}

/// A private directory for the provider's unix socket, removed on drop,
/// and registered with the [`Cancellation`] so a forced exit removes it
/// too.
#[derive(Debug)]
struct SocketDirectory {
    path: PathBuf,
    cancellation: Cancellation,
}

impl SocketDirectory {
    fn create(cancellation: &Cancellation) -> Result<Self> {
        let base = if cfg!(unix) && Path::new("/tmp").is_dir() {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let path = base.join(format!("cuenv-plugin-{}", &suffix[..12]));
        // Created private in one step: there is no moment in which another
        // user could open it, and an existing path is never reused.
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).map_err(|error| {
            InfrastructureError::input_output("create provider socket directory", error)
        })?;
        cancellation.register_socket_directory(&path);
        Ok(Self {
            path,
            cancellation: cancellation.clone(),
        })
    }
}

impl Drop for SocketDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        self.cancellation.unregister_socket_directory(&self.path);
    }
}

/// A running provider plugin and a gRPC client connected to it.
///
/// Dropping the client kills the provider process.
#[derive(Debug)]
pub struct ProviderClient {
    name: String,
    protocol: Protocol,
    channel: Channel,
    process: Arc<ProviderProcess>,
    log: ProviderLog,
    _socket_directory: SocketDirectory,
}

impl ProviderClient {
    /// Start a provider binary and connect to it.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Plugin`] if the binary cannot be started, does
    /// not complete the handshake in time, or cannot be dialed. Errors include
    /// the provider's recent log output.
    #[tracing::instrument(skip_all, fields(binary = %options.binary.display()))]
    pub async fn launch(options: &LaunchOptions<'_>) -> Result<Self> {
        let binary = options.binary;
        let socket_directory = SocketDirectory::create(options.cancellation)?;
        let mut command = Command::new(binary);
        configure_provider_environment(&mut command, options, &socket_directory.path);
        // Own process group: a terminal interrupt must reach cuenv only, so
        // in-flight provider operations are stopped deliberately, not killed.
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(target_os = "linux")]
        kill_with_parent(&mut command);
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                InfrastructureError::plugin(format!(
                    "failed to start {}: {error}",
                    binary.display()
                ))
            })?;

        let standard_output = child.stdout.take().ok_or_else(|| {
            InfrastructureError::plugin("provider standard output was not captured")
        })?;
        let standard_error = child.stderr.take().ok_or_else(|| {
            InfrastructureError::plugin("provider standard error was not captured")
        })?;

        let name = binary.file_name().map_or_else(
            || "provider".to_string(),
            |file_name| file_name.to_string_lossy().into_owned(),
        );
        let process = Arc::new(ProviderProcess {
            name: name.clone(),
            child: Mutex::new(child),
            connection: OnceLock::new(),
        });
        options.cancellation.register(&process);
        let log = ProviderLog::default();
        // The drain ends with the provider's standard error.
        drop(spawn_log_drain(name.clone(), standard_error, log.clone()));

        let mut reader = BufReader::new(standard_output);
        let line = tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            read_bounded_line(&mut reader, MAXIMUM_HANDSHAKE_BYTES),
        )
        .await
        .map_err(|_| {
            InfrastructureError::plugin(format!(
                "{name} did not complete the plugin handshake within {} seconds{}",
                HANDSHAKE_TIMEOUT.as_secs(),
                log.render()
            ))
        })?
        .map_err(|error| InfrastructureError::input_output("read provider handshake", error))?;
        if line.as_ref().is_some_and(|line| line.truncated) {
            return Err(InfrastructureError::plugin(
                "provider handshake line is too long; is this a Terraform provider?",
            ));
        }
        let Some(BoundedLine { text: line, .. }) = line else {
            // Give the log drain a moment to capture why the provider exited.
            tokio::time::sleep(Duration::from_millis(200)).await;
            return Err(InfrastructureError::plugin(format!(
                "{name} exited before the plugin handshake{}",
                log.render()
            )));
        };
        let handshake = Handshake::parse(&line)?;
        tracing::debug!(provider = %name, "provider handshake completed");

        // Keep draining standard output so the plugin never blocks on a full
        // pipe; nothing after the handshake is meaningful, so discard it.
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
        });

        let channel = connect(&handshake)
            .await
            .map_err(|error| InfrastructureError::plugin(format!("{error}{}", log.render())))?;
        let _ = process.connection.set(Connection {
            protocol: handshake.protocol,
            channel: channel.clone(),
        });
        Ok(Self {
            name,
            protocol: handshake.protocol,
            channel,
            process,
            log,
            _socket_directory: socket_directory,
        })
    }

    /// Negotiated protocol version.
    #[must_use]
    pub const fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// Operating system process identifier of the provider, while it runs.
    #[must_use]
    pub fn process_identifier(&self) -> Option<u32> {
        self.process
            .child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .id()
    }

    async fn call<RequestMessage, ResponseMessage>(
        &self,
        rpc: RemoteProcedure,
        request: RequestMessage,
    ) -> Result<ResponseMessage>
    where
        RequestMessage: prost::Message + Send + Sync + 'static,
        ResponseMessage: prost::Message + Default + Send + Sync + 'static,
    {
        let path = rpc.path(self.protocol);
        unary(&self.channel, path, request).await.map_err(|status| {
            let transport_failure = matches!(
                status.code(),
                tonic::Code::Unavailable | tonic::Code::Unknown | tonic::Code::Internal
            );
            // The message comes from the provider and is displayed.
            let message = displayable_provider_message(status.message());
            let status = if transport_failure {
                tonic::Status::new(status.code(), format!("{message}{}", self.log.render()))
            } else {
                tonic::Status::new(status.code(), message)
            };
            InfrastructureError::RemoteProcedure {
                method: path.to_string(),
                status: Box::new(status),
            }
        })
    }

    /// Fetch the provider and managed resource schemas.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails or the provider reports errors.
    pub async fn schema(&self) -> Result<(ProviderSchema, Vec<Diagnostic>)> {
        match self.protocol {
            Protocol::Version5 => {
                let response: protocol::version5::GetProviderSchemaResponse = self
                    .call(RemoteProcedure::GetSchema, protocol::Empty {})
                    .await?;
                let diagnostics = response.diagnostics.clone();
                Ok((ProviderSchema::from_version5(response)?, diagnostics))
            }
            Protocol::Version6 => {
                let response: protocol::version6::GetProviderSchemaResponse = self
                    .call(RemoteProcedure::GetSchema, protocol::Empty {})
                    .await?;
                let diagnostics = response.diagnostics.clone();
                Ok((ProviderSchema::from_version6(response)?, diagnostics))
            }
        }
    }

    /// Validate provider configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn validate_provider_configuration(
        &self,
        configuration: Vec<u8>,
    ) -> Result<Vec<Diagnostic>> {
        let response: protocol::ValidateProviderConfigurationResponse = self
            .call(
                RemoteProcedure::ValidateProviderConfiguration,
                protocol::ValidateProviderConfigurationRequest {
                    configuration: Some(message_pack_value(configuration)),
                },
            )
            .await?;
        Ok(response.diagnostics)
    }

    /// Configure the provider.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn configure(&self, configuration: Vec<u8>) -> Result<Vec<Diagnostic>> {
        let response: protocol::DiagnosticsResponse = self
            .call(
                RemoteProcedure::Configure,
                protocol::ConfigureProviderRequest {
                    terraform_version: TERRAFORM_VERSION.to_string(),
                    configuration: Some(message_pack_value(configuration)),
                    client_capabilities: Some(client_capabilities()),
                },
            )
            .await?;
        Ok(response.diagnostics)
    }

    /// Validate a managed resource configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn validate_resource_configuration(
        &self,
        type_name: &str,
        configuration: Vec<u8>,
    ) -> Result<Vec<Diagnostic>> {
        let response: protocol::DiagnosticsResponse = self
            .call(
                RemoteProcedure::ValidateResourceConfiguration,
                protocol::ValidateResourceConfigurationRequest {
                    type_name: type_name.to_string(),
                    configuration: Some(message_pack_value(configuration)),
                    client_capabilities: Some(client_capabilities()),
                },
            )
            .await?;
        Ok(response.diagnostics)
    }

    /// Upgrade stored JSON state to the provider's current schema. The
    /// upgraded state comes back as the provider sent it, MessagePack or
    /// JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn upgrade_resource_state(
        &self,
        type_name: &str,
        version: i64,
        state_json: Vec<u8>,
    ) -> Result<protocol::UpgradeResourceStateResponse> {
        self.call(
            RemoteProcedure::UpgradeResourceState,
            protocol::UpgradeResourceStateRequest {
                type_name: type_name.to_string(),
                version,
                raw_state: Some(protocol::RawState {
                    json: state_json,
                    flatmap: std::collections::HashMap::new(),
                }),
            },
        )
        .await
    }

    /// Refresh a managed resource from the real world.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn read_resource(
        &self,
        type_name: &str,
        current_state: Vec<u8>,
        private: Vec<u8>,
    ) -> Result<protocol::ReadResourceResponse> {
        self.call(
            RemoteProcedure::ReadResource,
            protocol::ReadResourceRequest {
                type_name: type_name.to_string(),
                current_state: Some(message_pack_value(current_state)),
                private,
                client_capabilities: Some(client_capabilities()),
            },
        )
        .await
    }

    /// Plan a change to a managed resource.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn plan_resource_change(
        &self,
        request: PlanRequest<'_>,
    ) -> Result<protocol::PlanResourceChangeResponse> {
        self.call(
            RemoteProcedure::PlanResourceChange,
            protocol::PlanResourceChangeRequest {
                type_name: request.type_name.to_string(),
                prior_state: Some(message_pack_value(request.prior_state)),
                proposed_new_state: Some(message_pack_value(request.proposed_new_state)),
                configuration: Some(message_pack_value(request.configuration)),
                prior_private: request.prior_private,
                client_capabilities: Some(client_capabilities()),
            },
        )
        .await
    }

    /// Apply a planned change to a managed resource.
    ///
    /// # Errors
    ///
    /// Returns an error if the RPC fails.
    pub async fn apply_resource_change(
        &self,
        request: ApplyRequest<'_>,
    ) -> Result<protocol::ApplyResourceChangeResponse> {
        self.call(
            RemoteProcedure::ApplyResourceChange,
            protocol::ApplyResourceChangeRequest {
                type_name: request.type_name.to_string(),
                prior_state: Some(message_pack_value(request.prior_state)),
                planned_state: Some(message_pack_value(request.planned_state)),
                configuration: Some(message_pack_value(request.configuration)),
                planned_private: request.planned_private,
            },
        )
        .await
    }

    /// Ask the provider to stop gracefully, then terminate the process.
    pub async fn shutdown(self) {
        // Ask the provider to cancel in-flight work, then let go-plugin shut
        // down cleanly (it removes its socket), and only then kill it.
        if !self.process.has_exited() {
            self.process.request_stop().await;
            let controller = tokio::time::timeout(
                SHUTDOWN_STEP_TIMEOUT,
                unary::<_, protocol::Empty>(
                    &self.channel,
                    CONTROLLER_SHUTDOWN_PATH,
                    protocol::Empty {},
                ),
            )
            .await;
            if !matches!(controller, Ok(Ok(_))) {
                tracing::debug!(provider = %self.name, "provider controller shutdown did not complete");
            }
        }
        if !self.process.wait_for_exit(SHUTDOWN_STEP_TIMEOUT).await {
            self.process.kill();
            let _ = self.process.wait_for_exit(SHUTDOWN_STEP_TIMEOUT).await;
        }
    }
}

/// Issue one unary gRPC call with Terraform's message size limits.
async fn unary<RequestMessage, ResponseMessage>(
    channel: &Channel,
    path: &'static str,
    request: RequestMessage,
) -> std::result::Result<ResponseMessage, tonic::Status>
where
    RequestMessage: prost::Message + Send + Sync + 'static,
    ResponseMessage: prost::Message + Default + Send + Sync + 'static,
{
    let mut grpc = tonic::client::Grpc::new(channel.clone())
        .max_decoding_message_size(MAX_MESSAGE_BYTES)
        .max_encoding_message_size(MAX_MESSAGE_BYTES);
    grpc.ready().await.map_err(|error| {
        tonic::Status::unavailable(format!("provider connection not ready for {path}: {error}"))
    })?;
    let codec = tonic_prost::ProstCodec::<RequestMessage, ResponseMessage>::default();
    grpc.unary(
        tonic::Request::new(request),
        PathAndQuery::from_static(path),
        codec,
    )
    .await
    .map(tonic::Response::into_inner)
}

/// Arguments for [`ProviderClient::plan_resource_change`]. All values are
/// cty MessagePack encoded against the resource schema.
#[derive(Debug)]
pub struct PlanRequest<'request> {
    /// Resource type name.
    pub type_name: &'request str,
    /// Prior state (MessagePack `nil` for create).
    pub prior_state: Vec<u8>,
    /// Proposed new state (MessagePack `nil` for delete).
    pub proposed_new_state: Vec<u8>,
    /// Configuration (MessagePack `nil` for delete).
    pub configuration: Vec<u8>,
    /// Provider private data from prior state.
    pub prior_private: Vec<u8>,
}

/// Arguments for [`ProviderClient::apply_resource_change`].
#[derive(Debug)]
pub struct ApplyRequest<'request> {
    /// Resource type name.
    pub type_name: &'request str,
    /// Prior state.
    pub prior_state: Vec<u8>,
    /// Planned state returned by `PlanResourceChange`.
    pub planned_state: Vec<u8>,
    /// Configuration.
    pub configuration: Vec<u8>,
    /// Private data returned by `PlanResourceChange`.
    pub planned_private: Vec<u8>,
}

const fn message_pack_value(bytes: Vec<u8>) -> DynamicValue {
    DynamicValue {
        message_pack: bytes,
        json: Vec::new(),
    }
}

const fn client_capabilities() -> protocol::ClientCapabilities {
    protocol::ClientCapabilities {
        deferral_allowed: false,
        write_only_attributes_allowed: false,
    }
}

fn spawn_log_drain(
    name: String,
    standard_error: impl AsyncRead + Unpin + Send + 'static,
    log: ProviderLog,
) -> tokio::task::JoinHandle<()> {
    use tracing::instrument::WithSubscriber as _;
    tokio::spawn(
        async move {
            let mut reader = BufReader::new(standard_error);
            while let Ok(Some(line)) = read_bounded_line(&mut reader, MAXIMUM_LOG_LINE_BYTES).await
            {
                // Classify the raw line first: nearly all of a provider's
                // output is trace-level chatter that is discarded, and
                // redacting it would cost time for nothing.
                let serious = provider_log_is_serious(&line.text);
                // Provider text may contain resolved secrets. Only the redacted
                // ProviderLog/event path may emit it; tracing records metadata.
                tracing::debug!(provider = %name, serious, "provider emitted a log line");
                if !serious {
                    continue;
                }
                // Keep only printable text: a provider must not drive the
                // terminal cuenv writes to. Redact the raw text first:
                // stripping control characters can change a secret that
                // contains one.
                let printable = strip_control_characters(
                    redact_provider_log_line(line.text.trim_end()).as_str(),
                );
                let text = if line.truncated {
                    format!("{printable} [line truncated]")
                } else {
                    printable
                };
                log.record(text);
            }
        }
        .with_current_subscriber(),
    )
}

/// go-plugin forwards provider logs as hclog JSON lines with an `@level`;
/// only errors and warnings are serious. Anything that is not hclog JSON
/// (a Go panic and its stack, or raw output) is kept too.
fn provider_log_is_serious(line: &str) -> bool {
    if line.trim().is_empty() {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(serde_json::Value::Object(entry)) => entry
            .get("@level")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|level| matches!(level, "error" | "warn")),
        _ => true,
    }
}

/// One provider log line with secrets replaced.
///
/// A line of hclog JSON is parsed and each decoded string (and key) redacted
/// before the line is written again: Go's JSON encoder writes `&`, `<` and
/// `>` as `\u0026`, `\u003c` and `\u003e`, so a secret holding them does not
/// appear in the raw text. A line that is not a JSON object (a Go panic, raw
/// output, a line cut at the length limit) is redacted as plain text, which
/// also finds the escaped forms the registry knows.
fn redact_provider_log_line(line: &str) -> String {
    let Ok(mut entry @ serde_json::Value::Object(_)) = serde_json::from_str(line) else {
        return redact_provider_text(line);
    };
    if !redact_json_strings(&mut entry) {
        return redact_provider_text(line);
    }
    // The rewritten line is redacted once more as text, which catches a
    // secret that only the whole line, not one string, contains.
    redact_provider_text(&entry.to_string())
}

/// Redact every string inside `value` in place; whether any changed.
fn redact_json_strings(value: &mut serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => {
            let redacted = redact_provider_text(text);
            let changed = redacted != *text;
            *text = redacted;
            changed
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .fold(false, |changed, item| redact_json_strings(item) | changed),
        serde_json::Value::Object(map) => {
            let entries = std::mem::take(map);
            let mut changed = false;
            for (key, mut entry) in entries {
                changed |= redact_json_strings(&mut entry);
                let redacted_key = redact_provider_text(&key);
                changed |= redacted_key != key;
                map.insert(redacted_key, entry);
            }
            changed
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            false
        }
    }
}

/// One line read by [`read_bounded_line`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundedLine {
    /// The line, including its newline when present, cut at the limit.
    text: String,
    /// Whether bytes beyond the limit were discarded.
    truncated: bool,
}

/// Read one line, keeping at most `limit` bytes of it and discarding the
/// rest, or `None` at end of output. Memory stays bounded however long the
/// line is.
async fn read_bounded_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    limit: usize,
) -> std::io::Result<Option<BoundedLine>> {
    let mut line = Vec::new();
    let mut truncated = false;
    let mut read_anything = false;
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Ok(read_anything.then(|| BoundedLine {
                text: String::from_utf8_lossy(&line).into_owned(),
                truncated,
            }));
        }
        read_anything = true;
        let (consumed, finished) = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or((buffer.len(), false), |index| (index + 1, true));
        let room = limit.saturating_sub(line.len());
        let kept = consumed.min(room);
        line.extend_from_slice(&buffer[..kept]);
        truncated |= kept < consumed;
        reader.consume(consumed);
        if finished {
            return Ok(Some(BoundedLine {
                text: String::from_utf8_lossy(&line).into_owned(),
                truncated,
            }));
        }
    }
}

async fn connect(handshake: &Handshake) -> Result<Channel> {
    match handshake.network.as_str() {
        "unix" => connect_unix(&handshake.address).await,
        "tcp" => Endpoint::from_shared(format!("http://{}", handshake.address))
            .map_err(|error| {
                InfrastructureError::plugin(format!("invalid provider address: {error}"))
            })?
            .connect()
            .await
            .map_err(|error| {
                InfrastructureError::plugin(format!("failed to dial provider: {error}"))
            }),
        other => Err(InfrastructureError::plugin(format!(
            "unsupported provider network '{other}'"
        ))),
    }
}

#[cfg(unix)]
async fn connect_unix(address: &str) -> Result<Channel> {
    let socket = PathBuf::from(address);
    // The URI is ignored by the connector; tonic only needs a valid one.
    Endpoint::from_static("http://[::]:50051")
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(socket).await?;
                Ok::<_, std::io::Error>(TokioIo::new(stream))
            }
        }))
        .await
        .map_err(|error| {
            InfrastructureError::plugin(format!(
                "failed to dial provider socket {address}: {error}"
            ))
        })
}

#[cfg(not(unix))]
async fn connect_unix(address: &str) -> Result<Channel> {
    Err(InfrastructureError::plugin(format!(
        "unix socket providers are not supported on this platform ({address})"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn provider_environment_preserves_host_overlays_cuenv_and_withholds_token() {
        let cancellation = Cancellation::default();
        let host_path = std::env::var("PATH").unwrap();
        let variables = BTreeMap::from([
            ("PATH".into(), "/cuenv/provider/path".into()),
            ("TURSO_AUTH_TOKEN".into(), "withheld-secret".into()),
            ("CUENV_PROVIDER_ONLY".into(), "visible".into()),
        ]);
        let withheld = vec!["TURSO_AUTH_TOKEN".into()];
        let options = LaunchOptions {
            binary: &env_program(),
            withheld_environment_variables: &withheld,
            provider_environment_variables: &variables,
            cancellation: &cancellation,
        };
        let mut command = Command::new(options.binary);
        configure_provider_environment(&mut command, &options, Path::new("/tmp/provider-test"));
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        let entries = String::from_utf8(output.stdout).unwrap();
        assert!(
            entries
                .lines()
                .any(|line| line == "PATH=/cuenv/provider/path")
        );
        assert!(
            entries
                .lines()
                .any(|line| line == "CUENV_PROVIDER_ONLY=visible")
        );
        assert!(
            !entries
                .lines()
                .any(|line| line.starts_with("TURSO_AUTH_TOKEN="))
        );

        let empty = BTreeMap::new();
        let options = LaunchOptions {
            provider_environment_variables: &empty,
            withheld_environment_variables: &[],
            ..options
        };
        let mut command = Command::new(options.binary);
        configure_provider_environment(&mut command, &options, Path::new("/tmp/provider-test"));
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        let entries = String::from_utf8(output.stdout).unwrap();
        assert!(
            entries
                .lines()
                .any(|line| line == format!("PATH={host_path}"))
        );
    }

    /// The `env` program, found on the host's `PATH`: not every system has
    /// `/usr/bin/env` (a Nix build sandbox does not).
    #[cfg(unix)]
    fn env_program() -> PathBuf {
        std::env::var_os("PATH")
            .and_then(|path| {
                std::env::split_paths(&path)
                    .map(|directory| directory.join("env"))
                    .find(|candidate| {
                        use std::os::unix::fs::PermissionsExt;
                        candidate.metadata().is_ok_and(|metadata| {
                            metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                        })
                    })
            })
            .expect("an `env` program on PATH")
    }

    /// The environment a provider launched with these options would see.
    #[cfg(unix)]
    async fn provider_visible_environment(options: &LaunchOptions<'_>) -> BTreeMap<String, String> {
        let mut command = Command::new(env_program());
        configure_provider_environment(&mut command, options, Path::new("/tmp/provider-test"));
        let output = command.output().await.unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn withheld_names_never_remove_cuenvs_own_handshake_variables() {
        let cancellation = Cancellation::default();
        let withheld: Vec<String> = [
            "TMPDIR",
            "PLUGIN_UNIX_SOCKET_DIR",
            "PLUGIN_PROTOCOL_VERSIONS",
            MAGIC_COOKIE_KEY,
            "TURSO_AUTH_TOKEN",
        ]
        .map(String::from)
        .to_vec();
        let variables = BTreeMap::from([(MAGIC_COOKIE_KEY.to_string(), "project".to_string())]);
        let environment = provider_visible_environment(&LaunchOptions {
            binary: &env_program(),
            withheld_environment_variables: &withheld,
            provider_environment_variables: &variables,
            cancellation: &cancellation,
        })
        .await;
        assert_eq!(environment["TMPDIR"], "/tmp/provider-test");
        assert_eq!(environment["PLUGIN_UNIX_SOCKET_DIR"], "/tmp/provider-test");
        assert_eq!(environment["PLUGIN_PROTOCOL_VERSIONS"], "5,6");
        assert_eq!(environment[MAGIC_COOKIE_KEY], MAGIC_COOKIE_VALUE);
        assert!(!environment.contains_key("TURSO_AUTH_TOKEN"));
    }

    #[test]
    fn isolated_mode_withholds_everything_outside_the_allowlist() {
        let ambient = [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "TMPDIR",
            "HTTP_PROXY",
            "https_proxy",
            "NO_PROXY",
            "ALL_PROXY",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "AWS_SECRET_ACCESS_KEY",
            "OP_SERVICE_ACCOUNT_TOKEN",
            "GITHUB_TOKEN",
            "SHELL",
        ]
        .map(std::ffi::OsString::from);
        let withheld = isolated_withheld_names(ambient, &["GITHUB_TOKEN".to_string()]);
        assert_eq!(
            withheld,
            vec![
                "AWS_SECRET_ACCESS_KEY".to_string(),
                "OP_SERVICE_ACCOUNT_TOKEN".to_string(),
                "SHELL".to_string(),
            ],
            "the allowlist and the variables the project passes stay"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_isolated_provider_sees_only_the_allowlist_cuenvs_own_and_project_values() {
        let cancellation = Cancellation::default();
        let variables = BTreeMap::from([("PROJECT_VALUE".to_string(), "kept".to_string())]);
        let provided: Vec<String> = variables.keys().cloned().collect();
        let withheld =
            isolated_withheld_names(std::env::vars_os().map(|(name, _)| name), &provided);
        let environment = provider_visible_environment(&LaunchOptions {
            binary: &env_program(),
            withheld_environment_variables: &withheld,
            provider_environment_variables: &variables,
            cancellation: &cancellation,
        })
        .await;
        let allowed = |name: &str| {
            ISOLATED_INHERITED_ENVIRONMENT_VARIABLES.contains(&name)
                || [
                    "PROJECT_VALUE",
                    "TMPDIR",
                    "PLUGIN_UNIX_SOCKET_DIR",
                    "PLUGIN_PROTOCOL_VERSIONS",
                    MAGIC_COOKIE_KEY,
                ]
                .contains(&name)
        };
        let unexpected: Vec<_> = environment.keys().filter(|name| !allowed(name)).collect();
        assert!(unexpected.is_empty(), "inherited {unexpected:?}");
        assert_eq!(environment["PROJECT_VALUE"], "kept");
    }

    /// Every text the test redactor was asked to redact.
    static REDACTED_TEXTS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    fn mask_test_secret(text: &str) -> String {
        REDACTED_TEXTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(text.to_string());
        text.replace("SECRET\u{1b}VALUE", "*_*")
            .replace("p&ss<w>rd-GGGG", "*_*")
    }

    /// Run `lines` through the log drain; what it kept.
    async fn drained(lines: &str) -> String {
        install_log_redactor(mask_test_secret);
        let log = ProviderLog::default();
        spawn_log_drain(
            "provider".to_string(),
            std::io::Cursor::new(lines.as_bytes().to_vec()),
            log.clone(),
        )
        .await
        .unwrap();
        log.render()
    }

    #[tokio::test]
    async fn a_discarded_provider_log_line_is_never_redacted() {
        let marker = "TRACE-CHATTER-9f3a1c";
        let lines = format!(
            "{{\"@level\":\"trace\",\"@message\":\"{marker} one\"}}\n\
             {{\"@level\":\"debug\",\"@message\":\"{marker} two\"}}\n\
             {{\"@level\":\"info\",\"@message\":\"{marker} three\"}}\n\
             {{\"@level\":\"error\",\"@message\":\"kept SECRET\\u001bVALUE\"}}\n"
        );
        let rendered = drained(&lines).await;
        assert!(rendered.contains("kept"), "{rendered}");
        assert!(!rendered.contains(marker), "{rendered}");
        let seen = REDACTED_TEXTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|text| text.contains(marker))
            .count();
        assert_eq!(seen, 0, "trace, debug and info lines are not redacted");
    }

    #[tokio::test]
    async fn go_json_escapes_do_not_hide_a_secret_in_an_hclog_line() {
        // Go's encoding/json writes & < > as \u0026 \u003c \u003e.
        let line = "{\"@level\":\"error\",\"@message\":\"auth failed for token p\\u0026ss\\u003cw\\u003erd-GGGG\",\"@module\":\"provider\"}\n";
        let rendered = drained(line).await;
        assert!(!rendered.contains("GGGG"), "{rendered}");
        assert!(rendered.contains("auth failed for token *_*"), "{rendered}");
        assert!(rendered.contains("\"@module\":\"provider\""), "{rendered}");
    }

    #[test]
    fn a_json_line_without_a_secret_is_left_as_it_was() {
        install_log_redactor(mask_test_secret);
        let line = "{\"@level\":\"warn\",   \"@message\":\"plain\"}";
        assert_eq!(redact_provider_log_line(line), line);
        // Not an object: redacted as text.
        assert_eq!(redact_provider_log_line("[1, 2]"), "[1, 2]");
    }

    #[test]
    fn a_status_message_is_redacted_before_it_is_stripped() {
        install_log_redactor(mask_test_secret);
        // Stripping first would leave "SECRETVALUE", which the redactor does
        // not know; the order matters.
        let message = displayable_provider_message("rpc failed: SECRET\u{1b}VALUE\nnext line\u{7}");
        assert_eq!(message, "rpc failed: *_*\nnext line");
    }

    #[cfg(unix)]
    #[test]
    fn variables_with_names_that_are_not_unicode_never_reach_a_provider() {
        use std::os::unix::ffi::OsStringExt;
        let ambient = vec![
            (
                std::ffi::OsString::from("PATH"),
                std::ffi::OsString::from("/usr/bin"),
            ),
            (
                std::ffi::OsString::from_vec(b"SECRET_\xff_NAME".to_vec()),
                std::ffi::OsString::from("leaked"),
            ),
        ];
        let kept = listable_ambient_environment(ambient);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].0, "PATH");
    }

    #[tokio::test]
    async fn provider_log_lines_are_redacted_before_control_characters_are_stripped() {
        install_log_redactor(mask_test_secret);
        // The secret holds a control character: stripping it first would
        // leave "SECRETVALUE", which no longer matches the registered secret.
        let line = b"panic: token SECRET\x1bVALUE rejected\n".to_vec();
        let log = ProviderLog::default();
        spawn_log_drain(
            "provider".to_string(),
            std::io::Cursor::new(line),
            log.clone(),
        )
        .await
        .unwrap();
        let rendered = log.render();
        assert!(!rendered.contains("SECRET"), "{rendered}");
        assert!(!rendered.contains("VALUE"), "{rendered}");
        assert!(rendered.contains("token *_* rejected"), "{rendered}");
    }

    #[test]
    fn parses_protocol_5_unix_handshake() {
        let handshake = Handshake::parse("1|5|unix|/tmp/plugin1|grpc|\n").unwrap();
        assert_eq!(handshake.protocol, Protocol::Version5);
        assert_eq!(handshake.network, "unix");
        assert_eq!(handshake.address, "/tmp/plugin1");
    }

    #[test]
    fn parses_protocol_6_handshake_without_trailing_certificate_field() {
        let handshake = Handshake::parse("1|6|tcp|127.0.0.1:1234|grpc").unwrap();
        assert_eq!(handshake.protocol, Protocol::Version6);
        assert_eq!(handshake.network, "tcp");
    }

    #[test]
    fn rejects_netrpc_tls_and_unknown_versions() {
        assert!(Handshake::parse("1|5|unix|/tmp/plugin|netrpc|").is_err());
        assert!(Handshake::parse("1|6|unix|/tmp/plugin|grpc|MIIC...").is_err());
        assert!(Handshake::parse("1|4|unix|/tmp/plugin|grpc|").is_err());
        assert!(Handshake::parse("2|6|unix|/tmp/plugin|grpc|").is_err());
        assert!(Handshake::parse("garbage").is_err());
    }

    #[tokio::test]
    async fn bounded_lines_discard_what_exceeds_the_limit() {
        let input: &[u8] = b"short\nthis line is far too long\ntail";
        let mut reader = BufReader::new(input);
        let first = read_bounded_line(&mut reader, 8).await.unwrap().unwrap();
        assert_eq!(first.text, "short\n");
        assert!(!first.truncated);
        let second = read_bounded_line(&mut reader, 8).await.unwrap().unwrap();
        assert_eq!(second.text, "this lin");
        assert!(second.truncated);
        let third = read_bounded_line(&mut reader, 8).await.unwrap().unwrap();
        assert_eq!(third.text, "tail");
        assert!(!third.truncated);
        assert!(read_bounded_line(&mut reader, 8).await.unwrap().is_none());
    }

    #[test]
    fn only_errors_warnings_and_raw_output_are_serious() {
        assert!(provider_log_is_serious(
            r#"{"@level":"error","@message":"failed"}"#
        ));
        assert!(provider_log_is_serious(
            r#"{"@level":"warn","@message":"careful"}"#
        ));
        assert!(!provider_log_is_serious(
            r#"{"@level":"debug","@message":"state value"}"#
        ));
        assert!(!provider_log_is_serious(
            r#"{"@level":"trace","@message":"[ERROR] inside a trace line"}"#
        ));
        assert!(provider_log_is_serious("panic: runtime error"));
        assert!(provider_log_is_serious("goroutine 1 [running]:"));
        assert!(!provider_log_is_serious("   "));
    }

    #[cfg(unix)]
    #[test]
    fn socket_directory_is_private_from_creation() {
        use std::os::unix::fs::PermissionsExt;
        let cancellation = Cancellation::default();
        let directory = SocketDirectory::create(&cancellation).unwrap();
        let mode = std::fs::metadata(&directory.path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        assert_eq!(cancellation.socket_directory_count(), 1);
        let path = directory.path.clone();
        drop(directory);
        assert!(!path.exists());
        assert_eq!(cancellation.socket_directory_count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_forced_exit_removes_socket_directories_still_in_use() {
        let cancellation = Cancellation::default();
        let directory = SocketDirectory::create(&cancellation).unwrap();
        let path = directory.path.clone();
        cancellation.terminate_providers();
        assert!(!path.exists());
        drop(directory);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn killing_a_provider_kills_its_process_group() {
        // A stand-in provider that starts a grandchild in its own group.
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 300 & echo $!; wait"])
            .stdout(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut grandchild = String::new();
        output.read_line(&mut grandchild).await.unwrap();
        let grandchild = grandchild.trim().to_string();
        let process = ProviderProcess {
            name: "stand-in".into(),
            child: Mutex::new(child),
            connection: OnceLock::new(),
        };
        assert!(
            Path::new(&format!("/proc/{grandchild}")).exists() || cfg!(not(target_os = "linux"))
        );
        process.kill();
        assert!(process.wait_for_exit(Duration::from_secs(5)).await);
        #[cfg(target_os = "linux")]
        {
            // The grandchild is killed too (it may linger as a zombie of
            // init for a moment).
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let alive = || {
                std::fs::read_to_string(format!("/proc/{grandchild}/stat")).is_ok_and(|stat| {
                    !stat
                        .rsplit_once(')')
                        .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
                })
            };
            while alive() && std::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(!alive(), "grandchild {grandchild} survived");
        }
    }

    #[tokio::test]
    async fn provider_log_lines_lose_control_characters() {
        let log = ProviderLog::default();
        let output: &'static [u8] =
            b"panic: \x1b]0;owned\x07boom\n{\"@level\":\"debug\",\"@message\":\"quiet\"}\n";
        spawn_log_drain("stand-in".into(), output, log.clone())
            .await
            .unwrap();
        let rendered = log.render();
        assert!(!rendered.contains('\u{1b}'), "{rendered}");
        assert!(rendered.contains("panic: ]0;ownedboom"), "{rendered}");
        assert!(!rendered.contains("quiet"), "{rendered}");
    }

    #[tokio::test]
    async fn provider_log_text_never_reaches_tracing() {
        #[derive(Clone)]
        struct CapturedTracing(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write as _;
                write!(self.0, "{field}={value:?};").unwrap();
            }
        }
        impl tracing::Subscriber for CapturedTracing {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
            fn event(&self, event: &tracing::Event<'_>) {
                let mut fields = Fields(String::new());
                event.record(&mut fields);
                self.0.lock().unwrap().push(fields.0);
            }
        }
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let _guard = tracing::subscriber::set_default(CapturedTracing(captured.clone()));
        let output: &'static [u8] = b"panic: project-secret-123\n[DEBUG] project-secret-123\n";
        spawn_log_drain("stand-in".into(), output, ProviderLog::default())
            .await
            .unwrap();
        let events = captured.lock().unwrap();
        assert!(!events.is_empty());
        assert!(
            events
                .iter()
                .all(|event| !event.contains("project-secret-123"))
        );
    }

    #[test]
    fn rpc_paths_follow_protocol_naming() {
        assert_eq!(
            RemoteProcedure::GetSchema.path(Protocol::Version5),
            "/tfplugin5.Provider/GetSchema"
        );
        assert_eq!(
            RemoteProcedure::GetSchema.path(Protocol::Version6),
            "/tfplugin6.Provider/GetProviderSchema"
        );
        assert_eq!(
            RemoteProcedure::ValidateResourceConfiguration.path(Protocol::Version5),
            "/tfplugin5.Provider/ValidateResourceTypeConfig"
        );
        assert_eq!(
            RemoteProcedure::Stop.path(Protocol::Version6),
            "/tfplugin6.Provider/StopProvider"
        );
    }
}
