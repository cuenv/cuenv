//! Safe interface to the CUE evaluation bridge.

// Local type alias for internal use
use crate::Result;
use crate::error::CueEngineError as Error;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString, OsStr};
use std::marker::PhantomData;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

// Bridge error codes - keep in sync with Go side constants
// These match the constants defined in bridge.go
pub const ERROR_CODE_INVALID_INPUT: &str = "INVALID_INPUT";
pub const ERROR_CODE_LOAD_INSTANCE: &str = "LOAD_INSTANCE";
pub const ERROR_CODE_BUILD_VALUE: &str = "BUILD_VALUE";
pub const ERROR_CODE_ANALYSIS: &str = "ANALYSIS_FAILED";
const ERROR_CODE_ORDERED_JSON: &str = "ORDERED_JSON";
pub const ERROR_CODE_PANIC_RECOVER: &str = "PANIC_RECOVER";
const ERROR_CODE_JSON_MARSHAL: &str = "JSON_MARSHAL_ERROR";
const ERROR_CODE_REGISTRY_INIT: &str = "REGISTRY_INIT";
const ERROR_CODE_DEPENDENCY_RES: &str = "DEPENDENCY_RESOLUTION";
pub const BRIDGE_PROTOCOL_VERSION: &str = "bridge/1";

/// Environment variable that sets the module evaluation timeout, in whole seconds.
///
/// The timeout exists to stop a hung evaluation. The default is deliberately
/// generous because a cold evaluation that also fetches registry modules can
/// take several seconds on small CI runners.
pub const EVAL_TIMEOUT_ENV_VAR: &str = "CUENV_EVAL_TIMEOUT";

/// Module evaluation timeout used when [`EVAL_TIMEOUT_ENV_VAR`] is unset or empty.
pub const DEFAULT_MODULE_EVAL_TIMEOUT: Duration = Duration::from_secs(60);

/// Error response from the Go bridge
#[derive(Debug, Deserialize, Serialize)]
pub struct BridgeError {
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
}

/// Structured response envelope from the Go bridge
#[derive(Debug, Deserialize)]
pub struct BridgeEnvelope<'bridge_envelope> {
    pub version: String,
    #[serde(borrow)]
    pub ok: Option<&'bridge_envelope serde_json::value::RawValue>,
    pub error: Option<BridgeError>,
}

/// RAII wrapper for C strings returned from FFI
/// Ensures proper cleanup when the wrapper goes out of scope
///
/// This type is intentionally !Send and !Sync, keeping each owned bridge
/// allocation on the thread that received it.
pub struct CStringPtr {
    ptr: *mut c_char,
    // Marker to make this type !Send + !Sync
    _marker: PhantomData<*const ()>,
}

impl CStringPtr {
    /// Creates a new wrapper from a raw pointer
    ///
    /// # Safety
    ///
    /// `ptr` must be null or the original pointer to a live C allocation
    /// accepted by `cue_free_string` (`free`). The entire NUL-terminated
    /// string, including its terminator, must lie within that allocation and
    /// occupy fewer than `isize::MAX` bytes. The caller transfers exclusive
    /// ownership of the allocation to the wrapper, must not access or free
    /// it while the wrapper owns it, and must not use the pointer after the
    /// wrapper drops it.
    ///
    /// # FFI Contract
    ///
    /// This function expects that the Go side:
    /// - Returns either null or a valid C string pointer
    /// - Allocates C memory that must be freed with `cue_free_string`
    /// - Does not modify the memory after returning the pointer
    #[expect(
        unsafe_code,
        reason = "Required to take ownership of C strings returned by the Go bridge"
    )]
    pub const unsafe fn new(ptr: *mut c_char) -> Self {
        Self {
            ptr,
            _marker: PhantomData,
        }
    }

    /// Checks if the wrapped pointer is null
    #[must_use]
    pub const fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    /// Converts the C string to a Rust &str
    ///
    /// # Safety
    ///
    /// The wrapped pointer must be non-null and point to a readable,
    /// NUL-terminated C string contained in one allocation, including its
    /// terminator, and occupying fewer than `isize::MAX` bytes. Its memory must
    /// not be modified during the lifetime of the returned `&str`. The string
    /// need not be valid UTF-8; this method checks that before returning a `&str`.
    ///
    /// # Errors
    ///
    /// Returns an error if the C string contains invalid UTF-8
    ///
    /// # Panics
    ///
    /// In debug builds, a null pointer triggers a diagnostic assertion. Calling
    /// this method with a null pointer violates its safety requirements in all builds.
    #[expect(
        unsafe_code,
        reason = "Required to expose a borrowed str from the wrapped C string"
    )]
    pub unsafe fn to_str(&self) -> Result<&str> {
        debug_assert!(
            !self.is_null(),
            "Attempted to convert null pointer to string"
        );

        let cstr = {
            #[expect(
                unsafe_code,
                reason = "Required to borrow the validated C string pointer"
            )]
            // SAFETY: The caller guarantees a non-null, readable C string whose
            // contents remain unchanged for the lifetime of the returned borrow.
            unsafe {
                CStr::from_ptr(self.ptr)
            }
        };
        cstr.to_str().map_err(|e| {
            Error::ffi(
                "cue_eval_module",
                format!("failed to convert C string to UTF-8: {e}"),
            )
        })
    }
}

impl Drop for CStringPtr {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            #[expect(
                unsafe_code,
                reason = "Required to free strings allocated by the Go bridge"
            )]
            // SAFETY: `new` requires an exclusively owned C allocation that
            // can be freed by the bridge. Drop transfers that allocation once.
            unsafe {
                cue_free_string(self.ptr);
            }
        }
    }
}

