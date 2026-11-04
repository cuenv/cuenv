//! CUE-independent contracts for an immutable native configuration evaluator.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Map, Value as Json};

use super::SourceAddress;
use crate::error::Result;
use crate::type_system::Value;

/// A native selector before schema classification into an attribute or map key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigurationSelector {
    /// A struct field or map key, including quoted and special names.
    Field(String),
    /// An ordered list or tuple element.
    Index(i64),
}

/// One primitive output of a managed resource or data source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutputIdentity {
    /// Producer namespace and address.
    pub source: SourceAddress,
    /// Top-level primitive output attribute.
    pub attribute: String,
}

/// A native selector discovered from the original immutable CUE instance.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NativeReference {
    /// Managed resource or data lookup consuming the output.
    pub consumer: SourceAddress,
    /// Path relative to the consumer's configuration object.
    pub destination: Vec<ConfigurationSelector>,
    /// Producer namespace and address.
    pub source: SourceAddress,
    /// Top-level primitive output attribute.
    pub attribute: String,
    /// CUE computes the destination from its sources instead of carrying this one,
    /// so it may read several, and the two need not have the same type.
    pub computed: bool,
}

/// Inputs for a fresh consumer evaluation from the original snapshot.
/// Values deliberately do not implement Debug to prevent accidental disclosure.
pub struct ConfigurationSpecialization<'input> {
    /// Consumer whose complete configuration must be validated and exported.
    pub consumer: &'input SourceAddress,
    /// Concrete primitive source values. Unknown values are never placed here.
    pub sources: &'input BTreeMap<OutputIdentity, Value>,
    /// Exact native references whose source is still unknown in this call.
    pub deferred: &'input [NativeReference],
}

/// Strict consumer projection and its remaining native source provenance.
/// Values deliberately do not implement Debug to prevent accidental disclosure.
pub struct SpecializedConfiguration {
    /// Concrete configuration JSON, with null at exact deferred native leaves.
    pub configuration: Map<String, Json>,
    /// Exact references still deferred, including a source narrowed by CUE to
    /// a literal. The engine verifies these against the requested provenance.
    pub unresolved: Vec<NativeReference>,
}

/// Real output values to check against the whole instance.
/// Values deliberately do not implement Debug to prevent accidental disclosure.
pub struct ConfigurationVerification<'input> {
    /// Concrete primitive outputs known at this point. Unknown values never appear.
    pub sources: &'input BTreeMap<OutputIdentity, Value>,
}

/// Immutable, in-memory evaluation of the original CUE instance.
///
/// Implementations must start every specialization from the same frozen source
/// graph, apply concrete values as scalar literals, validate the complete
/// consumer subtree, and preserve each requested deferred source identity even
/// when CUE constraints narrow the projected value. No filesystem reads, source
/// rewrites, retained fills, or runtime expression evaluation are permitted.
pub trait ConfigurationEvaluator: Send + Sync {
    /// Native references inspected before any runtime output was supplied.
    fn references(&self) -> &[NativeReference];

    /// Maximum time to wait for one specialization. The underlying evaluator
    /// may continue on a detached worker after the caller stops waiting.
    fn timeout(&self) -> Duration {
        Duration::from_secs(60)
    }

    /// Every output this instance declares, whether or not a reference uses it.
    fn declared_outputs(&self) -> &[OutputIdentity] {
        &[]
    }

    /// Check that real outputs change nothing outside the native references.
    ///
    /// Reference analysis cannot prove an expression it does not model is
    /// independent of every value. This checks the values that actually exist.
    ///
    /// # Errors
    ///
    /// Returns a value-free error when the values conflict with CUE constraints or
    /// change a configuration value no native reference accounts for.
    fn verify(&self, _input: &ConfigurationVerification<'_>) -> Result<()> {
        Ok(())
    }

    /// Specialize and validate one complete consumer configuration.
    ///
    /// # Errors
    ///
    /// Returns a value-free error for any invalid fill, violated CUE constraint,
    /// nonconcrete ordinary value, or invalid deferred reference provenance.
    fn specialize(
        &self,
        input: &ConfigurationSpecialization<'_>,
    ) -> Result<SpecializedConfiguration>;
}
