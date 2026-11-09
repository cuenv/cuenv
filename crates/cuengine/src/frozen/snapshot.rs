//! Immutable CUE snapshots and specialization.

use crate::bridge::BridgeEnvelope;
use crate::error::FrozenWorkerFailure;
use crate::{CueEngineError, Result};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::ffi::CString;
use std::fmt;
use std::os::raw::c_char;
use std::path::PathBuf;
use std::sync::Arc;

/// One unambiguous CUE value selector. A field named `"0"` is not list index 0.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ValueSelector {
    /// An ordinary struct field, including quoted and punctuation-bearing names.
    Field(String),
    /// A concrete list index.
    Index(usize),
}

/// A sequence of ordinary field or list selectors; empty selects the root.
pub type ValuePath = Vec<ValueSelector>;

/// The exact package instance to freeze, including its transitive imports.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreezeOptions {
    /// Module root used to resolve imports.
    pub module_root: PathBuf,
    /// Directory containing the selected package.
    pub directory: PathBuf,
    /// Explicit package name; wildcard and recursive selection are unsupported.
    pub package: String,
}

/// Lenient inspection of a frozen instance; this does not validate provider input.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenEvaluationOptions {
    /// Struct field paths to export, preserving their nesting. No list indices.
    pub export_paths: Vec<ValuePath>,
    /// Subtrees in which to inspect native reference provenance.
    pub reference_paths: Vec<ValuePath>,
    /// Values only known at runtime, including what is below them. Bare primitive
    /// declarations nothing else refines count as well.
    pub source_paths: Option<Vec<ValuePath>>,
}

/// Strict validation of selected frozen subtrees before consuming inspection data.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenValidationOptions {
    /// Subtrees that must exist and be final and concrete except the exemptions.
    pub concrete_paths: Vec<ValuePath>,
    /// Exact structs containing only bare string, number or boolean declarations.
    pub abstract_paths: Vec<ValuePath>,
    /// Exact direct reference leaves, verified against the same frozen instance.
    pub deferred: Vec<DeferredValue>,
    /// Runtime source declarations, with the same meaning as inspection options.
    pub source_paths: Option<Vec<ValuePath>>,
}

/// A native reference discovered before runtime values are supplied.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenReference {
    /// Consumer path, including typed list indices.
    pub path: ValuePath,
    /// Terminal source path after resolving aliases.
    pub target: ValuePath,
    /// Bare source declaration (`string`, `number`, `bool`), or empty otherwise.
    pub primitive_type: String,
    /// Whether the value can be supplied once its sources are known: nothing about
    /// the structure around it depends on a source.
    pub supported: bool,
    /// Whether CUE computes the value from its sources rather than carrying one.
    #[serde(default)]
    pub computed: bool,
}

/// Inspection result. Raw JSON preserves exact numeric values.
#[derive(Clone, Deserialize)]
pub struct FrozenEvaluation {
    /// Projected inspection data, with incomplete values represented as null.
    pub value: Box<RawValue>,
    /// Reference provenance from the same frozen instance.
    pub references: Vec<FrozenReference>,
    /// What each requested source declares itself to be, referenced or not.
    pub declarations: Vec<FrozenDeclaration>,
}

/// What a source declares itself to be.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenDeclaration {
    /// The source.
    pub path: ValuePath,
    /// Its bare primitive type, empty when it is anything else.
    pub primitive_type: String,
}

impl fmt::Debug for FrozenEvaluation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrozenEvaluation")
            .field("reference_count", &self.references.len())
            .finish_non_exhaustive()
    }
}

/// A JSON string, boolean or exact number, never a CUE expression.
#[derive(Clone, Serialize)]
#[serde(transparent)]
pub struct ScalarValue(Box<RawValue>);

impl ScalarValue {
    /// Encode a string as a literal, including its quotes and escapes.
    ///
    /// # Errors
    /// Returns an error if JSON encoding fails.
    pub fn string(value: &str) -> Result<Self> {
        serde_json::value::to_raw_value(value)
            .map(Self)
            .map_err(|_| safe_error("scalar", "CUE scalar could not be encoded"))
    }

