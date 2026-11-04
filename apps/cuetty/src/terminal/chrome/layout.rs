use super::tokens::{ChromeColors, border, material_color, size, space, text};
use crate::integrations::cuenv::TitlebarVisibility;
use crate::terminal::engine::TerminalView;
use crate::terminal::presentation::TerminalTheme;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Styled, WindowControlArea, div, px,
};

impl TerminalView {
    pub(in crate::terminal) fn render_shell_layout(
        &self,
        sidebar: gpui::AnyElement,
        content: gpui::AnyElement,
        theme: &TerminalTheme,
        _cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = ChromeColors::from_theme(theme);
        let workspace = div()
            .flex_1()
            .flex()
            // The content panel must establish a flex formatting context.
            // Otherwise a child's `flex_1` has no parent flex layout during
            // the first window pass and AppKit's white backing surface leaks
            // through until a later resize happens to trigger a relayout.
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .min_h(px(0.0))
                    .flex()
                    // This panel is the single terminal background layer. The
                    // grid only fills whole cells, so padding and whatever is
                    // left over past the last row and column read as one
                    // continuous canvas. Default-background cells never repaint
                    // it, and no child may, or the alpha would compound.
                    .bg(material_color(theme, theme.surface))
                    .child(content),
            )
            .child(div().w(size::RAIL_WIDTH).flex_shrink_0().child(sidebar));
        let titlebar_visibility = self
            .active_state()
            .map(|state| state.cuenv.presentation.titlebar)
            .unwrap_or_default();
        let shell = div().size_full().flex().flex_col();
        if titlebar_visibility == TitlebarVisibility::Hidden {
            // The terminal grid is resized from its measured bounds, so it
            // reclaims the strip's height without extra arithmetic here.
            return shell.child(workspace).into_any_element();
        }
        let titlebar = div()
            .h(size::TITLEBAR_HEIGHT)
            .w_full()
            .flex()
            .items_center()
            .bg(colors.material)
            .border_b(border::HAIRLINE)
            .border_color(colors.divider)
            .child(
                div()
                    .h_full()
                    .w(size::WINDOW_CONTROLS_WIDTH)
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(
                div()
                    .h_full()
                    .px(space::MD)
                    .flex()
                    .items_center()
                    .text_size(text::BODY)
                    .text_color(colors.text)
                    .child("Cuetty"),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            );
        shell.child(titlebar).child(workspace).into_any_element()
    }
}