// The marker keeps ownership of a bridge allocation confined to one thread.

// Real FFI for normal builds
#[cfg(not(docsrs))]
#[link(name = "cue_bridge")]
#[expect(unsafe_code, reason = "Required for Go bridge FFI declarations")]
unsafe extern "C" {
    fn cue_eval_file(file_path: *const c_char) -> *mut c_char;
    fn cue_eval_module(
        module_root: *const c_char,
        package_name: *const c_char,
        options_json: *const c_char,
    ) -> *mut c_char;
    fn cue_module_dependency_version(
        module_root: *const c_char,
        dependency_path: *const c_char,
    ) -> *mut c_char;
    fn cue_instance_sources(
        module_root: *const c_char,
        directory: *const c_char,
        package_name: *const c_char,
    ) -> *mut c_char;
    fn cue_free_string(s: *mut c_char);
    fn cue_bridge_version() -> *mut c_char;
}

// Stub FFI for documentation builds - these satisfy the compiler but panic if called
#[cfg(docsrs)]
unsafe fn cue_eval_file(_: *const c_char) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

#[cfg(docsrs)]
unsafe fn cue_eval_module(_: *const c_char, _: *const c_char, _: *const c_char) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

#[cfg(docsrs)]
unsafe fn cue_module_dependency_version(_: *const c_char, _: *const c_char) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

#[cfg(docsrs)]
unsafe fn cue_instance_sources(
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

#[cfg(docsrs)]
unsafe fn cue_free_string(_: *mut c_char) {}

#[cfg(docsrs)]
unsafe fn cue_bridge_version() -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

/// Options for module evaluation
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleEvalOptions {
    /// Extract source positions into separate `meta` map
    pub with_meta: bool,
    /// Extract reference paths for values that are CUE references (e.g., `dependsOn: [build]`)
    pub with_references: bool,
    /// true: cue eval ./... (recursive), false: cue eval . (current directory)
    pub recursive: bool,
    /// Filter to specific package name, None = all packages
    pub package_name: Option<String>,
    /// Directory to evaluate (for non-recursive), None = module root. With a
    /// package filter, this may also be a standalone directory without
    /// `cue.mod/module.cue`; imports that require a module then fail normally.
    /// Use this to evaluate a specific subdirectory without loading the entire module.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_dir: Option<String>,
    /// CUE paths that must exist and be fully concrete in every evaluated
    /// instance, written in CUE path syntax (`config`, `config.database`,
    /// `"quoted-label".items[0]`). Within them, undefined references, missing
    /// required fields and non-concrete values fail the instance instead of
    /// being exported as null; a path that does not exist fails the instance
    /// too. An empty or malformed path fails the whole evaluation as a
    /// configuration error. Paths not listed keep the lenient export.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub concrete_paths: Vec<String>,
    /// What happens to a loaded instance that cannot be evaluated (load,
    /// build, `concrete_paths` or export failure).
    #[serde(default)]
    pub instance_failures: InstanceFailures,
    /// Which packages are evaluated, and so how instances are keyed.
    #[serde(default)]
    pub package_scope: PackageScope,
    /// When not empty, export only these paths of each instance instead of
    /// the whole value. Paths use CUE path syntax with regular fields only
    /// (`name`, `config.database`, `"quoted-label".items`); list indices,
    /// definitions and hidden fields are a configuration error. The exported
    /// object keeps the nesting (`config.database` exports
    /// `{"config": {"database": …}}`), and a path an instance lacks is left
    /// out. Loading, building, `concrete_paths` and `instance_failures` are
    /// unchanged, so failures are still reported; only the export (and the
    /// memory it takes) shrinks. Use this for module-wide checks that read a
    /// few fields of every instance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub export_paths: Vec<String>,
    /// Paths (same syntax as `export_paths`) whose existence is reported per
    /// instance in [`ModuleResult::present`], without exporting their values.
    /// A path exists when the instance has a regular field there; a field
    /// that is only declared optional (`field?:`) does not exist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presence_paths: Vec<String>,
    /// Whether a recursive evaluation reports the directories it does not
    /// load.
    #[serde(default)]
    pub skipped_directories: SkippedDirectories,
    /// The top-level field that holds the task graph. Sequence items inside
    /// it get their hidden `_name` field injected before export so that
    /// output references resolve, and a projection (`export_paths`) that
    /// exports nothing below this field skips the injection.
    ///
    /// `None` keeps the behaviour of callers that predate this option: the
    /// field is `tasks`. That default is a compatibility shim, not a
    /// recommendation: callers should name the field themselves (the cuenv
    /// command line does). `Some("")` turns the injection off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_field: Option<String>,
    /// Other directories whose instance of the same named package is
    /// unified into each evaluated instance before `concrete_paths`
    /// validation and export, in order. This is CUE unification, so a
    /// conflicting concrete value fails the instance while a default
    /// (`*"a" | string`) yields to a concrete value from either side. A
    /// source without the package contributes nothing. Only a
    /// non-recursive evaluation of one named package accepts sources, and
    /// the target directory must itself hold the package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unify: Vec<UnifySource>,
}

/// A directory whose package instance [`ModuleEvalOptions::unify`] merges
/// into the evaluated instance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnifySource {
    /// Directory holding the package files.
    pub dir: String,
    /// Root that resolves the directory's imports. `None` treats the
    /// directory as its own root, so a standalone directory evaluates and
    /// module imports fail through the normal CUE error path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_root: Option<String>,
}

