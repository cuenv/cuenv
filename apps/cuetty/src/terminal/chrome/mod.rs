//! Window chrome: the title strip, tab rail, task sidebar, and task palette.
//!
//! The terminal engine owns sessions and input; this module only turns view
//! state into GPUI elements around the terminal surface.

mod layout;
mod rail;
mod task_palette;
mod task_sidebar;
mod tokens;

pub(super) use tokens::material_color;
