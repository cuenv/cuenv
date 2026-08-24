//! Cuetty GPUI proof of concept.
//!
//! The production boundary is deliberately small:
//! `librio` (PTY + VT state) -> host event adapter -> GPUI entity.

pub mod integrations;
pub mod terminal;
pub mod workspace;

fn main() {
    // librio's child shell inherits this process environment. Keep the
    // identity stable for shell integrations and terminal-aware tools.
    unsafe {
        std::env::set_var("TERM_PROGRAM", "cuetty");
        std::env::set_var("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    }
    terminal::run();
}