/// Whether a recursive evaluation reports the directories it leaves out.
///
/// A recursive evaluation follows the rules of CUE's `./...` pattern: below
/// the evaluated directory, directories whose name starts with `.` or `_`,
/// directories named `testdata`, and directories holding their own
/// `cue.mod` (nested modules) are not loaded, and neither are directories
/// that cannot be read. The evaluated directory itself is always loaded,
/// whatever its name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SkippedDirectories {
    /// Leave them out silently.
    #[default]
    Ignore,
    /// Leave them out and list each one that holds a `.cue` file (at any
    /// depth) in [`ModuleResult::skipped_directories`], so a caller that
    /// must see every instance can refuse to proceed. Only the topmost
    /// left-out directory of a tree is listed. Reporting walks into the
    /// left-out trees, which costs a directory walk of each.
    Report,
}

/// A directory a recursive evaluation did not load.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SkippedDirectory {
    /// Directory relative to the module root, with `/` separators.
    pub path: String,
    /// Why it was not loaded.
    pub reason: SkippedReason,
}

/// Why a recursive evaluation did not load a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum SkippedReason {
    /// Its name starts with `.`.
    Dot,
    /// Its name starts with `_`.
    Underscore,
    /// It is named `testdata`.
    Testdata,
    /// It holds its own `cue.mod`: another CUE module.
    NestedModule,
    /// It could not be read.
    Unreadable,
}

/// Which CUE packages an evaluation covers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PackageScope {
    /// Evaluate the package named by `package_name` (or the legacy
    /// parameter), or the single package of each directory when no name is
    /// given. Instances are keyed by their directory relative to the module
    /// root (`"."`, `"services/api"`).
    #[default]
    Named,
    /// Evaluate every package in every loaded directory, including
    /// directories holding several packages. Instances, `projects` and `meta`
    /// entries are keyed as `"<directory>:<package>"` (`".:app"`,
    /// `"services/api:worker"`); files without a package clause appear under
    /// CUE's anonymous package as `"<directory>:_"`. CUE package names are
    /// identifiers, so a key splits unambiguously at its last `:`. `meta`
    /// keys are `"<instance key>/<field path>"`. These keys are not
    /// directory paths: do not hand them to consumers that expect the
    /// directory keys of [`PackageScope::Named`] (such as
    /// `cuenv_core::ModuleEvaluation::from_raw`). No package name may be
    /// given: both `package_name` and the legacy parameter must be empty, or
    /// evaluation fails with a configuration error.
    All,
}

/// Policy for loaded instances that cannot be evaluated.
///
/// Instances excluded by the package filter are never failures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InstanceFailures {
    /// Leave failed instances out of the result as long as another instance
    /// succeeds; evaluation fails only when no instance succeeds.
    #[default]
    Skip,
    /// Fail the whole evaluation when any instance fails, with an error
    /// listing every failed instance as `<key>: <errors>`, where each CUE
    /// error carries its field path and file positions relative to the
    /// module root. The key is the instance key (see [`PackageScope`]); a
    /// failure the loader reports before it knows the package is keyed by
    /// its directory alone, or by the load pattern (`./...`) when it has no
    /// directory, so under [`PackageScope::All`] a bare directory in the
    /// message is not a package key. Use this where a partial result would
    /// be unsafe, for example a check that must see every instance.
    Fail,
}

