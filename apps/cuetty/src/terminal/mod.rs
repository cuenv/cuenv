mod chrome;
mod color;
pub mod config;
mod cursor;
mod engine;
mod events;
mod foreground;
mod git_branch;
mod glyphs;
mod host;
mod input;
mod interaction;
mod motion;
pub mod overlay;
pub mod scrollback;
pub mod search;
pub mod selection;
// P0 interchange surface; adapters will adopt it incrementally.
#[allow(dead_code)]
pub mod model;
mod presentation;
mod rio_adapter;
mod rio_input;
mod sizing;
mod tab_label;
mod tab_rename;
mod tab_status;
pub mod workspace_shell;

pub use engine::run;
