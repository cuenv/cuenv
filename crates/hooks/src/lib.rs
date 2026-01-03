//! Hook execution system for cuenv
//!
//! This crate provides the core hook execution functionality, including:
//! - Hook definition and serialization
//! - Background hook execution with state tracking
//! - Approval-based security system
//! - Shell integration support
//!
//! # Overview
//!
//! The hooks system enables environment-triggered command execution:
//! - `onEnter` hooks run when entering a directory with a cuenv configuration
//! - `onExit` hooks run when leaving such a directory
//! - `prePush` hooks run before git push operations
//!
//! # Security
//!
//! All hook configurations must be approved by the user before execution.
//! This prevents malicious configurations from executing arbitrary commands.
//! In CI environments, hooks are auto-approved since the environment is
//! assumed to be already secured.

// Placeholder module structure - actual code will be migrated in subsequent steps
