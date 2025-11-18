//! JavaScript package manager lockfile parsers.
//!
//! Each parser translates a package manager specific lockfile into [`LockfileEntry`](crate::LockfileEntry)
//! instances using the shared [`LockfileParser`](crate::LockfileParser) trait. The implementations are
//! gated behind fine-grained Cargo features so consumers can opt into only the parsers required for
//! their toolchain.

#[cfg(feature = "parser-bun")]
pub mod bun;
#[cfg(feature = "parser-npm")]
pub mod npm;
#[cfg(feature = "parser-pnpm")]
pub mod pnpm;
#[cfg(feature = "parser-yarn-classic")]
pub mod yarn_classic;
#[cfg(feature = "parser-yarn-modern")]
pub mod yarn_modern;

#[cfg(feature = "parser-bun")]
pub use bun::BunLockfileParser;
#[cfg(feature = "parser-npm")]
pub use npm::NpmLockfileParser;
#[cfg(feature = "parser-pnpm")]
pub use pnpm::PnpmLockfileParser;
#[cfg(feature = "parser-yarn-classic")]
pub use yarn_classic::YarnClassicLockfileParser;
#[cfg(feature = "parser-yarn-modern")]
pub use yarn_modern::YarnModernLockfileParser;
