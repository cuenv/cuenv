use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use flume::Receiver;
use gpui::{
    AnyElement, AppContext, Context as GpuiContext, Entity, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Render,
    SharedString, Styled, Window, div, px, rgb,
};
use gpui_ghostty_terminal::view::{TerminalInput, TerminalView};
use gpui_ghostty_terminal::{
    TerminalConfig, TerminalSession, default_terminal_font, default_terminal_font_features,
};

use crate::pty::{GridMetrics, PtyGridSize, TerminalProcess, TerminalProcessOptions};
use crate::terminal_responses::TerminalResponseScanner;
use crate::theme::Theme;
use crate::{CloseTab, FocusNextPane, NewTab, SplitDown, SplitRight};

const TITLEBAR_HEIGHT: f32 = 40.0;
const SIDEBAR_WIDTH: f32 = 220.0;
const DIVIDER_THICKNESS: f32 = 1.0;
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
            .id(SharedString::from(format!("cuetty-pane-{}", terminal_id.0)))
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
        let Some((cell_width, cell_height)) = cell_metrics(window) else {
            return;
        };

        let region = terminal_region(window);
        let mut input = ResizeLayoutInput {
            panes: &mut self.panes,
            cell_width,
            cell_height,
            cx,
        };
        resize_layout_node(&tab.layout, region, &mut input);
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
        let id = TerminalId(self.next_terminal_id);
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TerminalId(u64);

struct TerminalTab {
    title: String,
    layout: PaneNode,
    active_pane: TerminalId,
}

