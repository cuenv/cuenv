//! A task discovered from a file in a project's `taskScripts` directory.

use serde::{Deserialize, Serialize};

/// The file a discovered task runs, and the interpreter chosen for it.
///
/// Written by cuenv when it discovers the task, never by CUE. A task that
/// carries one receives the command line's arguments verbatim after the
/// script path, instead of through `params` interpolation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScriptFile {
    /// The script's path relative to the project directory.
    pub path: String,
    /// The program that runs it, such as `bash` or `python3`.
    pub interpreter: String,
}
