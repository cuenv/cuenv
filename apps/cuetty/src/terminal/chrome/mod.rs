//! Window chrome: the title strip, tab rail, task sidebar, and task palette.
//!
//! The terminal engine owns sessions and input; this module only turns view
//! state into GPUI elements around the terminal surface.

mod layout;
mod rail;
mod task_palette;
mod task_sidebar;
mod tokens;

use super::engine::TerminalView;
use crate::integrations::cuenv::Keybindings;

impl TerminalView {
    /// The active tab's task palette chord as shown in hints.
    fn palette_chord_label(&self) -> String {
        self.active_state().map_or_else(
            || Keybindings::default().palette.label(),
            |state| state.cuenv.presentation.keybindings.palette.label(),
        )
    }
}
