use super::tokens::{
    ChromeColors, WindowControls, border, drawn_titlebar, size, space, terminal_top_inset, text,
    ui_font_family,
};
use crate::integrations::cuenv::{SidebarMode, TitlebarVisibility};
use crate::terminal::engine::TerminalView;
use crate::terminal::motion::SidebarFrame;
use crate::terminal::presentation::TerminalTheme;
use crate::terminal::tab_status::ExitFailure;
use gpui::{
    Div, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Styled,
    WindowControlArea, div, px,
};

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

/// The room a tab's exit notice takes from the bottom of its surface.
pub(in crate::terminal) fn exit_notice_height(failure: Option<ExitFailure>) -> Pixels {
    failure.map_or(px(0.0), |_| size::EXIT_NOTICE_HEIGHT)
}

/// The width the sidebar settles at in a mode, along with the gap between it and the
/// window's edge.
pub(in crate::terminal) fn sidebar_width(mode: SidebarMode) -> f32 {
    f32::from(match mode {
        SidebarMode::Expanded => size::SIDEBAR_EXPANDED_WIDTH,
        SidebarMode::Collapsed => size::SIDEBAR_COLLAPSED_WIDTH,
    })
}

/// The sidebar as this draw has it, and how wide.
pub(in crate::terminal) struct ShellPanels {
    pub(in crate::terminal) sidebar: gpui::AnyElement,
    pub(in crate::terminal) sidebar_frame: SidebarFrame,
}

/// What the title strip signs the window with.
const WORDMARK: &str = "cuetty";

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
        padding + exit_notice_height(self.active_state().and_then(|state| state.status.failure()))
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
        panels: ShellPanels,
        content: gpui::AnyElement,
        theme: &TerminalTheme,
    ) -> gpui::AnyElement {
        let colors = ChromeColors::from_theme(theme);
        let titlebar_visibility = self.titlebar_visibility();
        // The sidebar floats inset from the window's edges, so its corners stay
        // concentric with a rounded window instead of meeting them in a hard line.
        let inset_above_sidebar = div().w_full().h(space::MD).flex_shrink_0();
        let ShellPanels {
            sidebar,
            sidebar_frame,
        } = panels;
        // The slot gives the layout the sidebar's room, which moves in one step. The panel
        // travels inside it against the window's edge, where the room it has not reached
        // yet is the same material as everything else.
        let sidebar_slot = div()
            .w(px(sidebar_frame.slot))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_end()
            .pr(space::MD)
            .pb(space::MD)
            // With no title strip this is all that is left to move the window by.
            .child(if titlebar_visibility == TitlebarVisibility::Hidden {
                draggable(inset_above_sidebar)
            } else {
                inset_above_sidebar
            })
            .child(
                div()
                    .w(px(sidebar_frame.panel) - space::MD)
                    .flex_1()
                    .min_h(px(0.0))
                    .flex()
                    .flex_col()
                    .child(sidebar),
            );
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
        // Sits in the margin the title leaves for the window controls' sake, over against
        // them, so it can move neither the title nor the controls.
        let wordmark = div()
            .absolute()
            .top_0()
            .bottom_0()
            .right(space::LG)
            .flex()
            .items_center()
            .text_size(text::LABEL)
            .font_weight(text::MEDIUM)
            .text_color(colors.text_muted)
            .child(WORDMARK);
        // No fill and no rule: the strip is the top of the window's one material,
        // holding the window controls and saying which tab is showing.
        let titlebar = draggable(div())
            .relative()
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
            .children(title.map(|title| div().min_w_0().truncate().child(title)))
            .child(wordmark);
        shell.child(titlebar).child(workspace).into_any_element()
    }
}
