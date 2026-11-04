//! Stateless, authenticated provenance reuse for an immutable source graph.

use super::{
    DeferredValue, FrozenInstance, ScalarFill, SpecializationResult, ValuePath, decode_result,
    encode_options, encode_snapshot,
};
use crate::Result;
use serde::Serialize;
use std::fmt;
use std::os::raw::c_char;
use std::sync::Arc;

/// Fix the dependency scope and exact consumer projections for preparation.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparationOptions {
    /// Roots whose discovered references authorize global dependency analysis.
    /// Every projection and its references must already be covered by these roots.
    pub reference_paths: Vec<ValuePath>,
    /// Values only known at runtime. `Some([])` cannot authorize any source for a
    /// prepared consumer. Explicit paths cover what is below them; fills remain
    /// limited to exact proven source leaves.
    pub source_paths: Option<Vec<ValuePath>>,
    /// Exact consumer paths allowed by the resulting capability. Descendants do
    /// not inherit permission, and runtime calls cannot expand this list.
    pub projections: Vec<ValuePath>,
}

/// Values for one of the capability's exact, previously proven projections.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PreparedSpecializationOptions {
    /// Known sources used by this projection; other consumers' sources fail.
    pub fills: Vec<ScalarFill>,
    /// An exact projection supplied during preparation.
    pub projection: ValuePath,
    /// Exact unknown leaves and targets proven for this projection.
    pub deferred: Vec<DeferredValue>,
}

/// Real runtime values and what they may legitimately change.
///
/// Analysis classifies expressions ahead of time. This asks CUE what the values
/// that actually exist do to the instance, so it needs no model of the syntax.
#[derive(Clone, Debug, Default, Serialize)]
pub struct VerificationOptions {
    /// Known source values to apply together.
    pub fills: Vec<ScalarFill>,
    /// Subtrees compared with and without the fills.
    pub roots: Vec<ValuePath>,
    /// Subtrees the fills are allowed to change.
    pub mutable: Vec<ValuePath>,
    /// Leaves that must equal their target once it is filled, which they may change.
    pub references: Vec<DeferredValue>,
}

/// Private immutable provenance bound to one frozen graph and fixed scope.
///
/// This capability owns only Rust source/proof bytes. No live CUE context or
/// global session handle is retained. The bridge authenticates its provenance
/// with a process-local key, and this type cannot be constructed or serialized
/// by callers. Every specialization rebuilds and validates CUE afresh.
#[derive(Clone)]
pub struct PreparedInstance {
    instance: FrozenInstance,
    capability: Arc<str>,
}

impl fmt::Debug for PreparedInstance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedInstance")
            .finish_non_exhaustive()
    }
}

impl FrozenInstance {
    /// Analyze fixed reference roots and prove exact consumer projections once.
    ///
    /// Preparation uses the shared evaluation deadline and worker capacity. A
    /// timeout drops the eventual result but cannot interrupt the CUE worker.
    ///
    /// # Errors
    /// Returns a value-free error for untracked dependencies, unsupported native
    /// references, projections outside the reference scope, or worker failure.
    pub fn prepare(&self, options: &PreparationOptions) -> Result<PreparedInstance> {
        let instance = self.clone();
        let options = options.clone();
        crate::frozen_worker::run(move || {
            let snapshot = encode_snapshot(&instance.snapshot)?;
            let options = encode_options(&options)?;
            // SAFETY: the owned C strings live through the synchronous call;
            // decode_result takes ownership of and frees its result allocation.
            #[expect(unsafe_code, reason = "Required for prepared CUE provenance FFI")]
            let pointer = unsafe { cue_prepare_frozen(snapshot.as_ptr(), options.as_ptr()) };
            let capability: String = decode_result(pointer, "cue_prepare_frozen")?;
            Ok(PreparedInstance {
                instance,
                capability: Arc::from(capability),
            })
        })
    }
}

impl PreparedInstance {
    /// Specialize with authenticated static provenance and fresh CUE validation.
    ///
    /// This synchronous call shares frozen worker capacity. It does not create
    /// another worker or deadline; callers can bound their wait externally.
    /// Every fill, exact deferred pair, whole-instance conflict and final
    /// projected value is checked on a newly reconstructed CUE instance.
    ///
    /// # Errors
    /// Returns a value-free error for an unauthorized projection/source/deferred
    /// pair, failed constraints, incomplete data, or worker capacity that stays
    /// exhausted until the evaluation timeout.
    pub fn specialize(
        &self,
        options: &PreparedSpecializationOptions,
    ) -> Result<SpecializationResult> {
        let _permit = crate::frozen_worker::admit()?;
        let snapshot = encode_snapshot(&self.instance.snapshot)?;
        let capability = encode_snapshot(&self.capability)?;
        let options = encode_options(options)?;
        // SAFETY: all input strings live through this synchronous call;
        // decode_result takes ownership of and frees its result allocation.
        #[expect(unsafe_code, reason = "Required for prepared CUE specialization FFI")]
        let pointer = unsafe {
            cue_specialize_prepared(snapshot.as_ptr(), capability.as_ptr(), options.as_ptr())
        };
        decode_result(pointer, "cue_specialize_prepared")
    }

    /// Check that real values change nothing outside the declared references.
    ///
    /// This shares frozen worker capacity, like [`Self::specialize`]. It
    /// authorizes nothing and returns no values.
    ///
    /// # Errors
    /// Returns a value-free error when the values conflict with CUE constraints or
    /// change a value outside `mutable`, or when worker capacity stays exhausted
    /// until the evaluation timeout.
    pub fn verify(&self, options: &VerificationOptions) -> Result<()> {
        let _permit = crate::frozen_worker::admit()?;
        let snapshot = encode_snapshot(&self.instance.snapshot)?;
        let options = encode_options(options)?;
        // SAFETY: both inputs remain alive throughout the synchronous call, and
        // decode_result takes ownership of and frees its result allocation.
        #[expect(unsafe_code, reason = "Required for frozen CUE verification FFI")]
        let pointer = unsafe { cue_verify_frozen(snapshot.as_ptr(), options.as_ptr()) };
        decode_result::<bool>(pointer, "cue_verify_frozen").map(|_| ())
    }
}

#[cfg(not(docsrs))]
#[link(name = "cue_bridge")]
#[expect(unsafe_code, reason = "Required for prepared CUE FFI declarations")]
unsafe extern "C" {
    fn cue_prepare_frozen(snapshot: *const c_char, options: *const c_char) -> *mut c_char;
    fn cue_specialize_prepared(
        snapshot: *const c_char,
        capability: *const c_char,
        options: *const c_char,
    ) -> *mut c_char;
    fn cue_verify_frozen(snapshot: *const c_char, options: *const c_char) -> *mut c_char;
}

#[cfg(docsrs)]
unsafe fn cue_prepare_frozen(_: *const c_char, _: *const c_char) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

#[cfg(docsrs)]
unsafe fn cue_specialize_prepared(
    _: *const c_char,
    _: *const c_char,
    _: *const c_char,
) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}

#[cfg(docsrs)]
unsafe fn cue_verify_frozen(_: *const c_char, _: *const c_char) -> *mut c_char {
    panic!("FFI not available in documentation builds")
}
