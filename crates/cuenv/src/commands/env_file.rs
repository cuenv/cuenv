//! CUE module and env.cue file discovery utilities.
//!
//! This module re-exports shared discovery helpers from cuenv-core.

pub use cuenv_core::cue::discovery::{
    EnvFileStatus, discover_env_cue_directories, find_ancestor_env_files, find_cue_module_root,
    find_env_file,
};
