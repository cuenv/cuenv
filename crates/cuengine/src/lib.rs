//! Go-Rust FFI bridge for CUE evaluation
//!
//! This crate provides a safe Rust interface to the Go-based CUE evaluator.
//! It handles all FFI operations, memory management, and error handling for
//! calling Go functions from Rust.

pub mod cache;
pub mod error;
pub mod retry;
pub mod validation;

// Re-export main types
pub use error::{CueEngineError, Result};
pub use retry::RetryConfig;
pub use validation::Limits;

// Local type alias for internal use
use error::CueEngineError as Error;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString};
use std::marker::PhantomData;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

// Bridge error codes - keep in sync with Go side constants
// These match the constants defined in bridge.go
const ERROR_CODE_INVALID_INPUT: &str = "INVALID_INPUT";
const ERROR_CODE_LOAD_INSTANCE: &str = "LOAD_INSTANCE";
const ERROR_CODE_BUILD_VALUE: &str = "BUILD_VALUE";
const ERROR_CODE_ORDERED_JSON: &str = "ORDERED_JSON";
const ERROR_CODE_PANIC_RECOVER: &str = "PANIC_RECOVER";
const ERROR_CODE_JSON_MARSHAL: &str = "JSON_MARSHAL_ERROR";
const ERROR_CODE_REGISTRY_INIT: &str = "REGISTRY_INIT";
const ERROR_CODE_DEPENDENCY_RES: &str = "DEPENDENCY_RESOLUTION";
const BRIDGE_PROTOCOL_VERSION: &str = "bridge/1";
const MODULE_EVAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Error response from the Go bridge
#[derive(Debug, Deserialize, Serialize)]
struct BridgeError {
    code: String,
    message: String,
    hint: Option<String>,
}

/// Structured response envelope from the Go bridge
#[derive(Debug, Deserialize)]
struct BridgeEnvelope<'a> {
    version: String,
    #[serde(borrow)]
    ok: Option<&'a serde_json::value::RawValue>,
    error: Option<BridgeError>,
}

/// RAII wrapper for C strings returned from FFI
/// Ensures proper cleanup when the wrapper goes out of scope
///
/// This type is intentionally !Send and !Sync because the underlying
/// C pointer comes from Go's runtime which is not thread-safe.
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
    /// The caller must ensure that:
    /// - `ptr` is either null or a valid pointer returned from a CUE FFI function
    /// - The pointer has not been freed already
    /// - The pointer will not be used after this wrapper is dropped
    /// - No other thread is accessing this pointer
    /// - The memory pointed to by `ptr` will remain valid for the lifetime of this wrapper
    ///
    /// # FFI Contract
    ///
    /// This function expects that the Go side:
    /// - Returns either null or a valid C string pointer
    /// - Allocates memory that must be freed with `cue_free_string`
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
    /// This function is safe to call when:
    /// - The wrapped pointer is not null (checked with `debug_assert`)
    /// - The pointer points to a valid null-terminated C string
    /// - The pointed-to memory contains valid UTF-8 data
    /// - The memory will not be modified during the lifetime of the returned &str
    ///
    /// # Errors
    ///
    /// Returns an error if the C string contains invalid UTF-8
    ///
    /// # Panics
    ///
    /// In debug builds, panics if the pointer is null
    #[expect(
        unsafe_code,
        reason = "Required to expose a borrowed str from the wrapped C string"
    )]
    pub unsafe fn to_str(&self) -> Result<&str> {
        debug_assert!(
            !self.is_null(),
            "Attempted to convert null pointer to string"
        );

        // SAFETY: We've verified the pointer is not null via debug_assert
        // The caller must ensure the pointer points to a valid C string
        let cstr = {
            #[expect(
                unsafe_code,
                reason = "Required to borrow the validated C string pointer"
            )]
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
            // Safety: cue_free_string is safe to call because:
            // - self.ptr is not null (checked above)
            // - self.ptr was allocated by the Go side via cue_eval_package
            // - We have exclusive ownership of this pointer (enforced by Rust's ownership)
            // - This pointer has not been freed already (enforced by Drop only running once)
            // - After this call, the pointer becomes invalid and won't be used again
            #[expect(
                unsafe_code,
                reason = "Required to free strings allocated by the Go bridge"
            )]
            unsafe {
                cue_free_string(self.ptr);
            }
        }
    }
}

// SAFETY: CStringPtr contains a raw pointer to memory managed by Go's garbage collector.
// The PhantomData<*const ()> marker makes this type !Send and !Sync because:
// 1. The Go runtime may have thread-local state associated with this memory
// 2. The FFI contract doesn't guarantee thread-safety of the underlying memory
// 3. Concurrent access to cue_free_string from multiple threads is undefined behavior
// 4. Raw pointers are inherently not Send/Sync, so PhantomData<*const ()> prevents both