/// Source location metadata for a single field
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldMeta {
    /// Directory containing the file (relative to module root)
    #[serde(default)]
    pub directory: String,
    /// Filename where the field is defined
    #[serde(default)]
    pub filename: String,
    /// Line number in the file
    #[serde(default)]
    pub line: usize,
    /// Directory containing the original value definition (relative to module root)
    #[serde(default, rename = "definitionDirectory")]
    pub definition_directory: String,
    /// Filename where the original value is defined
    #[serde(default, rename = "definitionFilename")]
    pub definition_filename: String,
    /// Line number where the original value is defined
    #[serde(default, rename = "definitionLine")]
    pub definition_line: usize,
    /// If this value is a CUE reference, the path it refers to (e.g., "tasks.build")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// Result of evaluating an entire CUE module
#[derive(Debug, Default, Deserialize)]
pub struct ModuleResult {
    /// Map of relative path to evaluated JSON value
    pub instances: std::collections::HashMap<String, serde_json::Value>,
    /// Paths of instances that declare a `name` field. The engine detects
    /// the field only; it does not unify instances with `schema.#Project`.
    #[serde(default)]
    pub projects: Vec<String>,
    /// Map of "path/field" to source location (only populated when `with_meta`: true)
    #[serde(default)]
    pub meta: std::collections::HashMap<String, FieldMeta>,
    /// For each exported instance key, the entries of
    /// [`ModuleEvalOptions::presence_paths`] that exist in it, as written in
    /// the options. Empty when no presence paths were requested.
    #[serde(default)]
    pub present: std::collections::HashMap<String, Vec<String>>,
    /// Directories a recursive evaluation did not load although they hold
    /// CUE files, sorted by path. Only filled with
    /// [`SkippedDirectories::Report`].
    #[serde(default, rename = "skippedDirectories")]
    pub skipped_directories: Vec<SkippedDirectory>,
}

/// CUE module dependency metadata extracted from `cue.mod/module.cue`.
#[derive(Debug, Deserialize)]
pub struct ModuleDependencyVersion {
    /// The dependency version, if present.
    pub version: Option<String>,
}

/// The files a CUE instance is built from.
#[derive(Debug, Deserialize)]
pub struct InstanceSources {
    /// Paths relative to the module root, sorted, with `/` separators: the
    /// instance's own files (including same-package files of ancestor
    /// directories) and the files of every package it imports, transitively,
    /// that lives inside the module.
    pub files: Vec<String>,
    /// What the `@embed` attributes of those files read, relative to the
    /// module root and sorted: a path for `file=`, a glob for `glob=`. They
    /// are read from the source, so a file a glob would match is covered
    /// whether or not it exists.
    #[serde(default)]
    pub embeds: Vec<String>,
}

struct ModuleEvalWorker {
    c_module_root: CString,
    c_package: CString,
    c_options: CString,
    module_root: PathBuf,
    span: tracing::Span,
}

/// Evaluates CUE instances in a module and returns results with optional source metadata
///
/// This function evaluates CUE files in a module using native CUE loading patterns:
/// - `recursive: true` → equivalent to `cue eval ./...`
/// - `recursive: false` → equivalent to `cue eval .`
///
/// # Arguments
/// * `module_root` - Path to the CUE module root (directory containing
///   `cue.mod/`), or the exact standalone directory when a package-filtered
///   `target_dir` evaluation is requested
/// * `package_name` - Name of the CUE package to evaluate (legacy parameter, prefer using `options.package_name`)
/// * `options` - Evaluation options:
///   - `with_meta`: Extract source positions into separate `meta` map
///   - `with_references`: Extract CUE reference paths (e.g., for `dependsOn: [build]`, records that `dependsOn[0]` refers to `tasks.build`)
///   - `recursive`: Evaluate entire module tree (./...) or just current directory (.)
///   - `package_name`: Filter to specific package (takes precedence over legacy parameter)
///   - `concrete_paths`: CUE paths that must exist and be fully concrete
///   - `instance_failures`: Skip failed instances (default) or fail the evaluation
///   - `package_scope`: The named package (default) or every package, keyed
///     as `"<directory>:<package>"`
///   - `export_paths`: Export only these fields of each instance
///   - `presence_paths`: Report which of these fields exist in each instance
///   - `skipped_directories`: Report the directories a recursive evaluation
///     did not load
///
/// A recursive evaluation loads the evaluated directory whatever its name,
/// and below it follows CUE's `./...` rules (see [`SkippedDirectories`]).
/// Each instance is built in its own CUE context and exported before the
/// next is built, so memory does not grow with the whole module.
///
/// # Returns
/// A `ModuleResult` containing:
/// - `instances`: Map of relative paths to their evaluated JSON values
/// - `meta`: Map of "path/field" to source locations and reference paths (when `with_meta` or `with_references` is true)
///
/// # Errors
/// Returns an error if:
/// - The module root path is invalid
/// - The CUE module cannot be loaded
/// - All CUE instances fail evaluation
/// - Any instance fails evaluation with [`InstanceFailures::Fail`]; the
///   message lists every failed instance by its path relative to the module
///   root
/// - A `concrete_paths` entry is empty or not a valid CUE path, or
///   [`PackageScope::All`] is combined with a package name
///   ([`crate::CueEngineError::Configuration`])
///
/// A package-filtered evaluation returns an empty `instances` map when the
/// exact target directory has no matching package. Syntax and build errors
/// remain errors even when the requested package is absent.
#[tracing::instrument(
    name = "evaluate_module",
    fields(
        module_root = %module_root.display(),
        package_name = package_name,
        operation_id = %uuid::Uuid::new_v4(),
    ),
    level = "info",
    skip(options)
)]
pub fn evaluate_module(
    module_root: &Path,
    package_name: &str,
    options: Option<&ModuleEvalOptions>,
) -> Result<ModuleResult> {
    tracing::info!("Starting module-wide CUE evaluation");
    let start_time = Instant::now();
    let timeout = evaluation_timeout()?;

    let c_module_root = path_to_cstring(module_root, "cue_eval_module", "module root")?;
    let c_package = str_to_cstring(package_name, "cue_eval_module", "package name")?;
    let c_options = options_to_cstring(options)?;
    let worker = ModuleEvalWorker {
        c_module_root,
        c_package,
        c_options,
        module_root: module_root.to_path_buf(),
        span: tracing::Span::current(),
    };

    let rx = spawn_module_eval_worker(worker);
    let module_result = receive_module_eval_result(
        &rx,
        &ModuleEvalWait {
            module_root,
            package_name,
            timeout,
        },
    )?;
    log_module_eval_success(&module_result, start_time);

    Ok(module_result)
}

/// Read the validated evaluation budget from [`EVAL_TIMEOUT_ENV_VAR`].
///
/// Unset, empty or whitespace-only values select [`DEFAULT_MODULE_EVAL_TIMEOUT`].
/// This only returns the configured budget; it does not interrupt a running CUE
/// evaluation or make synchronous frozen-instance operations cancellable.
///
/// # Errors
/// Returns a configuration error unless a nonempty value is a positive whole
/// number of seconds.
pub fn evaluation_timeout() -> Result<Duration> {
    parse_module_eval_timeout(std::env::var_os(EVAL_TIMEOUT_ENV_VAR).as_deref())
}

