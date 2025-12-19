#![allow(missing_docs)]

pub mod affected;
pub mod compiler;
pub mod context;
pub mod discovery;
pub mod executor;
pub mod flake;
pub mod ir;
pub mod provider;
pub mod report;

use cuenv_core::Error;
pub type Result<T> = std::result::Result<T, Error>;