// Real FFI for normal builds
#[cfg(not(docsrs))]
#[link(name = "cue_bridge")]
#[expect(unsafe_code, reason = "Required for Go bridge FFI declarations")]
unsafe extern "C" {
    fn cue_eval_module(
        module_root: *const c_char,
        package_name: *const c_char,
        options_json: *const c_char,
    ) -> *mut c_char;
    fn cue_module_custom_version(
        module_root: *const c_char,
        namespace: *const c_char,
    ) -> *mut c_char;
    fn cue_format_module_with_custom_version(
        module_root: *const c_char,
        namespace: *const c_char,
        version: *const c_char,
    ) -> *mut c_char;
    fn cue_free_string(s: *mut c_char);
    fn cue_bridge_version() -> *mut c_char;
}

// Stub FFI for documentation builds - these satisfy the compiler but panic if called
#[cfg(docsrs)]
unsafe fn cue_eval_module(_: *const c_char, _: *const c_char, _: *const c_char) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

#[cfg(docsrs)]
unsafe fn cue_module_custom_version(_: *const c_char, _: *const c_char) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

#[cfg(docsrs)]
unsafe fn cue_format_module_with_custom_version(
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
    /// Directory to evaluate (for non-recursive), None = module root.
    /// Use this to evaluate a specific subdirectory without loading the entire module.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_dir: Option<String>,
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
#[derive(Debug, Deserialize)]
pub struct ModuleResult {
    /// Map of relative path to evaluated JSON value
    pub instances: std::collections::HashMap<String, serde_json::Value>,
    /// Paths that conform to schema.#Project (verified via CUE unification)
    #[serde(default)]
    pub projects: Vec<String>,
    /// Map of "path/field" to source location (only populated when `with_meta`: true)
    #[serde(default)]
    pub meta: std::collections::HashMap<String, FieldMeta>,
}

/// Cuenv custom metadata extracted from `cue.mod/module.cue`.
#[derive(Debug, Deserialize)]
pub struct ModuleCustomVersion {
    /// The custom cuenv version marker, if present.
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FormattedModuleFile {
    content: String,
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
/// * `module_root` - Path to the CUE module root (directory containing cue.mod/)
/// * `package_name` - Name of the CUE package to evaluate (legacy parameter, prefer using `options.package_name`)
/// * `options` - Evaluation options:
///   - `with_meta`: Extract source positions into separate `meta` map
///   - `with_references`: Extract CUE reference paths (e.g., for `dependsOn: [build]`, records that `dependsOn[0]` refers to `tasks.build`)
///   - `recursive`: Evaluate entire module tree (./...) or just current directory (.)
///   - `package_name`: Filter to specific package (takes precedence over legacy parameter)
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
    let module_result =
        receive_module_eval_result(&rx, module_root, package_name, MODULE_EVAL_TIMEOUT)?;
    log_module_eval_success(&module_result, start_time);

    Ok(module_result)
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

fn receive_module_eval_result(
    rx: &Receiver<Result<ModuleResult>>,
    module_root: &Path,
    package_name: &str,
    timeout: Duration,
) -> Result<ModuleResult> {
    match rx.recv_timeout(timeout) {
        Ok(inner) => inner,
        Err(RecvTimeoutError::Timeout) => {
            tracing::error!(
                timeout_secs = timeout.as_secs(),
                module_root = %module_root.display(),
                package_name,
                "CUE evaluation timed out"
            );
            Err(Error::ffi(
                "cue_eval_module",
                format!(
                    "CUE evaluation timed out after {}s for module {}",
                    timeout.as_secs(),
                    module_root.display()
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

    // Safety: cue_eval_module is an FFI function that:
    // - Takes three valid C string pointers (guaranteed by CString::as_ptr())
    // - Returns either null or a valid pointer to a C string
    // - The returned pointer must be freed with cue_free_string
    let result_ptr = {
        #[expect(
            unsafe_code,
            reason = "Required to call the Go CUE module evaluation bridge"
        )]
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
            json_response = json_str,
            parse_error = %e,
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
    tracing::error!(
        error_code = bridge_error.code,
        error_message = bridge_error.message,
        error_hint = bridge_error.hint,
        "Module evaluation failed"
    );

    let full_message = bridge_error
        .hint
        .map(|hint| format!("{} (Hint: {})", bridge_error.message, hint))
        .unwrap_or(bridge_error.message);

    match bridge_error.code.as_str() {
        ERROR_CODE_INVALID_INPUT | ERROR_CODE_REGISTRY_INIT => Error::configuration(full_message),
        ERROR_CODE_LOAD_INSTANCE | ERROR_CODE_BUILD_VALUE | ERROR_CODE_DEPENDENCY_RES => {
            Error::cue_parse(module_root, full_message)
        }
        ERROR_CODE_ORDERED_JSON | ERROR_CODE_PANIC_RECOVER | ERROR_CODE_JSON_MARSHAL => {
            Error::ffi("cue_eval_module", full_message)
        }
        _ => Error::ffi("cue_eval_module", full_message),
    }
}

/// Parse the module result from JSON.
fn parse_module_result(json_data: &str) -> Result<ModuleResult> {
    serde_json::from_str(json_data).map_err(|e| {
        tracing::error!(
            json_data = json_data,
            parse_error = %e,
            "Failed to parse module result"
        );
        Error::ffi(
            "cue_eval_module",
            format!("Failed to parse module result: {e}"),
        )
    })
}

/// Extract a string from an FFI result wrapper.
fn extract_ffi_string(wrapper: &CStringPtr, fn_name: &'static str) -> Result<String> {
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

    // Safety: wrapper.to_str() is safe because we checked wrapper is not null
    {
        #[expect(
            unsafe_code,
            reason = "Required to read a non-null bridge-owned C string"
        )]
        unsafe {
            wrapper.to_str()
        }
    }
    .map(String::from)
}

fn bridge_owned_c_string(ptr: *mut c_char) -> CStringPtr {
    #[expect(
        unsafe_code,
        reason = "Required to wrap bridge-owned C string pointers for RAII cleanup"
    )]
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

    // Safety: cue_bridge_version is an FFI function that:
    // - Takes no parameters
    // - Returns either null or a valid pointer to a C string
    // - The returned pointer must be freed with cue_free_string
    let version_ptr = {
        #[expect(
            unsafe_code,
            reason = "Required to call the Go bridge version function"
        )]
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

/// Read cuenv custom version metadata from `cue.mod/module.cue`.
///
/// # Errors
///
/// Returns an error if the module file cannot be read or parsed.
pub fn module_custom_version(module_root: &Path, namespace: &str) -> Result<ModuleCustomVersion> {
    const FUNCTION_NAME: &str = "cue_module_custom_version";
    let c_module_root = path_to_cstring(module_root, FUNCTION_NAME, "module root")?;
    let c_namespace = str_to_cstring(namespace, FUNCTION_NAME, "namespace")?;

    // Safety: cue_module_custom_version takes valid C strings and returns a
    // heap-allocated C string owned by the caller.
    let result_ptr = {
        #[expect(unsafe_code, reason = "Required to call the Go custom-version bridge")]
        unsafe {
            cue_module_custom_version(c_module_root.as_ptr(), c_namespace.as_ptr())
        }
    };
    let result = bridge_owned_c_string(result_ptr);
    let json_str = extract_ffi_string(&result, FUNCTION_NAME)?;
    process_bridge_json(&json_str, module_root, FUNCTION_NAME)
}

/// Return a formatted `cue.mod/module.cue` with cuenv custom version metadata set.
///
/// This function does not write to disk; callers decide how to handle dry-run
/// and check modes.
///
/// # Errors
///
/// Returns an error if the module file cannot be read, parsed, or formatted.
pub fn format_module_with_custom_version(
    module_root: &Path,
    namespace: &str,
    version: &str,
) -> Result<String> {
    const FUNCTION_NAME: &str = "cue_format_module_with_custom_version";
    let c_module_root = path_to_cstring(module_root, FUNCTION_NAME, "module root")?;
    let c_namespace = str_to_cstring(namespace, FUNCTION_NAME, "namespace")?;
    let c_version = str_to_cstring(version, FUNCTION_NAME, "version")?;

    // Safety: cue_format_module_with_custom_version takes valid C strings and
    // returns a heap-allocated C string owned by the caller.
    let result_ptr = {
        #[expect(
            unsafe_code,
            reason = "Required to call the Go module-formatting bridge"
        )]
        unsafe {
            cue_format_module_with_custom_version(
                c_module_root.as_ptr(),
                c_namespace.as_ptr(),
                c_version.as_ptr(),
            )
        }
    };
    let result = bridge_owned_c_string(result_ptr);
    let json_str = extract_ffi_string(&result, FUNCTION_NAME)?;
    let formatted: FormattedModuleFile =
        process_bridge_json(&json_str, module_root, FUNCTION_NAME)?;
    Ok(formatted.content)
}

/// Convenience wrapper around `evaluate_module` for single-directory evaluation.
///
/// Uses `evaluate_module` with `recursive: false` internally.
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
    let options = ModuleEvalOptions {
        with_meta: false,
        with_references: false,
        recursive: false,
        package_name: None,
        target_dir: None, // Use module root
    };

    let result = evaluate_module(dir_path, package_name, Some(&options))?;

    // For single-directory eval, extract the instance at "." or the only instance
    let instance = result
        .instances
        .get(".")
        .or_else(|| result.instances.values().next())
        .ok_or_else(|| {
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
/// ```no_run
/// use cuengine::evaluate_cue_package_typed;
/// use serde::Deserialize;
/// use std::path::Path;
///
/// #[derive(Deserialize)]
/// struct MyConfig {
///     name: String,
/// }
///
/// let path = Path::new("/path/to/cue/files");
/// let config: MyConfig = evaluate_cue_package_typed(path, "mypackage").unwrap();
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
            "Failed to deserialize CUE output to {}: {}",
            std::any::type_name::<T>(),
            e
        );
        Error::configuration(format!(
            "Failed to parse CUE output as {}: {}",
            std::any::type_name::<T>(),
            e
        ))
    })
}

#[cfg(test)]
mod tests;
