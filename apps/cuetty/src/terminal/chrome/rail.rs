use super::icons::Icon;
use super::tokens::{ChromeColors, border, opaque, radius, size, space, text};
use super::tooltip::chrome_tooltip;
use crate::integrations::cuenv::{RailColor, SidebarMode};
use crate::terminal::engine::{SIDEBAR_TOGGLE_KEY, TerminalView, app_chord_label, tab_number_hint};
use crate::terminal::model::Rgb;
use crate::terminal::presentation::TerminalTheme;
use crate::terminal::tab_label::{TabMarkSources, tab_mark, tab_tooltip};
use crate::terminal::workspace_shell::TabNumber;
use crate::workspace::TabId;
use gpui::{
    Context, Div, InteractiveElement, IntoElement, ParentElement, Rgba, SharedString, Stateful,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder,
};

/// The active tab's marker: the project's `rail` from a matching `package cuetty`,
/// or the window's accent. The colour only ever fills this small mark, so a project
/// can never choose a fill the rail's text has to survive.
fn active_tab_marker_color(project_accent: Option<RailColor>, colors: &ChromeColors) -> Rgba {
    project_accent.map_or(colors.accent, |RailColor(red, green, blue)| {
        opaque(Rgb(red, green, blue))
    })
}

/// Where the tab list has to scroll to: the active tab's position, once for each
/// time it changes, so wheel scrolling through the list is left alone in between.
fn rail_scroll_target(
    active_tab: Option<TabId>,
    revealed_tab: Option<TabId>,
    mut tabs: impl Iterator<Item = TabId>,
) -> Option<usize> {
    active_tab
        .filter(|_| active_tab != revealed_tab)
        .and_then(|active| tabs.position(|tab| tab == active))
}

impl TerminalView {
    /// Keeps the active tab's marker on screen when the list is taller than the
    /// rail, whichever way the tab became active.
    pub(in crate::terminal) fn reveal_active_rail_tab(&mut self, active_tab: Option<TabId>) {
        let tabs = self.registry.workspace.workspace().tabs();
        if let Some(index) = rail_scroll_target(
            active_tab,
            self.rail_revealed_tab,
            tabs.iter().map(|tab| tab.id),
        ) {
            self.rail_scroll.scroll_to_item(index);
        }
        self.rail_revealed_tab = active_tab;
    }

    pub(in crate::terminal) fn render_sidebar(
        &self,
        active_tab: Option<TabId>,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = ChromeColors::from_theme(theme);
        let mode = self.sidebar_mode();
        let marker_color = active_tab_marker_color(
            self.active_state()
                .and_then(|state| state.cuenv.presentation.rail),
            &colors,
        );
        let tab_list = self.registry.workspace.workspace().tabs();
        let tabs = tab_list
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let label = self.registry.tab_label(tab);
                let cuenv = self
                    .registry
                    .sessions
                    .get(&tab.focused)
                    .map(|state| &state.cuenv);
                let banner = cuenv.and_then(|cuenv| cuenv.presentation.banner.as_deref());
                let tooltip = tab_tooltip(&label, banner);
                let mark = tab_mark(TabMarkSources {
                    custom_name: tab.custom_name.as_deref(),
                    banner,
                    directory: cuenv.and_then(|cuenv| cuenv.cwd.as_deref()),
                    label: &label,
                });
                render_tab_row(
                    TabRow {
                        id: tab.id,
                        label,
                        mark,
                        tooltip,
                        number: self
                            .tab_hints_visible
                            .then(|| TabNumber::of_position(index, tab_list.len()))
                            .flatten(),
                        selected: active_tab == Some(tab.id),
                        mode,
                        marker_color,
                        colors,
                        mono: self.mono_font_family(),
                    },
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let tabs = div()
            .id("cuetty-rail-tabs")
            .track_scroll(&self.rail_scroll)
            .overflow_y_scroll()
            .flex_1()
            .flex()
            .flex_col()
            .gap(space::XXS)
            .p(space::MD)
            .min_h_0()
            .children(tabs);
        let integration_notice = active_tab
            .and_then(|tab_id| tab_list.iter().find(|tab| tab.id == tab_id))
            .and_then(|tab| self.registry.sessions.get(&tab.focused))
            .and_then(|state| state.marker_notice());
        // A message does not fit here, and an evaluation error can run to hundreds
        // of characters. A marker reports that there is one and can never displace
        // the tabs or controls.
        let notice_marker = self
            .registry
            .notice
            .as_deref()
            .or(integration_notice)
            .map(|notice| {
                control(&colors, "cuetty-notice")
                    .tooltip(chrome_tooltip(notice.to_string(), colors))
                    .child(
                        div()
                            .size(size::NOTICE_MARK)
                            .rounded(radius::CONTROL)
                            .bg(colors.accent),
                    )
            });
        let has_tasks = self
            .active_state()
            .is_some_and(|state| !state.cuenv.tasks.is_empty());
        let new_tab = control(&colors, "cuetty-new-tab")
            .cursor_pointer()
            .tooltip(chrome_tooltip(
                format!("New tab · {}", app_chord_label("t")),
                colors,
            ))
            .on_click(cx.listener(|view, _event, window, cx| {
                view.new_tab(window, cx);
            }))
            .child(Icon::Plus.element(colors.text_secondary));
        let palette_chord = self.palette_chord_label();
        let tasks = control(&colors, "cuetty-tasks")
            .when(self.task_sidebar_open, |this| this.bg(colors.state))
            .cursor_pointer()
            .tooltip(chrome_tooltip(
                format!("Cuenv tasks · {palette_chord} to search"),
                colors,
            ))
            .on_click(cx.listener(|view, _event, _window, cx| {
                view.task_sidebar_open = !view.task_sidebar_open;
                cx.notify();
            }))
            .child(Icon::Tasks.element(colors.text_secondary));
        let (toggle_label, toggle_icon) = match mode {
            SidebarMode::Expanded => ("Collapse sidebar", Icon::SidebarCollapse),
            SidebarMode::Collapsed => ("Expand sidebar", Icon::SidebarExpand),
        };
        let toggle = control(&colors, "cuetty-sidebar-toggle")
            .cursor_pointer()
            .tooltip(chrome_tooltip(
                format!("{toggle_label} · {}", app_chord_label(SIDEBAR_TOGGLE_KEY)),
                colors,
            ))
            .on_click(cx.listener(|view, _event, _window, cx| view.toggle_sidebar(cx)))
            .child(toggle_icon.element(colors.text_secondary));
        let footer = div()
            .w_full()
            .flex_shrink_0()
            .flex()
            .gap(space::XS)
            .p(space::SM)
            .map(|this| match mode {
                SidebarMode::Expanded => this.flex_row().items_center(),
                SidebarMode::Collapsed => this.flex_col().items_center(),
            })
            .children(notice_marker)
            .child(new_tab)
            .when(has_tasks, |this| this.child(tasks))
            // Expanded, the toggle sits at the far end of the row, where the
            // collapsed sidebar's edge will be.
            .when(mode == SidebarMode::Expanded, |this| {
                this.child(div().flex_1())
            })
            .child(toggle);
        div()
            .relative()
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(radius::PANEL)
            .bg(colors.sidebar)
            .border(border::HAIRLINE)
            .border_color(colors.divider)
            .child(tabs)
            .child(footer)
            .into_any_element()
    }
}

/// The square-ish hit target every sidebar control shares.
fn control(colors: &ChromeColors, id: &'static str) -> Stateful<Div> {
    let hover = colors.state;
    div()
        .id(id)
        .w(size::CONTROL_WIDTH)
        .h(size::CONTROL_HEIGHT)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius::CONTROL)
        .hover(move |this| this.bg(hover))
}

