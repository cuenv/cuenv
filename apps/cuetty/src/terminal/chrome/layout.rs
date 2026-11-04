use super::{SHELL_TITLEBAR_HEIGHT, TAB_RAIL_WIDTH, chrome_glass_color};
use crate::integrations::cuenv::TitlebarVisibility;
use crate::terminal::engine::TerminalView;
use crate::terminal::presentation::{TerminalTheme, gpui_rgb};
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
                    .child(content),
            )
            .child(div().w(px(TAB_RAIL_WIDTH)).flex_shrink_0().child(sidebar));
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
            .h(px(SHELL_TITLEBAR_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .bg(chrome_glass_color(theme.title_surface))
            .border_b_1()
            .border_color(gpui::rgb(gpui_rgb(theme.host_background)))
            .child(
                div()
                    .h_full()
                    .w(px(82.0))
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(
                div()
                    .h_full()
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .text_size(px(13.0))
                    .text_color(gpui::rgb(gpui_rgb(theme.text)))
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