/// Parses a module evaluation timeout value.
///
/// An unset, empty, or whitespace-only value selects
/// [`DEFAULT_MODULE_EVAL_TIMEOUT`]. Any other value must be a positive whole
/// number of seconds.
fn parse_module_eval_timeout(value: Option<&OsStr>) -> Result<Duration> {
    let Some(value) = value else {
        return Ok(DEFAULT_MODULE_EVAL_TIMEOUT);
    };
    let invalid = || {
        Error::configuration(format!(
            "Invalid {EVAL_TIMEOUT_ENV_VAR} value \"{}\": expected a positive whole number of seconds, such as {EVAL_TIMEOUT_ENV_VAR}=120",
            value.display()
        ))
    };
    let text = value.to_str().ok_or_else(invalid)?.trim();
    if text.is_empty() {
        return Ok(DEFAULT_MODULE_EVAL_TIMEOUT);
    }
    match text.parse::<u64>() {
        Ok(0) | Err(_) => Err(invalid()),
        Ok(seconds) => Ok(Duration::from_secs(seconds)),
    }
}

fn spawn_module_eval_worker(worker: ModuleEvalWorker) -> Receiver<Result<ModuleResult>> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<Result<ModuleResult>>(1);

    std::thread::spawn(move || {
        let result = worker.run();
        let _ = tx.send(result);
    });

    rx
}

impl ModuleEvalWorker {
    fn run(self) -> Result<ModuleResult> {
        let _entered = self.span.enter();
        let json_str = call_ffi_eval_module(&self.c_module_root, &self.c_package, &self.c_options)?;
        let envelope = parse_bridge_envelope(&json_str)?;
        process_bridge_response(envelope, &self.module_root)
    }
}

struct ModuleEvalWait<'module_eval_wait> {
    module_root: &'module_eval_wait Path,
    package_name: &'module_eval_wait str,
    timeout: Duration,
}

fn receive_module_eval_result(
    rx: &Receiver<Result<ModuleResult>>,
    wait: &ModuleEvalWait<'_>,
) -> Result<ModuleResult> {
    match rx.recv_timeout(wait.timeout) {
        Ok(inner) => inner,
        Err(RecvTimeoutError::Timeout) => {
            tracing::error!(
                timeout_secs = wait.timeout.as_secs(),
                module_root = %wait.module_root.display(),
                package_name = wait.package_name,
                "CUE evaluation timed out"
            );
            Err(Error::ffi(
                "cue_eval_module",
                format!(
                    "CUE evaluation timed out after {}s for module {}; set {EVAL_TIMEOUT_ENV_VAR} to a larger number of seconds to allow more time",
                    wait.timeout.as_secs(),
                    wait.module_root.display()
                ),
            ))
        }
        Err(RecvTimeoutError::Disconnected) => Err(Error::ffi(
            "cue_eval_module",
            "CUE evaluation worker thread disconnected unexpectedly".to_string(),
        )),
    }
}

fn log_module_eval_success(module_result: &ModuleResult, start_time: Instant) {
    tracing::info!(
        total_duration_ms = start_time.elapsed().as_millis(),
        instance_count = module_result.instances.len(),
        "Module evaluation completed successfully"
    );
}

/// Convert a path to a `CString` for FFI.
fn path_to_cstring(path: &Path, fn_name: &'static str, param_name: &str) -> Result<CString> {
    let Some(path_str) = path.to_str() else {
        tracing::error!("{} path is not valid UTF-8: {:?}", param_name, path);
        return Err(Error::configuration(format!(
            "Invalid {param_name} path: not UTF-8"
        )));
    };
    str_to_cstring(path_str, fn_name, param_name)
}

/// Convert a string to a `CString` for FFI.
fn str_to_cstring(s: &str, fn_name: &'static str, param_name: &str) -> Result<CString> {
    CString::new(s).map_err(|e| {
        tracing::error!("Failed to convert {} to C string: {}", param_name, e);
        Error::ffi(fn_name, format!("Invalid {param_name}: {e}"))
    })
}

/// Serialize options to JSON `CString` for FFI.
fn options_to_cstring(options: Option<&ModuleEvalOptions>) -> Result<CString> {
    let options_json = options.map_or_else(
        || "{}".to_string(),
        |o| serde_json::to_string(o).unwrap_or_else(|_| "{}".to_string()),
    );
    str_to_cstring(&options_json, "cue_eval_module", "options")
}

/// Call the FFI function and return the JSON string result.
fn call_ffi_eval_module(
    c_module_root: &CString,
    c_package: &CString,
    c_options: &CString,
) -> Result<String> {
    tracing::debug!("Calling FFI function cue_eval_module");
    let ffi_start = Instant::now();

    let result_ptr = {
        #[expect(
            unsafe_code,
            reason = "Required to call the Go CUE module evaluation bridge"
        )]
        // SAFETY: cue_eval_module is an FFI function that:
        // - Takes three valid C string pointers (guaranteed by CString::as_ptr())
        // - Returns either null or a valid pointer to a C string
        // - The returned pointer must be freed with cue_free_string
        unsafe {
            cue_eval_module(
                c_module_root.as_ptr(),
                c_package.as_ptr(),
                c_options.as_ptr(),
            )
        }
    };

    tracing::debug!(
        ffi_duration_ms = ffi_start.elapsed().as_millis(),
        "FFI call completed"
    );

    let result = bridge_owned_c_string(result_ptr);
    extract_ffi_string_with_null_message(
        &result,
        "cue_eval_module",
        "Module evaluation returned null",
    )
}

/// Parse the JSON envelope from the Go bridge.
fn parse_bridge_envelope(json_str: &str) -> Result<BridgeEnvelope<'_>> {
    let envelope: BridgeEnvelope = serde_json::from_str(json_str).map_err(|e| {
        tracing::error!(
            response_bytes = json_str.len(),
            "Failed to parse JSON envelope from Go bridge"
        );
        Error::ffi(
            "cue_eval_module",
            format!("Invalid JSON envelope from Go bridge: {e}"),
        )
    })?;

    validate_bridge_protocol(&envelope)?;
    Ok(envelope)
}

