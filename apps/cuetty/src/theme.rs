use gpui::{Rgba, rgba};

pub(crate) struct Theme;

impl Theme {
    pub const BG: u32 = 0x131517;
    pub const PANEL: u32 = 0x181a1d;
    pub const RULE: u32 = 0x24272c;
    pub const INK: u32 = 0xe9e3d4;
    pub const INK_2: u32 = 0xb9b3a5;
    pub const SUB: u32 = 0x7d7669;
    pub const SUB_2: u32 = 0x524d44;
    pub const ACCENT: u32 = 0xf5a623;

    pub fn hover_bg() -> Rgba {
        rgba(0xffffff06)
    }

    pub fn active_bg() -> Rgba {
        rgba(0xffffff0a)
    }
}
