//! A brief note at the foot of the terminal that something just happened, such as a copy.

use super::hover;
use super::tokens::{self, ChromeColors, Elevation, space, text};
use crate::terminal::engine::TerminalView;
use crate::terminal::presentation::TerminalTheme;
use gpui::{IntoElement, ParentElement, Styled};

impl TerminalView {
    /// Centred along the terminal's bottom edge, and never in the way of the pointer.
    pub(in crate::terminal) fn render_toast(
        &self,
        theme: &TerminalTheme,
    ) -> Option<gpui::AnyElement> {
        let message = self.toast_message()?;
        let colors = ChromeColors::from_theme(theme);
        let toast = tokens::elevated(gpui::div(), Elevation::Raised, &colors)
            .flex_none()
            .px(space::LG)
            .py(space::SM)
            .text_size(text::LABEL)
            .font_weight(text::MEDIUM)
            .text_color(colors.text)
            .line_height(text::LINE_HEIGHT)
            .child(message);
        Some(
            gpui::div()
                .absolute()
                .bottom(space::XL)
                .left(space::MD)
                .right(space::MD)
                .flex()
                .justify_center()
                .child(hover::fading_in(
                    toast,
                    "cuetty-toast-fade",
                    self.motion_pace(),
                ))
                .into_any_element(),
        )
    }
}
