//! Event bus subscribers.
//!
//! Subscribers consume events off the bus and route them to a non-UI
//! destination — files, telemetry sinks, replay harnesses. They differ
//! from [`crate::renderers`] in that they produce no user-facing
//! rendering; output is structured / machine-readable.

pub mod recorder;

pub use recorder::{EventRecorder, EventReplayReader, RecorderError};
