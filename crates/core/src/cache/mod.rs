//! Cache infrastructure for task execution
//!
//! Provides content-addressable storage (CAS) for task outputs with support for
//! shared local caching across git branches.
//!
//! ## Architecture
//!
//! The cache system consists of three main components:
//!
//! ### 1. Content-Addressable Storage (CAS)
//!
//! Task outputs are stored in a content-addressable blob store at `~/.cache/cuenv/cas/`.
//! Each file is stored by its SHA-256 hash using a two-level directory structure:
//!
//! ```text
//! ~/.cache/cuenv/cas/
//!   ab/
//!     cd/
//!       abcdef123456...  (actual blob)
//! ```
//!
//! Benefits:
//! - **Deduplication**: Identical files stored once regardless of which task produced them
//! - **Integrity**: SHA-256 verification on read prevents corruption
//! - **Cross-branch sharing**: Content-addressed, not branch-specific
//! - **Efficient lookup**: Two-level structure avoids filesystem bottlenecks
//!
//! ### 2. Task Cache Entries
//!
//! Each task execution is cached with a unique key computed from:
//! - Input file hashes
//! - Command and arguments
//! - Environment variables
//! - Platform and cuenv version
//! - Workspace lockfile hashes
//!
//! Cache entries are stored at `~/.cache/cuenv/tasks/{cache_key}/` containing:
//! - `metadata.json`: Task metadata with output index
//! - `outputs/`: Traditional output files (backward compatibility)
//! - `logs/`: stdout/stderr logs
//! - `workspace.tar.zst`: Hermetic workspace snapshot
//!
//! ### 3. Task Index
//!
//! A project-level index tracks the latest cache key for each task, enabling fast
//! cache lookups and protecting recent entries during garbage collection.
//!
//! ## Cache Invalidation
//!
//! Cache keys are automatically invalidated when:
//! - Input files change (detected via SHA-256 hashes)
//! - Command or arguments change
//! - Environment variables change
//! - Platform changes
//! - cuenv version changes
//! - Workspace dependencies change (lockfile hashes)
//!
//! ## Garbage Collection
//!
//! The `gc` module provides configurable cleanup:
//! - Age-based: Remove entries older than N days (default: 30)
//! - Size-based: Remove oldest entries when total size exceeds limit
//! - Protection: Always keep latest entries per task
//! - Orphan cleanup: Remove unreferenced CAS blobs
//!
//! ## Usage
//!
//! ```rust,no_run
//! use cuenv_core::cache::tasks::{save_result, materialize_outputs, cas_stats};
//! use cuenv_core::cache::gc::{gc, GcPolicy};
//!
//! // Save task results to cache
//! # use std::path::Path;
//! # use cuenv_core::cache::tasks::{TaskResultMeta, TaskLogs};
//! # let key = "cache-key";
//! # let meta = TaskResultMeta {
//! #     task_name: "build".into(),
//! #     command: "cargo".into(),
//! #     args: vec!["build".into()],
//! #     env_summary: Default::default(),
//! #     inputs_summary: Default::default(),
//! #     created_at: chrono::Utc::now(),
//! #     cuenv_version: "0.21.0".into(),
//! #     platform: "linux-x86_64".into(),
//! #     duration_ms: 1000,
//! #     exit_code: 0,
//! #     cache_key_envelope: serde_json::json!({}),
//! #     output_index: vec![],
//! # };
//! # let outputs_root = Path::new("/tmp/outputs");
//! # let hermetic_root = Path::new("/tmp/hermetic");
//! # let logs = TaskLogs { stdout: None, stderr: None };
//! save_result(key, &meta, outputs_root, hermetic_root, logs, None)?;
//!
//! // Restore outputs from cache
//! # let destination = Path::new("/tmp/dest");
//! let count = materialize_outputs(key, destination, None)?;
//!
//! // Get cache statistics
//! let stats = cas_stats(None)?;
//! println!("Cache size: {}", stats.human_size);
//!
//! // Run garbage collection
//! let policy = GcPolicy::default();
//! let result = gc(None, &policy)?;
//! println!("Freed {} bytes", result.bytes_freed);
//! # Ok::<(), cuenv_core::Error>(())
//! ```

pub mod cas;
pub mod gc;
pub mod tasks;
