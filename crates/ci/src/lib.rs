#![allow(missing_docs)]

pub mod affected;
pub mod compiler;
pub mod context;
pub mod diff;
pub mod discovery;
pub mod emitter;
pub mod executor;
pub mod flake;
pub mod gc;
pub mod ir;
pub mod pipeline;
pub mod provider;
pub mod render;
pub mod report;
pub mod stages;

pub use compiler::ContributorFactory;
pub use render::StageRenderer;
pub use stages::StageContributor;

pub use diff::{DiffError, DigestDiff, compare_by_sha, compare_runs};
pub use gc::{GCConfig, GCError, GCStats, GarbageCollector};

use cuenv_core::Error;
pub type Result<T> = std::result::Result<T, Error>;