fn validate_bridge_protocol(envelope: &BridgeEnvelope<'_>) -> Result<()> {
    if envelope.version == BRIDGE_PROTOCOL_VERSION {
        return Ok(());
    }

    Err(Error::ffi(
        "cue_eval_module",
        format!(
            "Unsupported CUE bridge protocol version {}; expected {}",
            envelope.version, BRIDGE_PROTOCOL_VERSION
        ),
    ))
}

/// Process the bridge response and return the module result.
fn process_bridge_response(envelope: BridgeEnvelope, module_root: &Path) -> Result<ModuleResult> {
    if let Some(bridge_error) = envelope.error {
        return Err(handle_bridge_error(bridge_error, module_root));
    }

    let json_data = envelope
        .ok
        .map(|raw| raw.get().to_string())
        .ok_or_else(|| {
            tracing::error!("Bridge envelope has neither 'ok' nor 'error' field");
            Error::ffi(
                "cue_eval_module",
                "Invalid bridge response: missing both 'ok' and 'error' fields".to_string(),
            )
        })?;

    parse_module_result(&json_data)
}

/// Handle a bridge error and convert it to our error type.
fn handle_bridge_error(bridge_error: BridgeError, module_root: &Path) -> Error {
    tracing::error!(error_code = bridge_error.code, "Module evaluation failed");

    // A hint explains how to call the bridge correctly (option names and
    // values), so it belongs with configuration errors only. Evaluation
    // errors carry every CUE error with its position in the message; their
    // hints are not shown to the end user. Value-bearing diagnostics must never
    // enter raw tracing sinks, which do not apply application redaction.
    let with_hint = |bridge_error: BridgeError| {
        bridge_error
            .hint
            .map(|hint| format!("{}. {hint}", bridge_error.message))
            .unwrap_or(bridge_error.message)
    };

    match bridge_error.code.as_str() {
        ERROR_CODE_INVALID_INPUT | ERROR_CODE_REGISTRY_INIT => {
            Error::configuration(with_hint(bridge_error))
        }
        ERROR_CODE_LOAD_INSTANCE | ERROR_CODE_BUILD_VALUE | ERROR_CODE_DEPENDENCY_RES => {
            Error::cue_parse(module_root, bridge_error.message)
        }
        ERROR_CODE_ORDERED_JSON | ERROR_CODE_PANIC_RECOVER | ERROR_CODE_JSON_MARSHAL => {
            Error::ffi("cue_eval_module", with_hint(bridge_error))
        }
        _ => Error::ffi("cue_eval_module", with_hint(bridge_error)),
    }
}

/// Parse the module result from JSON.
fn parse_module_result(json_data: &str) -> Result<ModuleResult> {
    serde_json::from_str(json_data).map_err(|e| {
        tracing::error!(
            response_bytes = json_data.len(),
            "Failed to parse module result"
        );
        Error::ffi(
            "cue_eval_module",
            format!("Failed to parse module result: {e}"),
        )
    })
}

/// Extract a string from an FFI result wrapper.
pub fn extract_ffi_string(wrapper: &CStringPtr, fn_name: &'static str) -> Result<String> {
    extract_ffi_string_with_null_message(wrapper, fn_name, &format!("{fn_name} returned null"))
}

fn extract_ffi_string_with_null_message(
    wrapper: &CStringPtr,
    fn_name: &'static str,
    null_message: &str,
) -> Result<String> {
    if wrapper.is_null() {
        tracing::error!("{} returned null pointer", fn_name);
        return Err(Error::ffi(fn_name, null_message.to_string()));
    }

    {
        #[expect(
            unsafe_code,
            reason = "Required to read a non-null bridge-owned C string"
        )]
        // SAFETY: `new` requires a live, immutable C string and the null case was
        // rejected above. The returned borrow cannot outlive `wrapper`.
        unsafe {
            wrapper.to_str()
        }
    }
    .map(String::from)
}

pub fn bridge_owned_c_string(ptr: *mut c_char) -> CStringPtr {
    #[expect(
        unsafe_code,
        reason = "Required to wrap bridge-owned C string pointers for RAII cleanup"
    )]
    // SAFETY: All callers pass a null pointer or a C allocation returned by
    // the bridge, which transfers ownership without retaining a mutable alias.
    unsafe {
        CStringPtr::new(ptr)
    }
}

/// Gets the bridge version information from the Go side
///
/// This function returns version information about the Go FFI bridge,
/// including the protocol version and Go runtime version.
///
/// # Errors
///
/// Returns an error if:
/// - The FFI call fails
/// - The returned string is not valid UTF-8
///
/// # Returns
/// String containing bridge version information (e.g., "bridge/1 (Go go1.21.1)")
pub fn get_bridge_version() -> Result<String> {
    tracing::debug!("Getting bridge version information");

    let version_ptr = {
        #[expect(
            unsafe_code,
            reason = "Required to call the Go bridge version function"
        )]
        // SAFETY: cue_bridge_version is an FFI function that:
        // - Takes no parameters
        // - Returns either null or a valid pointer to a C string
        // - The returned pointer must be freed with cue_free_string
        unsafe {
            cue_bridge_version()
        }
    };

    let version_wrapper = bridge_owned_c_string(version_ptr);

    let bridge_version = extract_ffi_string(&version_wrapper, "cue_bridge_version")?;

    tracing::info!(bridge_version = bridge_version, "Retrieved bridge version");
    Ok(bridge_version)
}

