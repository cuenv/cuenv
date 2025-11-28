#![allow(missing_docs)]

pub mod affected;
pub mod context;
pub mod discovery;
pub mod executor;
pub mod provider;
pub mod report;

use cuenv_core::Error;
pub type Result<T> = std::result::Result<T, Error>;
