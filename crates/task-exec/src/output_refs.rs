//! Task output reference handling for the execution engine.
//!
//! The pure parsing/detection half (`process_output_refs`,
//! `parse_passthrough`, ...) lives in `cuenv_core::tasks::output_refs`
//! because module evaluation needs it too; it is re-exported here so the
//! engine keeps one import surface. The runtime half —
//! [`OutputRefResolver`], which substitutes placeholders with completed
//! [`TaskResult`](crate::TaskResult) values — is owned by this crate.

mod resolver;

pub use cuenv_core::tasks::output_refs::{
    OutputRefDep, TaskOutputField, TaskOutputRef, has_output_refs, parse_passthrough,
    process_output_refs, try_extract_passthrough,
};
pub use resolver::OutputRefResolver;