/// What a row in the tab list shows.
struct TabRow {
    id: TabId,
    label: String,
    /// What stands in for the label when the sidebar is collapsed.
    mark: String,
    tooltip: String,
    number: Option<TabNumber>,
    selected: bool,
    mode: SidebarMode,
    marker_color: Rgba,
    colors: ChromeColors,
    mono: SharedString,
}

fn render_tab_row(row: TabRow, cx: &mut Context<TerminalView>) -> Stateful<Div> {
    let TabRow {
        id,
        label,
        mark,
        tooltip,
        number,
        selected,
        mode,
        marker_color,
        colors,
        mono,
    } = row;
    let hover = colors.state;
    let text_color = if selected {
        colors.text
    } else {
        colors.text_secondary
    };
    div()
        .id(("cuetty-rail-tab", id.get()))
        .relative()
        .w_full()
        .h(size::TAB_ROW_HEIGHT)
        .flex_shrink_0()
        .flex()
        .items_center()
        .rounded(radius::CONTROL)
        .hover(move |this| this.bg(hover))
        .cursor_pointer()
        .on_click(cx.listener(move |view, _event, window, cx| {
            view.select_terminal_tab(id, window, cx);
        }))
        .tooltip(chrome_tooltip(tooltip, colors))
        .text_size(text::BODY)
        .text_color(text_color)
        .font_weight(if selected {
            text::MEDIUM
        } else {
            text::REGULAR
        })
        .when(selected, |this| {
            this.bg(colors.state).child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .w(size::TAB_MARKER_WIDTH)
                            .h(size::TAB_MARKER_HEIGHT)
                            .rounded(radius::CONTROL)
                            .bg(marker_color),
                    ),
            )
        })
        .map(|this| match mode {
            SidebarMode::Expanded => this
                .gap(space::MD)
                .px(space::LG)
                .child(div().flex_1().min_w_0().truncate().child(label))
                .children(number.map(|number| {
                    div()
                        .flex_none()
                        .font_family(mono.clone())
                        .text_size(text::CAPTION)
                        .text_color(colors.text_muted)
                        .font_weight(text::REGULAR)
                        .child(tab_number_hint(number))
                })),
            // Only a character fits, so the hint stands in for the mark.
            SidebarMode::Collapsed => this.justify_center().child(match number {
                Some(number) => div()
                    .font_family(mono.clone())
                    .text_size(text::LABEL)
                    .text_color(colors.text_muted)
                    .font_weight(text::REGULAR)
                    .child(number.digit().to_string()),
                None => div().child(mark),
            }),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::config::TerminalConfig;

    #[test]
    fn active_tab_marker_prefers_the_project_accent_over_the_theme_accent() {
        let theme = TerminalTheme::from_config(&TerminalConfig::default());
        let colors = ChromeColors::from_theme(&theme);
        assert_eq!(active_tab_marker_color(None, &colors), colors.accent);
        assert_eq!(
            active_tab_marker_color(Some(RailColor(255, 0, 0)), &colors),
            gpui::rgb(0xff0000)
        );
    }

    #[test]
    fn the_tab_list_scrolls_once_for_each_change_of_active_tab() {
        let (first, second, closed) = (TabId::new(1), TabId::new(2), TabId::new(3));
        let tabs = || [first, second].into_iter();
        assert_eq!(rail_scroll_target(Some(second), None, tabs()), Some(1));
        assert_eq!(
            rail_scroll_target(Some(first), Some(second), tabs()),
            Some(0)
        );
        assert_eq!(rail_scroll_target(Some(second), Some(second), tabs()), None);
        assert_eq!(rail_scroll_target(None, Some(second), tabs()), None);
        assert_eq!(rail_scroll_target(Some(closed), Some(first), tabs()), None);
    }
}
