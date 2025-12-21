//! 1Password secret resolution
//!
//! This module provides secret resolution from 1Password using either
//! the WASM SDK (preferred) or CLI fallback.

mod core;
mod resolver;
mod wasm;

pub use resolver::{OnePasswordConfig, OnePasswordResolver};
