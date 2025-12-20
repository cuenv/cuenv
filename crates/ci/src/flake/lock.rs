//! Data structures for Nix flake.lock v7 format
//!
//! This module provides serde-compatible types for parsing flake.lock files.
//! The flake.lock format is JSON and contains a graph of locked flake inputs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Nix flake.lock file representation (version 7)
///
/// The flake.lock file contains a directed graph of flake inputs,
/// where each input can reference other inputs or follow paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FlakeLock {
    /// Version of the lockfile format (currently 7)
    pub version: u8,

    /// Root node identifier (usually "root")
    pub root: String,

    /// All flake input nodes indexed by name
    pub nodes: HashMap<String, FlakeNode>,
}

/// A node in the flake dependency graph
///
/// This is a unified representation that can be either a root node
/// (which just contains input references) or an input node
/// (which contains locked version information).
///
/// The distinction is made by checking if `locked` or `original` is present.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FlakeNode {
    /// Whether this is a non-flake input (file, tarball, etc.)
    /// Only present on input nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flake: Option<bool>,

    /// Locked (pinned) version information
    /// Only present on input nodes (None for root nodes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<LockedInfo>,

    /// Original input specification (before locking)
    /// Only present on input nodes (None for root nodes)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<OriginalInfo>,

    /// Transitive inputs - present on both root and input nodes
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub inputs: HashMap<String, InputRef>,
}

impl FlakeNode {
    /// Check if this is a root node (no locked or original info)
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.locked.is_none() && self.original.is_none() && self.flake.is_none()
    }

    /// Check if this is an input node (has locked or original info)
    #[must_use]
    pub fn is_input(&self) -> bool {
        self.locked.is_some() || self.original.is_some() || self.flake.is_some()
    }
}

/// Reference to another input node
///
/// Can be either a direct reference to a node name,
/// or a "follows" path to inherit from another input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum InputRef {
    /// Direct reference to another node by name
    Direct(String),
    /// Follows path - inherits input from another node
    /// e.g., `["nixpkgs"]` means follow root's nixpkgs
    Follows(Vec<String>),
}

/// Locked (pinned) version information for an input
///
/// This contains the exact version that was resolved when
/// `nix flake lock` was run. The `nar_hash` is the critical
/// field for ensuring reproducibility.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LockedInfo {
    /// Input type (github, gitlab, tarball, path, etc.)
    #[serde(rename = "type")]
    pub locked_type: String,

    /// Timestamp of last modification (Unix epoch)
    #[serde(rename = "lastModified")]
    pub last_modified: Option<i64>,

    /// Content hash - critical for purity verification
    /// Format: "sha256-<base64>" or similar
    #[serde(rename = "narHash")]
    pub nar_hash: Option<String>,

    /// Git revision hash (for github/gitlab/git types)
    pub rev: Option<String>,

    /// Repository owner (for github/gitlab types)
    pub owner: Option<String>,

    /// Repository name
    pub repo: Option<String>,

    /// URL (for tarball/url types)
    pub url: Option<String>,

    /// Revision count (for some tarball sources)
    #[serde(rename = "revCount")]
    pub rev_count: Option<i64>,
}

/// Original (unpinned) input specification
///
/// This represents how the input was specified in flake.nix
/// before being locked to a specific version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OriginalInfo {
    /// Input type (github, gitlab, tarball, path, etc.)
    #[serde(rename = "type")]
    pub original_type: String,

    /// Branch reference (e.g., "nixos-unstable")
    /// Presence of ref without rev in locked indicates unpinned
    #[serde(rename = "ref")]
    pub reference: Option<String>,

    /// Repository owner
    pub owner: Option<String>,

    /// Repository name
    pub repo: Option<String>,

    /// URL (for tarball/url types)
    pub url: Option<String>,
}

impl FlakeLock {
    /// Parse a flake.lock from JSON string
    ///
    /// # Errors
    /// Returns an error if the JSON is invalid or doesn't match the schema
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Get the root node
    #[must_use]
    pub fn root_node(&self) -> Option<&FlakeNode> {
        self.nodes.get(&self.root).filter(|node| node.is_root())
    }

    /// Get an input node by name
    #[must_use]
    pub fn get_input(&self, name: &str) -> Option<&FlakeNode> {
        self.nodes.get(name).filter(|node| node.is_input())
    }

    /// Get any node by name (root or input)
    #[must_use]
    pub fn get_node(&self, name: &str) -> Option<&FlakeNode> {
        self.nodes.get(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_flake_lock() {
        let json = r#"{
            "nodes": {
                "root": { "inputs": {} }
            },
            "root": "root",
            "version": 7
        }"#;

        let lock = FlakeLock::from_json(json).unwrap();
        assert_eq!(lock.version, 7);
        assert_eq!(lock.root, "root");
        assert!(lock.root_node().is_some());
    }

    #[test]
    fn test_parse_with_locked_input() {
        let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "owner": "NixOS",
                        "repo": "nixpkgs",
                        "rev": "abc123",
                        "narHash": "sha256-xxxxxxxxxxxxx"
                    },
                    "original": {
                        "type": "github",
                        "owner": "NixOS",
                        "repo": "nixpkgs"
                    }
                },
                "root": {
                    "inputs": { "nixpkgs": "nixpkgs" }
                }
            },
            "root": "root",
            "version": 7
        }"#;

        let lock = FlakeLock::from_json(json).unwrap();
        let input = lock.get_input("nixpkgs").unwrap();
        assert!(input.locked.is_some());
        let locked = input.locked.as_ref().unwrap();
        assert_eq!(locked.locked_type, "github");
        assert_eq!(locked.nar_hash.as_deref(), Some("sha256-xxxxxxxxxxxxx"));
    }

    #[test]
    fn test_parse_follows_reference() {
        let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "narHash": "sha256-abc"
                    }
                },
                "rust-overlay": {
                    "inputs": {
                        "nixpkgs": ["nixpkgs"]
                    },
                    "locked": {
                        "type": "github",
                        "narHash": "sha256-def"
                    }
                },
                "root": {
                    "inputs": {
                        "nixpkgs": "nixpkgs",
                        "rust-overlay": "rust-overlay"
                    }
                }
            },
            "root": "root",
            "version": 7
        }"#;

        let lock = FlakeLock::from_json(json).unwrap();
        let rust_overlay = lock.get_input("rust-overlay").unwrap();

        // Check that rust-overlay follows nixpkgs
        let nixpkgs_ref = rust_overlay.inputs.get("nixpkgs").unwrap();
        assert!(matches!(nixpkgs_ref, InputRef::Follows(path) if path == &["nixpkgs"]));
    }

    #[test]
    fn test_parse_non_flake_input() {
        let json = r#"{
            "nodes": {
                "advisory-db": {
                    "flake": false,
                    "locked": {
                        "type": "github",
                        "narHash": "sha256-xyz"
                    }
                },
                "root": {
                    "inputs": { "advisory-db": "advisory-db" }
                }
            },
            "root": "root",
            "version": 7
        }"#;

        let lock = FlakeLock::from_json(json).unwrap();
        let input = lock.get_input("advisory-db").unwrap();
        assert_eq!(input.flake, Some(false));
    }
}