    /// Accept a pre-encoded JSON scalar without a numeric round trip.
    ///
    /// # Errors
    /// Returns an error for null, arrays or objects.
    pub fn from_json(value: &RawValue) -> Result<Self> {
        if !matches!(
            value.get().as_bytes().first(),
            Some(b'"' | b't' | b'f' | b'-' | b'0'..=b'9')
        ) {
            return Err(safe_error(
                "scalar",
                "CUE source must be a primitive scalar",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    /// Parse a JSON number without converting through a floating-point value.
    ///
    /// # Errors
    /// Returns a value-free error unless the entire input is one JSON number.
    pub fn number(value: &str) -> Result<Self> {
        let raw: Box<RawValue> = serde_json::from_str(value)
            .map_err(|_| safe_error("scalar", "Invalid CUE numeric scalar"))?;
        if !matches!(raw.get().as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
            return Err(safe_error("scalar", "Invalid CUE numeric scalar"));
        }
        Ok(Self(raw))
    }
}

impl fmt::Debug for ScalarValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ScalarValue(<redacted>)")
    }
}

/// A scalar unified at one existing bare primitive source declaration.
#[derive(Clone, Debug, Serialize)]
pub struct ScalarFill {
    /// Explicit source path, never a consumer path or a CUE expression.
    pub path: ValuePath,
    /// Runtime value. Debug output deliberately excludes its contents.
    pub value: ScalarValue,
}

/// One exact consumer leaf allowed to remain unknown, with its source identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeferredValue {
    /// Consumer leaf path. Ancestors and unrelated siblings cannot be deferred.
    pub path: ValuePath,
    /// Native source path verified against the frozen CUE reference.
    pub target: ValuePath,
}

/// A fresh specialization, always based on the original snapshot.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecializationOptions {
    /// Known primitive sources; duplicate, nonexistent and nonprimitive paths fail.
    pub fills: Vec<ScalarFill>,
    /// Consumer subtree to validate and export.
    pub projection: ValuePath,
    /// Exact native references whose source may remain unknown for this call.
    pub deferred: Vec<DeferredValue>,
    /// Runtime declarations, including refined declarations and descendants.
    pub source_paths: Option<Vec<ValuePath>>,
}

/// Strictly validated consumer data and unresolved native source provenance.
#[derive(Clone, Deserialize)]
pub struct SpecializationResult {
    /// Consumer JSON. Exact deferred leaves are null even if CUE narrowed them to
    /// a literal; all other leaves are concrete and preserve their numeric text.
    pub value: Box<RawValue>,
    /// Sources still unknown in this specialization, even after literal narrowing.
    pub unresolved: Vec<DeferredValue>,
}

impl fmt::Debug for SpecializationResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SpecializationResult")
            .field("unresolved_count", &self.unresolved.len())
            .finish_non_exhaustive()
    }
}

/// An immutable, memory-only snapshot of a selected instance and its imports.
///
/// Clones share immutable source bytes. Concurrent calls create independent Go
/// build graphs and CUE contexts. Source files and registry dependencies are not
/// read again. Snapshots intentionally cannot be serialized or logged as source.
#[derive(Clone)]
pub struct FrozenInstance {
    pub(super) snapshot: Arc<str>,
}

impl fmt::Debug for FrozenInstance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrozenInstance")
            .finish_non_exhaustive()
    }
}

impl FrozenInstance {
    /// Freeze one loader-selected package and all its imported source instances.
    ///
    /// Uses the configured evaluation timeout. A timeout stops waiting but does
    /// not stop CUE; the worker retains capacity until it actually returns.
    ///
    /// # Errors
    /// Returns a value-free error if loading or snapshot capture fails, or a
    /// typed worker error if capacity or the operation deadline is exhausted.
    pub fn load(options: &FreezeOptions) -> Result<Self> {
        let options = options.clone();
        crate::frozen_worker::run(move || Self::load_inner(&options))
    }

    fn load_inner(options: &FreezeOptions) -> Result<Self> {
        let options = encode_options(options)?;
        #[expect(unsafe_code, reason = "Required to call the CUE snapshot bridge")]
        // SAFETY: options is a valid C string; the bridge returns caller-owned
        // C memory which call_result frees before this method returns.
        let pointer = unsafe { cue_freeze_instance(options.as_ptr()) };
        let snapshot = call_result(pointer, "cue_freeze_instance")?;
        Ok(Self {
            snapshot: Arc::from(snapshot.get()),
        })
    }