struct TerminalPane {
    id: TerminalId,
    view: Entity<TerminalView>,
    master: Arc<dyn portable_pty::MasterPty + Send>,
    focus_handle: FocusHandle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PaneNode {
    Leaf(TerminalId),
    Split {
        axis: SplitAxis,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
}

impl PaneNode {
    fn split_leaf(&mut self, target: TerminalId, axis: SplitAxis, new_leaf: TerminalId) -> bool {
        match self {
            PaneNode::Leaf(id) if *id == target => {
                *self = PaneNode::Split {
                    axis,
                    first: Box::new(PaneNode::Leaf(target)),
                    second: Box::new(PaneNode::Leaf(new_leaf)),
                };
                true
            }
            PaneNode::Leaf(_) => false,
            PaneNode::Split { first, second, .. } => {
                first.split_leaf(target, axis, new_leaf)
                    || second.split_leaf(target, axis, new_leaf)
            }
        }
    }

    fn contains(&self, terminal_id: TerminalId) -> bool {
        match self {
            PaneNode::Leaf(id) => *id == terminal_id,
            PaneNode::Split { first, second, .. } => {
                first.contains(terminal_id) || second.contains(terminal_id)
            }
        }
    }

    fn active_or_first_leaf(&self, active: TerminalId) -> Option<TerminalId> {
        if self.contains(active) {
            Some(active)
        } else {
            self.first_leaf()
        }
    }

    fn first_leaf(&self) -> Option<TerminalId> {
        match self {
            PaneNode::Leaf(id) => Some(*id),
            PaneNode::Split { first, .. } => first.first_leaf(),
        }
    }

    fn leaf_ids(&self, ids: &mut Vec<TerminalId>) {
        match self {
            PaneNode::Leaf(id) => ids.push(*id),
            PaneNode::Split { first, second, .. } => {
                first.leaf_ids(ids);
                second.leaf_ids(ids);
            }
        }
    }

    fn leaf_count(&self) -> usize {
        match self {
            PaneNode::Leaf(_) => 1,
            PaneNode::Split { first, second, .. } => first.leaf_count() + second.leaf_count(),
        }
    }

    fn remove_leaf(self, target: TerminalId) -> Option<PaneNode> {
        match self {
            PaneNode::Leaf(id) if id == target => None,
            PaneNode::Leaf(_) => Some(self),
            PaneNode::Split {
                axis,
                first,
                second,
            } => match (first.remove_leaf(target), second.remove_leaf(target)) {
                (None, None) => None,
                (Some(node), None) | (None, Some(node)) => Some(node),
                (Some(f), Some(s)) => Some(PaneNode::Split {
                    axis,
                    first: Box::new(f),
                    second: Box::new(s),
                }),
            },
        }
    }
}

impl Default for PaneNode {
    fn default() -> Self {
        // TerminalId(0) is never allocated (next_terminal_id starts at 1) so this is a safe
        // placeholder for std::mem::take during layout surgery.
        PaneNode::Leaf(TerminalId(0))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SplitAxis {
    Row,
    Column,
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

struct TerminalLaunchOptions {
    terminal_id: TerminalId,
    process_options: TerminalProcessOptions,
}

fn launch_terminal(
    window: &mut Window,
    options: TerminalLaunchOptions,
    cx: &mut GpuiContext<RootView>,
) -> Result<TerminalPane> {
    let config = TerminalConfig::default();
    let process = TerminalProcess::spawn(TerminalProcessOptions {
        grid_size: PtyGridSize::new(config.cols, config.rows),
        ..options.process_options
    })?;

    let session = TerminalSession::new(config).context("failed to initialize Ghostty VT")?;
    let stdin_tx = process.stdin_tx.clone();
    let input = TerminalInput::new(move |bytes| {
        if stdin_tx.send(bytes.to_vec()).is_err() {
            tracing::debug!("terminal input channel is closed");
        }
    });
    let focus_handle = cx.focus_handle();
    let view_focus = focus_handle.clone();
    let view = cx.new(|_| TerminalView::new_with_input(session, view_focus, input));
    let master = process.master.clone();

    start_output_pump(
        window,
        OutputPumpInput {
            view: view.clone(),
            stdin_tx: process.stdin_tx.clone(),
            stdout_rx: process.stdout_rx.clone(),
        },
        cx,
    );

    spawn_pane_death_observer(window, options.terminal_id, process.exited_rx.clone(), cx);

    Ok(TerminalPane {
        id: options.terminal_id,
        view,
        master,
        focus_handle,
    })
}

fn spawn_pane_death_observer(
    window: &mut Window,
    terminal_id: TerminalId,
    exited_rx: Receiver<()>,
    cx: &mut GpuiContext<RootView>,
) {
    let root_entity = cx.entity();
    window
        .spawn(cx, async move |cx| {
            let _ = exited_rx.recv_async().await;
            let _ = cx.update(|window, cx| {
                root_entity.update(cx, |root, cx| {
                    root.handle_pane_exit(terminal_id, window, cx);
                });
            });
        })
        .detach();
}

fn install_resize_observer(window: &mut Window, cx: &mut GpuiContext<RootView>) {
    let subscription = cx.observe_window_bounds(window, move |root, window, cx| {
        root.resize_active_layout(window, cx);
    });
    subscription.detach();
}

struct OutputPumpInput {
    view: Entity<TerminalView>,
    stdin_tx: flume::Sender<Vec<u8>>,
    stdout_rx: flume::Receiver<Vec<u8>>,
}

fn start_output_pump(window: &mut Window, input: OutputPumpInput, cx: &mut GpuiContext<RootView>) {
    let view_for_task = input.view;
    let stdin_tx = input.stdin_tx;
    let stdout_rx = input.stdout_rx;
    window
        .spawn(cx, async move |cx| {
            let mut response_scanner = TerminalResponseScanner::default();
            let mut respond = |response: &[u8]| {
                if stdin_tx.send(response.to_vec()).is_err() {
                    tracing::debug!("terminal response channel is closed");
                }
            };

            while let Ok(first) = stdout_rx.recv_async().await {
                let mut batch = first;
                response_scanner.scan(&batch, &mut respond);

                while let Ok(chunk) = stdout_rx.try_recv() {
                    response_scanner.scan(&chunk, &mut respond);
                    batch.extend_from_slice(&chunk);
                }

                let _ = cx.update(|_, cx| {
                    view_for_task.update(cx, |terminal, cx| {
                        terminal.queue_output_bytes(&batch, cx);
                    });
                });
            }
        })
        .detach();
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PixelRegion {
    width: f32,
    height: f32,
}

struct ResizeLayoutInput<'a, 'b> {
    panes: &'a mut [TerminalPane],
    cell_width: f32,
    cell_height: f32,
    cx: &'a mut GpuiContext<'b, RootView>,
}

fn resize_layout_node(node: &PaneNode, region: PixelRegion, input: &mut ResizeLayoutInput<'_, '_>) {
    match node {
        PaneNode::Leaf(id) => resize_pane(*id, region, input),
        PaneNode::Split {
            axis,
            first,
            second,
        } => {
            let (first_region, second_region) = split_region(region, *axis);
            resize_layout_node(first, first_region, input);
            resize_layout_node(second, second_region, input);
        }
    }
}

fn resize_pane(
    terminal_id: TerminalId,
    region: PixelRegion,
    input: &mut ResizeLayoutInput<'_, '_>,
) {
    let Some(pane) = input.panes.iter_mut().find(|pane| pane.id == terminal_id) else {
        return;
    };

    let grid_size = PtyGridSize::from_metrics(GridMetrics {
        pixel_width: region.width,
        pixel_height: region.height,
        cell_width: input.cell_width,
        cell_height: input.cell_height,
    });

    if let Err(error) = pane.master.resize(grid_size.to_pty_size()) {
        tracing::debug!(%error, "failed to resize PTY");
    }
    pane.view.update(input.cx, |terminal, cx| {
        terminal.resize_terminal(grid_size.cols, grid_size.rows, cx);
    });
}

fn split_region(region: PixelRegion, axis: SplitAxis) -> (PixelRegion, PixelRegion) {
    match axis {
        SplitAxis::Row => {
            let width = ((region.width - DIVIDER_THICKNESS) / 2.0).max(1.0);
            (
                PixelRegion {
                    width,
                    height: region.height,
                },
                PixelRegion {
                    width,
                    height: region.height,
                },
            )
        }
        SplitAxis::Column => {
            let height = ((region.height - DIVIDER_THICKNESS) / 2.0).max(1.0);
            (
                PixelRegion {
                    width: region.width,
                    height,
                },
                PixelRegion {
                    width: region.width,
                    height,
                },
            )
        }
    }
}

fn terminal_region(window: &mut Window) -> PixelRegion {
    let size = window.viewport_size();
    PixelRegion {
        width: (f32::from(size.width) - SIDEBAR_WIDTH).max(1.0),
        height: (f32::from(size.height) - TITLEBAR_HEIGHT).max(1.0),
    }
}

fn cell_metrics(window: &mut Window) -> Option<(f32, f32)> {
    let mut style = window.text_style();
    let font = default_terminal_font();
    style.font_family = font.family.clone();
    style.font_features = default_terminal_font_features();
    style.font_fallbacks = font.fallbacks.clone();

    let rem_size = window.rem_size();
    let font_size = style.font_size.to_pixels(rem_size);
    let line_height = style.line_height.to_pixels(style.font_size, rem_size);

    let run = style.to_run(1);
    let lines = window
        .text_system()
        .shape_text(SharedString::from("M"), font_size, &[run], None, Some(1))
        .ok()?;
    let line = lines.first()?;
    let cell_width = f32::from(line.width()).max(1.0);
    let cell_height = f32::from(line_height).max(1.0);

    Some((cell_width, cell_height))
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
    use super::*;

    #[test]
    fn split_leaf_replaces_target_with_ordered_split() {
        let first = TerminalId(1);
        let second = TerminalId(2);
        let mut layout = PaneNode::Leaf(first);

        assert!(layout.split_leaf(first, SplitAxis::Row, second));

        assert_eq!(
            layout,
            PaneNode::Split {
                axis: SplitAxis::Row,
                first: Box::new(PaneNode::Leaf(first)),
                second: Box::new(PaneNode::Leaf(second)),
            }
        );
    }

    #[test]
    fn remove_leaf_collapses_split_to_sibling() {
        let layout = PaneNode::Split {
            axis: SplitAxis::Row,
            first: Box::new(PaneNode::Leaf(TerminalId(1))),
            second: Box::new(PaneNode::Leaf(TerminalId(2))),
        };

        assert_eq!(
            layout.remove_leaf(TerminalId(1)),
            Some(PaneNode::Leaf(TerminalId(2)))
        );
    }

    #[test]
    fn remove_leaf_returns_none_when_tree_empties() {
        let layout = PaneNode::Leaf(TerminalId(1));
        assert_eq!(layout.remove_leaf(TerminalId(1)), None);
    }

    #[test]
    fn remove_leaf_collapses_nested_split() {
        let layout = PaneNode::Split {
            axis: SplitAxis::Row,
            first: Box::new(PaneNode::Leaf(TerminalId(1))),
            second: Box::new(PaneNode::Split {
                axis: SplitAxis::Column,
                first: Box::new(PaneNode::Leaf(TerminalId(2))),
                second: Box::new(PaneNode::Leaf(TerminalId(3))),
            }),
        };

        assert_eq!(
            layout.remove_leaf(TerminalId(2)),
            Some(PaneNode::Split {
                axis: SplitAxis::Row,
                first: Box::new(PaneNode::Leaf(TerminalId(1))),
                second: Box::new(PaneNode::Leaf(TerminalId(3))),
            })
        );
    }

    #[test]
    fn leaf_ids_preserve_focus_order() {
        let layout = PaneNode::Split {
            axis: SplitAxis::Row,
            first: Box::new(PaneNode::Leaf(TerminalId(1))),
            second: Box::new(PaneNode::Split {
                axis: SplitAxis::Column,
                first: Box::new(PaneNode::Leaf(TerminalId(2))),
                second: Box::new(PaneNode::Leaf(TerminalId(3))),
            }),
        };
        let mut ids = Vec::new();

        layout.leaf_ids(&mut ids);

        assert_eq!(ids, vec![TerminalId(1), TerminalId(2), TerminalId(3)]);
    }

    #[test]
    fn split_region_accounts_for_divider() {
        let (left, right) = split_region(
            PixelRegion {
                width: 101.0,
                height: 80.0,
            },
            SplitAxis::Row,
        );

        assert_eq!(
            (left, right),
            (
                PixelRegion {
                    width: 50.0,
                    height: 80.0,
                },
                PixelRegion {
                    width: 50.0,
                    height: 80.0,
                },
            )
        );
    }

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
