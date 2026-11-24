//! Native CUE references to resource and data-source outputs.
//!
//! Scalar destinations may be nested inside provider objects, maps and ordered
//! collections. Provider schema compatibility, dependency ordering and
//! sensitivity belong to the engine; this module classifies references and
//! replaces their exact configuration positions.

mod configuration_references;
mod destination;
mod discovery;
mod evaluation;
mod expressions;
mod output_path;
mod path;
mod secrets;

use configuration_references::is_identifier;
pub use configuration_references::{
    ConfigurationReferences, ResourceOutputReference, SourceAddress,
};
pub use destination::ReferenceDestination;
pub use evaluation::{
    ConfigurationEvaluator, ConfigurationSelector, ConfigurationSpecialization,
    ConfigurationVerification, NativeReference, OutputIdentity, SpecializedConfiguration,
};
pub use expressions::{ExpressionEvaluator, LoweredReferences, Selection, lower};
pub use output_path::OutputPath;
pub use path::DestinationPath;
pub use secrets::SecretArgument;