    /// Inspect the original frozen instance without filling runtime sources.
    ///
    /// This is lenient inspection only. Use `specialize` before consuming values
    /// that must be concrete. Uses the configured timeout; a timeout does not
    /// cancel the underlying operation or release its worker capacity.
    ///
    /// # Errors
    /// Returns a value-free error if evaluation or reference inspection fails,
    /// or a typed worker error for exhausted capacity or deadline.
    pub fn evaluate(&self, options: &FrozenEvaluationOptions) -> Result<FrozenEvaluation> {
        let snapshot = self.clone();
        let options = options.clone();
        crate::frozen_worker::run(move || snapshot.evaluate_inner(&options))
    }

    fn evaluate_inner(&self, options: &FrozenEvaluationOptions) -> Result<FrozenEvaluation> {
        let snapshot = encode_snapshot(&self.snapshot)?;
        let options = encode_options(options)?;
        #[expect(
            unsafe_code,
            reason = "Required to call the frozen CUE evaluation bridge"
        )]
        // SAFETY: both inputs remain alive throughout the synchronous call, and
        // call_result takes ownership of and frees the returned C allocation.
        let pointer = unsafe { cue_evaluate_frozen(snapshot.as_ptr(), options.as_ptr()) };
        decode_result(pointer, "cue_evaluate_frozen")
    }

    /// Strictly validate selected subtrees from the same immutable snapshot.
    ///
    /// Required fields and list lengths remain mandatory. Exemptions permit only
    /// verified direct references and namespaces of bare primitive declarations.
    /// Uses the configured timeout; a timeout does not cancel the underlying
    /// operation or release its worker capacity.
    ///
    /// # Errors
    /// Returns a value-free error for invalid exemptions, missing required fields,
    /// ordinary incomplete values, or failed CUE constraints. Exhausted capacity
    /// or deadline returns a typed worker error.
    pub fn validate(&self, options: &FrozenValidationOptions) -> Result<()> {
        let snapshot = self.clone();
        let options = options.clone();
        crate::frozen_worker::run(move || snapshot.validate_inner(&options))
    }

    fn validate_inner(&self, options: &FrozenValidationOptions) -> Result<()> {
        let snapshot = encode_snapshot(&self.snapshot)?;
        let options = encode_options(options)?;
        #[expect(
            unsafe_code,
            reason = "Required to call the frozen CUE validation bridge"
        )]
        // SAFETY: both inputs remain alive throughout the synchronous call, and
        // call_result takes ownership of and frees the returned C allocation.
        let pointer = unsafe { cue_validate_frozen(snapshot.as_ptr(), options.as_ptr()) };
        let validated: bool = decode_result(pointer, "cue_validate_frozen")?;
        if validated {
            Ok(())
        } else {
            Err(safe_error(
                "cue_validate_frozen",
                "CUE validation was unsuccessful",
            ))
        }
    }

    /// Unify runtime scalars into fresh source declarations and validate a consumer.
    ///
    /// Every call starts from the original frozen sources. No previous fill is
    /// retained. Exact deferred references are checked against that same snapshot.
    /// Calls are synchronous and not cancellable. Admission shares the same
    /// worker capacity as loading, inspection and validation; callers may apply
    /// their own deadline without creating a second internal worker.
    ///
    /// # Errors
    /// Returns a value-free error for invalid fills, unsupported references,
    /// missing required values, or failed CUE constraints. Shared capacity that
    /// stays exhausted until the evaluation timeout returns a typed worker error
    /// without starting evaluation.
    pub fn specialize(&self, options: &SpecializationOptions) -> Result<SpecializationResult> {
        let _permit = crate::frozen_worker::admit()?;
        let snapshot = encode_snapshot(&self.snapshot)?;
        let options = encode_options(options)?;
        #[expect(
            unsafe_code,
            reason = "Required to call the frozen CUE specialization bridge"
        )]
        // SAFETY: both inputs remain alive throughout the synchronous call, and
        // call_result takes ownership of and frees the returned C allocation.
        let pointer = unsafe { cue_specialize_frozen(snapshot.as_ptr(), options.as_ptr()) };
        decode_result(pointer, "cue_specialize_frozen")
    }
}

fn safe_error(function: &'static str, message: &'static str) -> CueEngineError {
    CueEngineError::ffi(function, message.to_owned())
}