fn process_bridge_json<T: DeserializeOwned>(
    json_str: &str,
    module_root: &Path,
    function_name: &'static str,
) -> Result<T> {
    let envelope = parse_bridge_envelope(json_str).map_err(|err| match err {
        Error::Ffi { message, .. } => Error::ffi(function_name, message),
        err => err,
    })?;
    if let Some(bridge_error) = envelope.error {
        return match handle_bridge_error(bridge_error, module_root) {
            Error::Ffi { message, .. } => Err(Error::ffi(function_name, message)),
            err => Err(err),
        };
    }

    let json_data = envelope
        .ok
        .map(|raw| raw.get().to_string())
        .ok_or_else(|| {
            Error::ffi(
                function_name,
                "Invalid bridge response: missing both 'ok' and 'error' fields".to_string(),
            )
        })?;

    serde_json::from_str(&json_data).map_err(|e| {
        Error::ffi(
            function_name,
            format!("Failed to parse bridge response payload: {e}"),
        )
    })
}

/// Evaluate one CUE file using its original module and imports, without
/// unifying other files in the same directory or ancestor directories.
///
/// # Errors
///
/// Returns an error if the file cannot be loaded, evaluated, or exported.
pub fn evaluate_cue_file(file_path: &Path) -> Result<serde_json::Value> {
    const FUNCTION_NAME: &str = "cue_eval_file";
    let path = path_to_cstring(file_path, FUNCTION_NAME, "file path")?;
    let timeout = evaluation_timeout()?;
    let owned_path = file_path.to_path_buf();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result_ptr = {
            #[expect(unsafe_code, reason = "Required to call the Go file-evaluation bridge")]
            // SAFETY: the path is a valid C string and the returned string is
            // owned by the caller and released by `bridge_owned_c_string`.
            unsafe {
                cue_eval_file(path.as_ptr())
            }
        };
        let result = bridge_owned_c_string(result_ptr);
        let evaluated = extract_ffi_string(&result, FUNCTION_NAME)
            .and_then(|json| process_bridge_json(&json, &owned_path, FUNCTION_NAME));
        let _ = sender.send(evaluated);
    });
    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(Error::ffi(
            FUNCTION_NAME,
            format!(
                "CUE file evaluation timed out after {}s for {}; set {EVAL_TIMEOUT_ENV_VAR} to a larger number of seconds to allow more time",
                timeout.as_secs(),
                file_path.display()
            ),
        )),
        Err(RecvTimeoutError::Disconnected) => Err(Error::ffi(
            FUNCTION_NAME,
            "CUE file evaluation worker thread disconnected unexpectedly".to_string(),
        )),
    }
}

/// Read dependency version metadata from `cue.mod/module.cue`.
///
/// # Errors
///
/// Returns an error if the module file cannot be read or parsed.
pub fn module_dependency_version(
    module_root: &Path,
    dependency_path: &str,
) -> Result<ModuleDependencyVersion> {
    const FUNCTION_NAME: &str = "cue_module_dependency_version";
    let c_module_root = path_to_cstring(module_root, FUNCTION_NAME, "module root")?;
    let c_dependency_path = str_to_cstring(dependency_path, FUNCTION_NAME, "dependency path")?;

    let result_ptr = {
        #[expect(
            unsafe_code,
            reason = "Required to call the Go module-dependency bridge"
        )]
        // SAFETY: cue_module_dependency_version takes valid C strings and returns a
        // heap-allocated C string owned by the caller.
        unsafe {
            cue_module_dependency_version(c_module_root.as_ptr(), c_dependency_path.as_ptr())
        }
    };
    let result = bridge_owned_c_string(result_ptr);
    let json_str = extract_ffi_string(&result, FUNCTION_NAME)?;
    process_bridge_json(&json_str, module_root, FUNCTION_NAME)
}

/// List the files the CUE instance of `package_name` in `directory` is built
/// from. The instance is loaded but neither built nor evaluated.
///
/// # Errors
///
/// Returns an error if an argument is not a valid C string or the instance
/// cannot be loaded.
pub fn instance_sources(
    module_root: &Path,
    directory: &Path,
    package_name: &str,
) -> Result<InstanceSources> {
    const FUNCTION_NAME: &str = "cue_instance_sources";
    let c_module_root = path_to_cstring(module_root, FUNCTION_NAME, "module root")?;
    let c_directory = path_to_cstring(directory, FUNCTION_NAME, "directory")?;
    let c_package_name = str_to_cstring(package_name, FUNCTION_NAME, "package name")?;

    let result_ptr = {
        #[expect(
            unsafe_code,
            reason = "Required to call the Go instance-sources bridge"
        )]
        // SAFETY: cue_instance_sources takes valid C strings and returns a
        // heap-allocated C string owned by the caller.
        unsafe {
            cue_instance_sources(
                c_module_root.as_ptr(),
                c_directory.as_ptr(),
                c_package_name.as_ptr(),
            )
        }
    };
    let result = bridge_owned_c_string(result_ptr);
    let json_str = extract_ffi_string(&result, FUNCTION_NAME)?;
    process_bridge_json(&json_str, module_root, FUNCTION_NAME)
}

