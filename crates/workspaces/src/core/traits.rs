//! Core traits for workspace discovery and lockfile parsing.

use crate::core::types::{LockfileEntry, Workspace, WorkspaceMember};
use crate::error::Result;
use std::path::Path;

/// Discovers workspace configuration from a root directory.
///
/// Implementations of this trait handle the package manager-specific logic
/// for finding workspace members, validating their structure, and building
/// a complete workspace representation.
///
/// # Example
///
/// ```rust,ignore
/// use cuenv_workspaces::{WorkspaceDiscovery, Workspace};
/// use std::path::Path;
///
/// struct CargoDiscovery;
///
/// impl WorkspaceDiscovery for CargoDiscovery {
///     fn discover(&self, root: &Path) -> Result<Workspace> {
///         // Find Cargo.toml, parse workspace members, etc.
///         todo!()
///     }
///
///     fn find_members(&self, root: &Path) -> Result<Vec<WorkspaceMember>> {
///         // Use glob patterns from Cargo.toml to find member crates
///         todo!()
///     }
///
///     fn validate_member(&self, member_path: &Path) -> Result<bool> {
///         // Check for Cargo.toml in member directory
///         todo!()
///     }
/// }
/// ```
pub trait WorkspaceDiscovery {
    /// Discovers the complete workspace configuration from a root directory.
    ///
    /// This method should:
    /// 1. Locate the workspace configuration file (e.g., `package.json`, `Cargo.toml`)
    /// 2. Parse workspace member patterns
    /// 3. Find all workspace members
    /// 4. Validate each member
    /// 5. Build and return a complete [`Workspace`]
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The workspace configuration file is not found
    /// - The configuration is invalid
    /// - Members cannot be discovered or validated
    fn discover(&self, root: &Path) -> Result<Workspace>;

    /// Finds all workspace members using glob patterns or other discovery mechanisms.
    ///
    /// This method should scan the workspace root for members based on the
    /// package manager's conventions (e.g., `packages/*` for npm workspaces,
    /// `members = [...]` for Cargo).
    ///
    /// # Errors
    ///
    /// Returns an error if member discovery fails (e.g., invalid glob patterns,
    /// I/O errors).
    fn find_members(&self, root: &Path) -> Result<Vec<WorkspaceMember>>;

    /// Validates that a potential workspace member has the required manifest files.
    ///
    /// For example:
    /// - npm/Bun/pnpm/Yarn: Check for `package.json`
    /// - Cargo: Check for `Cargo.toml`
    ///
    /// # Errors
    ///
    /// Returns an error if validation cannot be performed (e.g., I/O errors).
    /// Returns `Ok(false)` if the member is invalid, `Ok(true)` if valid.
    fn validate_member(&self, member_path: &Path) -> Result<bool>;
}

/// Parses package manager-specific lockfiles into structured entries.
///
/// Each package manager has its own lockfile format:
/// - npm: `package-lock.json`
/// - Bun: `bun.lock` (JSONC format)
/// - pnpm: `pnpm-lock.yaml`
/// - Yarn Classic: `yarn.lock`
/// - Yarn Modern: `yarn.lock` (different format)
/// - Cargo: `Cargo.lock`
///
/// Implementations handle the parsing logic for these formats.
///
/// # Example
///
/// ```rust,ignore
/// use cuenv_workspaces::{LockfileParser, LockfileEntry};
/// use std::path::Path;
///
/// struct NpmLockfileParser;
///
/// impl LockfileParser for NpmLockfileParser {
///     fn parse(&self, lockfile_path: &Path) -> Result<Vec<LockfileEntry>> {
///         // Parse package-lock.json and convert to LockfileEntry structs
///         todo!()
///     }
///
///     fn supports_lockfile(&self, path: &Path) -> bool {
///         path.file_name()
///             .and_then(|n| n.to_str())
///             .map(|n| n == "package-lock.json")
///             .unwrap_or(false)
///     }
///
///     fn lockfile_name(&self) -> &str {
///         "package-lock.json"
///     }
/// }
/// ```
pub trait LockfileParser {
    /// Parses a lockfile into structured entries.
    ///
    /// Each entry represents a resolved dependency with its version, source,
    /// checksum, and direct dependencies.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The lockfile cannot be read
    /// - The lockfile format is invalid or corrupted
    /// - Required fields are missing
    fn parse(&self, lockfile_path: &Path) -> Result<Vec<LockfileEntry>>;

    /// Checks if this parser supports the given lockfile path.
    ///
    /// This is typically a simple filename check, but may involve inspecting
    /// file contents for formats that can't be distinguished by name alone.
    fn supports_lockfile(&self, path: &Path) -> bool;

    /// Returns the expected lockfile name for this parser.
    ///
    /// For example: `"package-lock.json"`, `"Cargo.lock"`, `"pnpm-lock.yaml"`.
    fn lockfile_name(&self) -> &str;
}
