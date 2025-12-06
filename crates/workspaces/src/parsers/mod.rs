//! Parser implementations for converting package-manager lockfiles into `LockfileEntry` structs.
//!
//! Each parser implements the [`LockfileParser`](crate::LockfileParser) trait and focuses on a
//! specific ecosystem. Feature flags allow opting into only the parsers you need so that binary
//! size and compile time remain minimal.
//!
//! Parsers are available for JavaScript and Rust ecosystems today, and the module layout is
//! intentionally extensible for additional package managers.
//!
//! ## Feature flags
//!
//! - `parsers-javascript`: Enables all JavaScript ecosystem parsers (npm, bun, pnpm, yarn)
//! - `parsers-rust`: Enables all Rust ecosystem parsers (currently implies `parser-cargo`)
//! - `parser-cargo`: Fine-grained feature for Cargo lockfile parsing (can be enabled on its own)
//!
//! **Note:** While fine-grained features like `parser-cargo` exist, it's recommended to use
//! the aggregate features (`parsers-rust`, `parsers-javascript`) for simplicity unless you
//! need to minimize dependencies for a specific parser.

#[cfg(feature = "parsers-javascript")]
pub mod javascript;
#[cfg(any(feature = "parsers-rust", feature = "parser-cargo"))]
pub mod rust;

#[cfg(feature = "parsers-javascript")]
pub use javascript::*;
#[cfg(any(feature = "parsers-rust", feature = "parser-cargo"))]
pub use rust::*;