/// Convenience wrapper around `evaluate_module` for single-directory evaluation.
///
/// The nearest ancestor containing `cue.mod/module.cue` supplies the module
/// root for imports. If there is no module, the requested directory is
/// evaluated as a standalone package. Only that directory is evaluated.
///
/// # Arguments
/// * `dir_path` - Directory containing the CUE files
/// * `package_name` - Name of the CUE package to evaluate
///
/// # Returns
/// JSON string containing the evaluated CUE configuration
///
/// # Errors
///
/// Returns an error if:
/// - The requested directory or its module root cannot be accessed
/// - The CUE module evaluation fails (see [`evaluate_module`])
/// - No CUE instance is found in the specified directory
#[tracing::instrument(
    name = "evaluate_cue_package",
    fields(
        dir_path = %dir_path.display(),
        package_name = package_name,
    ),
    level = "info"
)]
pub fn evaluate_cue_package(dir_path: &Path, package_name: &str) -> Result<String> {
    // Validate before canonicalizing so interior NULs and non-UTF-8 paths are
    // rejected consistently with the bridge's other path arguments.
    path_to_cstring(dir_path, "cue_eval_module", "directory")?;
    let directory = std::fs::canonicalize(dir_path).map_err(|error| {
        Error::configuration(format!(
            "Cannot resolve CUE package directory {}: {error}",
            dir_path.display()
        ))
    })?;
    if !directory.is_dir() {
        return Err(Error::configuration(format!(
            "CUE package path is not a directory: {}",
            directory.display()
        )));
    }

    let mut module_root = directory.as_path();
    for ancestor in directory.ancestors() {
        let module_file = ancestor.join("cue.mod/module.cue");
        match std::fs::metadata(&module_file) {
            Ok(metadata) if metadata.is_file() => {
                module_root = ancestor;
                break;
            }
            Ok(_) => {
                return Err(Error::configuration(format!(
                    "CUE module file is not a regular file: {}",
                    module_file.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::configuration(format!(
                    "Cannot inspect CUE module file {}: {error}",
                    module_file.display()
                )));
            }
        }
    }
    let directory_string = directory.to_str().ok_or_else(|| {
        Error::configuration("Invalid resolved CUE package directory path: not UTF-8")
    })?;
    let relative = directory.strip_prefix(module_root).map_err(|error| {
        Error::configuration(format!(
            "Cannot locate CUE package within its module: {error}"
        ))
    })?;
    let instance_key = if relative.as_os_str().is_empty() {
        "."
    } else {
        relative.to_str().ok_or_else(|| {
            Error::configuration("Invalid relative CUE package directory path: not UTF-8")
        })?
    };
    let options = ModuleEvalOptions {
        with_meta: false,
        with_references: false,
        recursive: false,
        package_name: None,
        target_dir: Some(directory_string.to_string()),
        concrete_paths: Vec::new(),
        instance_failures: InstanceFailures::Fail,
        package_scope: PackageScope::Named,
        export_paths: Vec::new(),
        presence_paths: Vec::new(),
        skipped_directories: SkippedDirectories::Ignore,
        task_field: None,
        unify: Vec::new(),
    };

    let result = evaluate_module(module_root, package_name, Some(&options))?;

    let instance = result.instances.get(instance_key).ok_or_else(|| {
        Error::configuration(format!(
            "No CUE instance found in directory: {}",
            dir_path.display()
        ))
    })?;

    Ok(instance.to_string())
}

/// Evaluates a CUE package and returns the result as a typed struct
///
/// This is a convenience wrapper around `evaluate_cue_package` that deserializes
/// the JSON result into a strongly-typed Rust struct.
///
/// # Type Parameters
/// * `T` - The type to deserialize into. Must implement `serde::de::DeserializeOwned`
///
/// # Errors
///
/// Returns an error if:
/// - The CUE evaluation fails (see `evaluate_cue_package` for details)
/// - The JSON cannot be deserialized into the target type
///
/// # Arguments
/// * `dir_path` - Directory containing the CUE files
/// * `package_name` - Name of the CUE package to evaluate
///
/// # Returns
/// The evaluated CUE configuration as the specified type `T`
///
/// # Example
/// ```
/// use serde::Deserialize;
///
/// #[derive(Deserialize)]
/// struct MyConfig {
///     name: String,
/// }
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let directory = tempfile::tempdir()?;
/// std::fs::create_dir(directory.path().join("cue.mod"))?;
/// std::fs::write(directory.path().join("cue.mod/module.cue"), "module: \"example.com/example\"\nlanguage: version: \"v0.9.0\"\n")?;
/// std::fs::write(directory.path().join("env.cue"), "package mypackage\nname: \"example\"\n")?;
/// let config: MyConfig = cuengine::evaluate_cue_package_typed(directory.path(), "mypackage")?;
/// assert_eq!(config.name, "example");
/// # Ok(())
/// # }
/// ```
#[tracing::instrument(
    name = "evaluate_cue_package_typed",
    fields(
        dir_path = %dir_path.display(),
        package_name = package_name,
        target_type = std::any::type_name::<T>(),
    ),
    level = "info"
)]
pub fn evaluate_cue_package_typed<T>(dir_path: &Path, package_name: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    tracing::debug!("Evaluating CUE package with typed deserialization");

    // Get the JSON string from the basic evaluation
    let json_str = evaluate_cue_package(dir_path, package_name)?;

    // Deserialize into the target type
    serde_json::from_str(&json_str).map_err(|e| {
        tracing::error!(
            target_type = std::any::type_name::<T>(),
            "Failed to deserialize CUE output"
        );
        Error::configuration(format!(
            "Failed to parse CUE output as {}: {}",
            std::any::type_name::<T>(),
            e
        ))
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "bridge_trace_tests.rs"]
mod bridge_trace_tests;
