use super::tokens::{ChromeColors, border, elevation, radius, space, text};
use gpui::{
    AnyView, App, AppContext, Context, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div,
};

/// A tooltip painted from the chrome tokens. GPUI draws tooltips as their own
/// view, so this one carries the colours it was built with.
struct ChromeTooltip {
    label: SharedString,
    colors: ChromeColors,
}

impl Render for ChromeTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // The outer element keeps the margin from being absorbed by the
        // tooltip's own positioned root.
        div().child(
            div()
                .m(space::MD)
                .px(space::MD)
                .py(space::XS)
                .rounded(radius::CONTROL)
                .border(border::HAIRLINE)
                .border_color(self.colors.panel_outline)
                .bg(self.colors.panel)
                .shadow(elevation::overlay())
                .text_size(text::LABEL)
                .text_color(self.colors.text)
                .child(self.label.clone()),
        )
    }
}

/// A builder for `.tooltip(...)` showing `label` in the chrome's own style.
pub(super) fn chrome_tooltip(
    label: impl Into<SharedString>,
    colors: ChromeColors,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let label = label.into();
    move |_window, cx| {
        let label = label.clone();
        cx.new(|_cx| ChromeTooltip { label, colors }).into()
    }
}
