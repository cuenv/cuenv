//! Cuetty GPUI proof of concept.
//!
//! The production boundary is deliberately small:
//! `rio-vt` (PTY + VT state) -> host event adapter -> GPUI entity.

pub mod integrations;
pub mod terminal;
pub mod workspace;

fn main() {
    terminal::run();
}
