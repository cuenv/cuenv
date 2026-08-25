pub mod config;
mod engine;
mod events;
mod host;
mod input;
mod interaction;
pub mod overlay;
pub mod scrollback;
pub mod search;
pub mod selection;
pub mod settings;
// P0 interchange surface; adapters will adopt it incrementally.
#[allow(dead_code)]
pub mod model;
mod presentation;
mod rio_adapter;
mod sizing;
pub mod workspace_shell;

pub use engine::run;
