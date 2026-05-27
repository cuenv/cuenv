use std::path::{Path, PathBuf};

use anyhow::Result;
use gpui::{
    AnyElement, Context as GpuiContext, FocusHandle, FontWeight, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Render, SharedString, Styled, Window, div, px, rgb,
};

use crate::pty::TerminalProcessOptions;
use crate::theme::Theme;
use crate::{CloseTab, FocusNextPane, NewTab, SplitDown, SplitRight};

mod layout;
mod terminal;

use layout::{DIVIDER_THICKNESS, PaneNode, SplitAxis, TerminalId, TerminalTab};
use terminal::{
    TerminalLaunchOptions, TerminalPane, install_resize_observer, launch_terminal, resize_layout,
};

const TITLEBAR_HEIGHT: f32 = 40.0;
const SIDEBAR_WIDTH: f32 = 220.0;
// Reserve space inside the titlebar for the macOS traffic lights overlay.
const TITLEBAR_LEFT_INSET: f32 = 80.0;
const CLOSE_HOVER_COLOR: u32 = 0xd35a4e;

pub struct RootView {
    tabs: Vec<TerminalTab>,
    panes: Vec<TerminalPane>,
    active_tab: usize,
    next_terminal_id: u64,
    process_options: TerminalProcessOptions,
    error: Option<String>,
    focus_handle: FocusHandle,
    startup_cwd: Option<PathBuf>,
}

impl RootView {
    pub fn new(
        window: &mut Window,
        process_options: TerminalProcessOptions,
        cx: &mut GpuiContext<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        let mut root = Self {
            tabs: Vec::new(),
            panes: Vec::new(),
            active_tab: 0,
            next_terminal_id: 1,
            process_options,
            error: None,
            focus_handle,
            startup_cwd: std::env::current_dir().ok(),
        };

        root.open_tab(window, cx);
        install_resize_observer(window, cx);
        root.resize_active_layout(window, cx);
        root
    }

    fn focus_active_pane(&self, window: &mut Window, cx: &mut GpuiContext<Self>) {
        if let Some(handle) = self.active_pane_focus_handle() {
            handle.focus(window, cx);
        } else {
            self.focus_handle.focus(window, cx);
        }
    }

    fn active_pane_focus_handle(&self) -> Option<FocusHandle> {
        let tab = self.tabs.get(self.active_tab)?;
        self.panes
            .iter()
            .find(|pane| pane.id == tab.active_pane)
            .map(|pane| pane.focus_handle.clone())
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut GpuiContext<Self>) -> impl IntoElement {
        div()
            .id("cuetty-root")
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(Theme::BG))
            .text_color(rgb(Theme::INK))
            .on_action(cx.listener(Self::handle_new_tab))
            .on_action(cx.listener(Self::handle_close_tab))
            .on_action(cx.listener(Self::handle_split_right))
            .on_action(cx.listener(Self::handle_split_down))
            .on_action(cx.listener(Self::handle_focus_next_pane))
            .child(self.render_titlebar(cx))
            .child(
                div()
                    .id("cuetty-shell")
                    .flex_1()
                    .flex()
                    .flex_row()
                    .child(self.render_sidebar(cx))
                    .child(self.render_workspace(cx)),
            )
    }
}

