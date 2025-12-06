//! Core traits for workspace discovery, lockfile parsing, and dependency resolution.

use crate::core::types::{DependencyRef, LockfileEntry, Workspace, WorkspaceMember};
use crate::error::Result;
use std::path::Path;

/// Type alias for dependency graphs using petgraph.
///
/// The graph nodes are [`DependencyRef`]s representing packages, and edges
/// represent dependency relationships (no edge data needed).
pub type DependencyGraph = petgraph::Graph<DependencyRef, ()>;

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

/// Builds dependency graphs from workspace and lockfile data.
///
/// This trait combines workspace-internal dependencies (workspace protocol references)
/// with external dependencies from lockfiles to build a complete dependency graph.
///
/// # Example
///
/// ```rust,ignore
/// use cuenv_workspaces::{DependencyResolver, Workspace, LockfileEntry};
///
/// struct NpmDependencyResolver;
///
/// impl DependencyResolver for NpmDependencyResolver {
///     fn resolve_dependencies(
///         &self,
///         workspace: &Workspace,
///         lockfile: &[LockfileEntry],
///     ) -> Result<DependencyGraph> {
///         // Build graph with both workspace and external dependencies
///         todo!()
///     }
///
///     fn resolve_workspace_deps(&self, workspace: &Workspace) -> Result<Vec<DependencyRef>> {
///         // Find dependencies using "workspace:*" protocol
///         todo!()
///     }
///
///     fn resolve_external_deps(&self, lockfile: &[LockfileEntry]) -> Result<Vec<DependencyRef>> {
///         // Extract all external dependencies from lockfile
///         todo!()
///     }
///
///     fn detect_workspace_protocol(&self, spec: &str) -> bool {
///         spec.starts_with("workspace:")
///     }
/// }
/// ```
pub trait DependencyResolver {
    /// Builds a complete dependency graph from workspace and lockfile data.
    ///
    /// The graph should include:
    /// - Workspace members as nodes
    /// - Workspace-internal dependencies (workspace protocol)
    /// - External dependencies from the lockfile
    /// - Edges representing dependency relationships
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Dependencies cannot be resolved
    /// - Circular dependencies are detected (if validation is enabled)
    /// - Required dependencies are missing from the lockfile
    fn resolve_dependencies(
        &self,
        workspace: &Workspace,
        lockfile: &[LockfileEntry],
    ) -> Result<DependencyGraph>;

    /// Resolves workspace-internal dependencies.
    ///
    /// These are dependencies that reference other workspace members using
    /// package manager-specific protocols:
    /// - npm/pnpm/Yarn: `"workspace:*"`, `"workspace:^"`, etc.
    /// - Cargo: `{ workspace = true }`
    ///
    /// # Errors
    ///
    /// Returns an error if workspace dependencies cannot be resolved.
    fn resolve_workspace_deps(&self, workspace: &Workspace) -> Result<Vec<DependencyRef>>;

    /// Resolves external dependencies from lockfile entries.
    ///
    /// Extracts all non-workspace dependencies from the lockfile.
    ///
    /// # Errors
    ///
    /// Returns an error if dependencies cannot be extracted or validated.
    fn resolve_external_deps(&self, lockfile: &[LockfileEntry]) -> Result<Vec<DependencyRef>>;

    /// Detects if a dependency specification uses the workspace protocol.
    ///
    /// # Examples
    ///
    /// - npm/pnpm/Yarn: `"workspace:*"` → `true`
    /// - Cargo: Requires parsing TOML to check `workspace = true`
    /// - Regular semver: `"^1.0.0"` → `false`
    fn detect_workspace_protocol(&self, spec: &str) -> bool;
}