pub(super) fn encode_options(options: &impl Serialize) -> Result<CString> {
    let encoded = serde_json::to_vec(options)
        .map_err(|_| safe_error("frozen", "CUE options could not be encoded"))?;
    CString::new(encoded).map_err(|_| safe_error("frozen", "Invalid CUE options encoding"))
}

pub(super) fn encode_snapshot(snapshot: &str) -> Result<CString> {
    CString::new(snapshot).map_err(|_| safe_error("frozen", "Invalid CUE snapshot encoding"))
}

fn call_result(pointer: *mut c_char, function: &'static str) -> Result<Box<RawValue>> {
    let owned = crate::bridge::bridge_owned_c_string(pointer);
    let response = crate::bridge::extract_ffi_string(&owned, function)
        .map_err(|_| safe_error(function, "CUE bridge returned an invalid response"))?;
    let envelope: BridgeEnvelope<'_> = serde_json::from_str(&response)
        .map_err(|_| safe_error(function, "CUE bridge returned an invalid response"))?;
    if envelope.version != crate::bridge::BRIDGE_PROTOCOL_VERSION {
        return Err(safe_error(function, "Unsupported CUE bridge protocol"));
    }
    if let Some(error) = envelope.error {
        return Err(bridge_failure(function, &error.code));
    }
    envelope
        .ok
        .map(ToOwned::to_owned)
        .ok_or_else(|| safe_error(function, "CUE bridge returned an empty response"))
}

/// Only a build or load failure is about the configuration. The bridge's message
/// stays behind, as it can contain values.
fn bridge_failure(function: &'static str, code: &str) -> CueEngineError {
    let reason = match code {
        crate::bridge::ERROR_CODE_LOAD_INSTANCE | crate::bridge::ERROR_CODE_BUILD_VALUE => {
            return safe_error(function, "CUE operation failed validation");
        }
        crate::bridge::ERROR_CODE_ANALYSIS => FrozenWorkerFailure::Analysis,
        _ => FrozenWorkerFailure::Bridge,
    };
    CueEngineError::FrozenWorker { reason }
}

pub(super) fn decode_result<T: for<'de> Deserialize<'de>>(
    pointer: *mut c_char,
    function: &'static str,
) -> Result<T> {
    let payload = call_result(pointer, function)?;
    serde_json::from_str(payload.get())
        .map_err(|_| safe_error(function, "CUE bridge returned an invalid payload"))
}

#[cfg(not(docsrs))]
#[link(name = "cue_bridge")]
#[expect(
    unsafe_code,
    reason = "Required for generic CUE snapshot FFI declarations"
)]
unsafe extern "C" {
    fn cue_freeze_instance(options: *const c_char) -> *mut c_char;
    fn cue_evaluate_frozen(snapshot: *const c_char, options: *const c_char) -> *mut c_char;
    fn cue_validate_frozen(snapshot: *const c_char, options: *const c_char) -> *mut c_char;
    fn cue_specialize_frozen(snapshot: *const c_char, options: *const c_char) -> *mut c_char;
}

#[cfg(docsrs)]
unsafe fn cue_freeze_instance(_: *const c_char) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

#[cfg(docsrs)]
unsafe fn cue_evaluate_frozen(_: *const c_char, _: *const c_char) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

#[cfg(docsrs)]
unsafe fn cue_validate_frozen(_: *const c_char, _: *const c_char) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

#[cfg(docsrs)]
unsafe fn cue_specialize_frozen(_: *const c_char, _: *const c_char) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_build_and_load_failures_are_about_the_configuration() {
        for code in [
            crate::bridge::ERROR_CODE_BUILD_VALUE,
            crate::bridge::ERROR_CODE_LOAD_INSTANCE,
        ] {
            assert!(matches!(
                bridge_failure("test", code),
                CueEngineError::Ffi { .. }
            ));
        }
        assert!(matches!(
            bridge_failure("test", crate::bridge::ERROR_CODE_ANALYSIS),
            CueEngineError::FrozenWorker {
                reason: FrozenWorkerFailure::Analysis
            }
        ));
        for code in [
            crate::bridge::ERROR_CODE_PANIC_RECOVER,
            crate::bridge::ERROR_CODE_INVALID_INPUT,
            "UNKNOWN",
        ] {
            assert!(matches!(
                bridge_failure("test", code),
                CueEngineError::FrozenWorker {
                    reason: FrozenWorkerFailure::Bridge
                }
            ));
        }
    }
}