impl RootView {
    fn handle_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut GpuiContext<Self>) {
        self.open_tab(window, cx);
    }

    fn handle_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut GpuiContext<Self>) {
        self.close_active_tab(window, cx);
    }

    fn handle_split_right(
        &mut self,
        _: &SplitRight,
        window: &mut Window,
        cx: &mut GpuiContext<Self>,
    ) {
        self.split_active_pane(SplitAxis::Row, window, cx);
    }

    fn handle_split_down(
        &mut self,
        _: &SplitDown,
        window: &mut Window,
        cx: &mut GpuiContext<Self>,
    ) {
        self.split_active_pane(SplitAxis::Column, window, cx);
    }

    fn handle_focus_next_pane(
        &mut self,
        _: &FocusNextPane,
        window: &mut Window,
        cx: &mut GpuiContext<Self>,
    ) {
        self.focus_next_pane(window, cx);
    }

    fn open_tab(&mut self, window: &mut Window, cx: &mut GpuiContext<Self>) {
        let terminal_id = self.allocate_terminal_id();
        match self.spawn_terminal(terminal_id, window, cx) {
            Ok(pane) => {
                self.panes.push(pane);
                self.tabs.push(TerminalTab {
                    title: format!("Tab {}", self.tabs.len() + 1),
                    layout: PaneNode::Leaf(terminal_id),
                    active_pane: terminal_id,
                });
                self.active_tab = self.tabs.len().saturating_sub(1);
                self.error = None;
                self.focus_active_pane(window, cx);
                self.resize_active_layout(window, cx);
                cx.notify();
            }
            Err(error) => self.record_error(error, cx),
        }
    }

    fn close_active_tab(&mut self, window: &mut Window, cx: &mut GpuiContext<Self>) {
        if self.tabs.len() <= 1 {
            return;
        }

        let removed = self.tabs.remove(self.active_tab);
        self.remove_panes_for_layout(&removed.layout);
        self.active_tab = self.active_tab.min(self.tabs.len().saturating_sub(1));
        self.focus_active_pane(window, cx);
        self.resize_active_layout(window, cx);
        cx.notify();
    }

    fn split_active_pane(
        &mut self,
        axis: SplitAxis,
        window: &mut Window,
        cx: &mut GpuiContext<Self>,
    ) {
        let Some(active_index) = self.active_tab() else {
            return;
        };
        let active_pane = self.tabs[active_index]
            .layout
            .active_or_first_leaf(self.tabs[active_index].active_pane);
        let Some(active_pane) = active_pane else {
            return;
        };

        let terminal_id = self.allocate_terminal_id();
        match self.spawn_terminal(terminal_id, window, cx) {
            Ok(pane) => {
                self.panes.push(pane);
                if self.tabs[active_index]
                    .layout
                    .split_leaf(active_pane, axis, terminal_id)
                {
                    self.tabs[active_index].active_pane = terminal_id;
                    self.error = None;
                    self.focus_active_pane(window, cx);
                    self.resize_active_layout(window, cx);
                    cx.notify();
                } else {
                    self.panes.retain(|pane| pane.id != terminal_id);
                }
            }
            Err(error) => self.record_error(error, cx),
        }
    }

    fn focus_next_pane(&mut self, window: &mut Window, cx: &mut GpuiContext<Self>) {
        let Some(active_index) = self.active_tab() else {
            return;
        };

        let mut panes = Vec::new();
        self.tabs[active_index].layout.leaf_ids(&mut panes);
        if panes.is_empty() {
            return;
        }

        let current = self.tabs[active_index].active_pane;
        let next_index = panes
            .iter()
            .position(|id| *id == current)
            .map(|index| (index + 1) % panes.len())
            .unwrap_or(0);
        self.tabs[active_index].active_pane = panes[next_index];
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    fn activate_tab(&mut self, tab_index: usize, window: &mut Window, cx: &mut GpuiContext<Self>) {
        if tab_index >= self.tabs.len() {
            return;
        }

        self.active_tab = tab_index;
        self.focus_active_pane(window, cx);
        self.resize_active_layout(window, cx);
        cx.notify();
    }

    fn close_tab(&mut self, tab_index: usize, window: &mut Window, cx: &mut GpuiContext<Self>) {
        if self.tabs.len() <= 1 || tab_index >= self.tabs.len() {
            return;
        }

        let removed = self.tabs.remove(tab_index);
        self.remove_panes_for_layout(&removed.layout);
        if self.active_tab >= tab_index {
            self.active_tab = self.active_tab.saturating_sub(1);
        }
        self.active_tab = self.active_tab.min(self.tabs.len().saturating_sub(1));
        self.focus_active_pane(window, cx);
        self.resize_active_layout(window, cx);
        cx.notify();
    }

    fn render_titlebar(&self, cx: &mut GpuiContext<Self>) -> AnyElement {
        let tab_label = self.titlebar_tab_label();
        let path = self.titlebar_path_string();

        let mut location = div().flex().items_center().gap_2().text_xs();
        location = location.child(
            div()
                .text_color(rgb(Theme::INK_2))
                .child(SharedString::from(tab_label)),
        );
        if !path.is_empty() {
            location = location
                .child(div().text_color(rgb(Theme::SUB_2)).child("·"))
                .child(
                    div()
                        .text_color(rgb(Theme::SUB))
                        .child(SharedString::from(path)),
                );
        }

        div()
            .id("cuetty-titlebar")
            .h(px(TITLEBAR_HEIGHT))
            .flex()
            .items_center()
            .gap_4()
            .pl(px(TITLEBAR_LEFT_INSET))
            .pr(px(12.0))
            .bg(rgb(Theme::BG))
            .border_b_1()
            .border_color(rgb(Theme::RULE))
            .child(
                div()
                    .text_color(rgb(Theme::INK))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_sm()
                    .child("cuetty"),
            )
            .child(location)
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(self.render_titlebar_action(TitleAction::NewTab, "+", cx))
                    .child(self.render_titlebar_action(TitleAction::SplitRight, "|", cx))
                    .child(self.render_titlebar_action(TitleAction::SplitDown, "—", cx)),
            )
            .into_any_element()
    }

    fn render_titlebar_action(
        &self,
        action: TitleAction,
        label: &'static str,
        cx: &mut GpuiContext<Self>,
    ) -> AnyElement {
        let id: &'static str = match action {
            TitleAction::NewTab => "cuetty-title-new-tab",
            TitleAction::SplitRight => "cuetty-title-split-right",
            TitleAction::SplitDown => "cuetty-title-split-down",
        };
        let hover_color = match action {
            TitleAction::NewTab => Theme::ACCENT,
            TitleAction::SplitRight | TitleAction::SplitDown => Theme::INK,
        };

        div()
            .id(SharedString::from(id))
            .w(px(28.0))
            .h(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .text_color(rgb(Theme::SUB))
            .text_sm()
            .cursor_pointer()
            .hover(move |style| style.bg(Theme::hover_bg()).text_color(rgb(hover_color)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                    match action {
                        TitleAction::NewTab => this.open_tab(window, cx),
                        TitleAction::SplitRight => {
                            this.split_active_pane(SplitAxis::Row, window, cx)
                        }
                        TitleAction::SplitDown => {
                            this.split_active_pane(SplitAxis::Column, window, cx)
                        }
                    }
                    cx.stop_propagation();
                }),
            )
            .child(label)
            .into_any_element()
    }

    fn render_sidebar(&self, cx: &mut GpuiContext<Self>) -> AnyElement {
        let mut list = div().flex().flex_col().gap_1();
        for (index, tab) in self.tabs.iter().enumerate() {
            list = list.child(self.render_tab_row(TabRowInput { index, tab }, cx));
        }

        div()
            .id("cuetty-sidebar")
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .bg(rgb(Theme::PANEL))
            .border_r_1()
            .border_color(rgb(Theme::RULE))
            .px(px(8.0))
            .py(px(12.0))
            .child(list)
            .into_any_element()
    }

    fn render_tab_row(&self, input: TabRowInput<'_>, cx: &mut GpuiContext<Self>) -> AnyElement {
        let index = input.index;
        let active = index == self.active_tab;
        let pane_count = input.tab.layout.leaf_count();
        let title = input.tab.title.clone();

        let num_color = if active { Theme::ACCENT } else { Theme::SUB };
        let name_color = if active { Theme::INK } else { Theme::INK_2 };

        let sub_text = if pane_count <= 1 {
            "1 pane".to_string()
        } else {
            format!("{pane_count} panes")
        };

        let mut strip = div().w(px(2.0)).h(px(28.0)).rounded(px(1.0));
        if active {
            strip = strip.bg(rgb(Theme::ACCENT));
        }

        let mut row = div()
            .id(SharedString::from(format!("cuetty-tab-{index}")))
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px(px(10.0))
            .py(px(10.0))
            .rounded(px(4.0))
            .cursor_pointer()
            .child(strip)
            .child(
                div()
                    .w(px(20.0))
                    .text_color(rgb(num_color))
                    .text_xs()
                    .child(format!("{:02}", index + 1)),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_color(rgb(name_color))
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_color(rgb(Theme::SUB_2))
                            .text_xs()
                            .child(SharedString::from(sub_text)),
                    ),
            )
            .child(self.render_tab_close(index, cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                    this.activate_tab(index, window, cx);
                    cx.stop_propagation();
                }),
            );

        if active {
            row = row.bg(Theme::active_bg());
        } else {
            row = row.hover(|style| style.bg(Theme::hover_bg()));
        }

        row.into_any_element()
    }

    fn render_tab_close(&self, tab_index: usize, cx: &mut GpuiContext<Self>) -> AnyElement {
        div()
            .id(SharedString::from(format!("cuetty-tab-close-{tab_index}")))
            .w(px(18.0))
            .h(px(18.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(3.0))
            .text_xs()
            .text_color(rgb(Theme::SUB_2))
            .cursor_pointer()
            .hover(|style| style.text_color(rgb(CLOSE_HOVER_COLOR)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                    this.close_tab(tab_index, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child("×")
            .into_any_element()
    }

    fn render_workspace(&self, cx: &mut GpuiContext<Self>) -> AnyElement {
        if let Some(message) = self.error.as_ref() {
            return self.render_error(message).into_any_element();
        }

        let Some(tab) = self.tabs.get(self.active_tab) else {
            return self
                .render_error("Cuetty has no terminal tabs.")
                .into_any_element();
        };

        div()
            .id("cuetty-workspace")
            .flex_1()
            .h_full()
            .overflow_hidden()
            .bg(rgb(Theme::BG))
            .child(self.render_layout(&tab.layout, tab.active_pane, cx))
            .into_any_element()
    }

    fn render_layout(
        &self,
        node: &PaneNode,
        active_pane: TerminalId,
        cx: &mut GpuiContext<Self>,
    ) -> AnyElement {
        match node {
            PaneNode::Leaf(id) => self.render_pane(*id, active_pane, cx),
            PaneNode::Split {
                axis,
                first,
                second,
            } => {
                let mut container = div().flex_1().size_full().flex().overflow_hidden();
                container = match axis {
                    SplitAxis::Row => container.flex_row(),
                    SplitAxis::Column => container.flex_col(),
                };

                container
                    .child(self.render_layout(first, active_pane, cx))
                    .child(split_divider(*axis))
                    .child(self.render_layout(second, active_pane, cx))
                    .into_any_element()
            }
        }
    }

    fn render_pane(
        &self,
        terminal_id: TerminalId,
        active_pane: TerminalId,
        cx: &mut GpuiContext<Self>,
    ) -> AnyElement {
        let Some(pane) = self.pane(terminal_id) else {
            return self
                .render_error("Cuetty lost a terminal pane.")
                .into_any_element();
        };

        let active = terminal_id == active_pane;
        let border = if active {
            rgb(Theme::ACCENT)
        } else {
            rgb(Theme::RULE)
        };

        div()
            .id(SharedString::from(format!(
                "cuetty-pane-{}",
                terminal_id.as_u64()
            )))
            .flex_1()
            .size_full()
            .overflow_hidden()
            .border_1()
            .border_color(border)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                    // Intentionally not stopping propagation: the underlying
                    // TerminalView needs the mouse-down to start text selection.
                    this.activate_pane(terminal_id, window, cx);
                }),
            )
            .child(pane.view.clone())
            .into_any_element()
    }

    fn render_error(&self, message: &str) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .items_center()
            .gap_3()
            .px(px(24.0))
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Cuetty could not start"),
            )
            .child(
                div()
                    .max_w(px(640.0))
                    .text_center()
                    .text_color(rgb(Theme::SUB))
                    .child(message.to_string()),
            )
            .into_any_element()
    }

    fn activate_pane(
        &mut self,
        terminal_id: TerminalId,
        window: &mut Window,
        cx: &mut GpuiContext<Self>,
    ) {
        let Some(tab) = self.tabs.get_mut(self.active_tab) else {
            return;
        };
        if tab.layout.contains(terminal_id) {
            tab.active_pane = terminal_id;
            self.focus_active_pane(window, cx);
            cx.notify();
        }
    }

    fn resize_active_layout(&mut self, window: &mut Window, cx: &mut GpuiContext<Self>) {
        let Some(tab) = self.tabs.get(self.active_tab) else {
            return;
        };
        resize_layout(&tab.layout, &mut self.panes, window, cx);
    }

    fn spawn_terminal(
        &mut self,
        terminal_id: TerminalId,
        window: &mut Window,
        cx: &mut GpuiContext<Self>,
    ) -> Result<TerminalPane> {
        launch_terminal(
            window,
            TerminalLaunchOptions {
                terminal_id,
                process_options: self.process_options.clone(),
            },
            cx,
        )
    }

    fn remove_panes_for_layout(&mut self, layout: &PaneNode) {
        let mut removed = Vec::new();
        layout.leaf_ids(&mut removed);
        self.panes.retain(|pane| !removed.contains(&pane.id));
    }

    fn record_error(&mut self, error: anyhow::Error, cx: &mut GpuiContext<Self>) {
        tracing::error!(%error, "failed to launch terminal");
        self.error = Some(error.to_string());
        cx.notify();
    }

    fn active_tab(&self) -> Option<usize> {
        (self.active_tab < self.tabs.len()).then_some(self.active_tab)
    }

    fn allocate_terminal_id(&mut self) -> TerminalId {
        let id = TerminalId::new(self.next_terminal_id);
        self.next_terminal_id += 1;
        id
    }

    fn pane(&self, terminal_id: TerminalId) -> Option<&TerminalPane> {
        self.panes.iter().find(|pane| pane.id == terminal_id)
    }

    fn titlebar_tab_label(&self) -> String {
        self.tabs
            .get(self.active_tab)
            .map(|tab| format!("{:02} {}", self.active_tab + 1, tab.title))
            .unwrap_or_default()
    }

    fn titlebar_path_string(&self) -> String {
        self.startup_cwd
            .as_deref()
            .map(collapse_home)
            .unwrap_or_default()
    }

    fn handle_pane_exit(
        &mut self,
        terminal_id: TerminalId,
        window: &mut Window,
        cx: &mut GpuiContext<Self>,
    ) {
        self.panes.retain(|pane| pane.id != terminal_id);

        let Some(tab_index) = self
            .tabs
            .iter()
            .position(|tab| tab.layout.contains(terminal_id))
        else {
            return;
        };

        let owned_layout = std::mem::take(&mut self.tabs[tab_index].layout);
        match owned_layout.remove_leaf(terminal_id) {
            Some(new_layout) => {
                let tab = &mut self.tabs[tab_index];
                tab.layout = new_layout;
                if tab.active_pane == terminal_id
                    && let Some(first) = tab.layout.first_leaf()
                {
                    tab.active_pane = first;
                }
            }
            None => {
                self.tabs.remove(tab_index);
                if self.active_tab > tab_index {
                    self.active_tab -= 1;
                }
                self.active_tab = self.active_tab.min(self.tabs.len().saturating_sub(1));

                if self.tabs.is_empty() {
                    cx.quit();
                    return;
                }
            }
        }

        self.focus_active_pane(window, cx);
        self.resize_active_layout(window, cx);
        cx.notify();
    }
}

#[derive(Clone, Copy, Debug)]
enum TitleAction {
    NewTab,
    SplitRight,
    SplitDown,
}

struct TabRowInput<'a> {
    index: usize,
    tab: &'a TerminalTab,
}

fn split_divider(axis: SplitAxis) -> AnyElement {
    match axis {
        SplitAxis::Row => div()
            .w(px(DIVIDER_THICKNESS))
            .h_full()
            .bg(rgb(Theme::RULE))
            .into_any_element(),
        SplitAxis::Column => div()
            .h(px(DIVIDER_THICKNESS))
            .w_full()
            .bg(rgb(Theme::RULE))
            .into_any_element(),
    }
}

fn collapse_home(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(rel) = path.strip_prefix(&home)
    {
        return if rel.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", rel.display())
        };
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::collapse_home;

    #[test]
    fn collapse_home_returns_tilde_for_home() {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if let Some(home) = home {
            assert_eq!(collapse_home(&home), "~");
            let nested = home.join("project");
            assert_eq!(collapse_home(&nested), "~/project");
        }
    }
}
