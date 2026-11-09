//! OCI lockfile sync provider.
//!
//! Resolves OCI image references to content-addressed digests and writes
//! them to `cuenv.lock` for hermetic binary resolution.
//!
//! For `#ToolsRuntime`, uses ToolProvider to resolve tools from multiple sources
//! (GitHub releases, Nix packages).

mod implementation;
mod tool_resolution;

pub use implementation::LockSyncProvider;
