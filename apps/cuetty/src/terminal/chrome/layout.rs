use super::tokens::{
    ChromeColors, WindowControls, border, drawn_titlebar, size, space, terminal_top_inset, text,
    ui_font_family,
};
use crate::integrations::cuenv::{SidebarMode, TitlebarVisibility};
use crate::terminal::engine::TerminalView;
use crate::terminal::presentation::TerminalTheme;
use crate::terminal::tab_status::ExitFailure;
use gpui::{
    Div, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, SharedString, Styled,
    WindowControlArea, div, px,
};

/// A centred status line that stands in for the terminal when a pane has no
/// live session or failed.
pub(in crate::terminal) fn render_shell_message(
    message: impl Into<SharedString>,
    theme: &TerminalTheme,
) -> gpui::AnyElement {
    let colors = ChromeColors::from_theme(theme);
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(space::XL)
        .text_size(text::BODY)
        .text_color(colors.text_secondary)
        .child(message.into())
        .into_any_element()
}

/// Says below a failed shell's final output why nothing responds to typing any more,
/// and how to be rid of the tab. It has its own room, which [`TerminalView::bottom_inset`]
/// keeps the grid out of, because the last lines are what the tab stayed open to show.
pub(in crate::terminal) fn render_exit_notice(
    failure: ExitFailure,
    theme: &TerminalTheme,
) -> gpui::AnyElement {
    let colors = ChromeColors::from_theme(theme);
    div()
        .absolute()
        .bottom_0()
        .left_0()
        .right_0()
        .h(size::EXIT_NOTICE_HEIGHT)
        .flex()
        .items_center()
        .justify_center()
        .px(space::LG)
        .border_t(border::HAIRLINE)
        .border_color(colors.divider)
        .text_size(text::LABEL)
        .text_color(colors.text_secondary)
        .child(
            div()
                .min_w_0()
                .truncate()
                .child(format!("{} · Enter closes this tab", failure.summary())),
        )
        .into_any_element()
}

/// Makes `this` move the window when dragged. GPUI's control area covers platforms
/// that hit-test it, and Linux compositors have to be asked.
fn draggable(this: Div) -> Div {
    this.window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |_event, window, _cx| {
            window.start_window_move();
        })
}

impl TerminalView {
    fn titlebar_visibility(&self) -> TitlebarVisibility {
        drawn_titlebar(
            self.active_state()
                .map(|state| state.cuenv.presentation.titlebar)
                .unwrap_or_default(),
            WindowControls::current(),
            self.window_decorations,
        )
    }

    /// Where the terminal starts below the top of its panel.
    pub(in crate::terminal) fn top_inset(&self, padding: Pixels) -> Pixels {
        terminal_top_inset(
            padding,
            self.titlebar_visibility(),
            WindowControls::current(),
        )
    }

    /// Where the terminal stops above the bottom of its panel: past its padding, and past
    /// the exit notice while there is one.
    pub(in crate::terminal) fn bottom_inset(&self, padding: Pixels) -> Pixels {
        padding
            + self
                .active_state()
                .and_then(|state| state.status.failure())
                .map_or(px(0.0), |_| size::EXIT_NOTICE_HEIGHT)
    }

    /// The room left for native window controls, which moves the window as a title
    /// strip would. `None` while nothing is left there.
    pub(in crate::terminal) fn window_controls_band(&self, padding: Pixels) -> Option<Div> {
        (self.titlebar_visibility() == TitlebarVisibility::Hidden
            && WindowControls::current() == WindowControls::OverContent)
            .then(|| {
                draggable(div())
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(self.top_inset(padding))
            })
    }

    pub(in crate::terminal) fn render_shell_layout(
        &self,
        sidebar: gpui::AnyElement,
        content: gpui::AnyElement,
        theme: &TerminalTheme,
    ) -> gpui::AnyElement {
        let colors = ChromeColors::from_theme(theme);
        let titlebar_visibility = self.titlebar_visibility();
        // The sidebar floats inset from the window's edges, so its corners stay
        // concentric with a rounded window instead of meeting them in a hard line.
        let inset_above_sidebar = div().h(space::MD).flex_shrink_0();
        let sidebar_slot = div()
            .w(match self.sidebar_mode() {
                SidebarMode::Expanded => size::SIDEBAR_EXPANDED_WIDTH,
                SidebarMode::Collapsed => size::SIDEBAR_COLLAPSED_WIDTH,
            })
            .flex_shrink_0()
            .flex()
            .flex_col()
            .pr(space::MD)
            .pb(space::MD)
            // With no title strip this is all that is left to move the window by.
            .child(if titlebar_visibility == TitlebarVisibility::Hidden {
                draggable(inset_above_sidebar)
            } else {
                inset_above_sidebar
            })
            .child(sidebar);
        let workspace = div()
            .flex_1()
            .min_h(px(0.0))
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
            .child(sidebar_slot);
        // This root is the single background layer. The grid only fills whole
        // cells, so padding and whatever is left over past the last row and column
        // read as one continuous canvas with the chrome. Default-background cells
        // never repaint it, and no structural child may, or the alpha would compound.
        let shell = div()
            .size_full()
            .flex()
            .flex_col()
            .font_family(ui_font_family())
            .bg(colors.material);
        if titlebar_visibility == TitlebarVisibility::Hidden {
            // The terminal grid is resized from its measured bounds, so it reclaims
            // whatever the strip leaves without extra arithmetic here.
            return shell.child(workspace).into_any_element();
        }
        let title = self.registry.active_tab_label();
        // No fill and no rule: the strip is the top of the window's one material,
        // holding the window controls and saying which tab is showing.
        let titlebar = draggable(div())
            .h(size::TITLEBAR_HEIGHT)
            .w_full()
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            // Equal on both sides, so the title stays centred on the window.
            .px(size::WINDOW_CONTROLS_WIDTH)
            .text_size(text::LABEL)
            .text_color(colors.text_secondary)
            .children(title.map(|title| div().min_w_0().truncate().child(title)));
        shell.child(titlebar).child(workspace).into_any_element()
    }
}
