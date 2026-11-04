use super::tokens::{ChromeColors, border, opaque, radius, size, space, text};
use super::tooltip::chrome_tooltip;
use crate::integrations::cuenv::RailColor;
use crate::terminal::engine::TerminalView;
use crate::terminal::model::Rgb;
use crate::terminal::presentation::TerminalTheme;
use crate::workspace::TabId;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, Rgba, StatefulInteractiveElement,
    Styled, div, prelude::FluentBuilder,
};

/// The active tab's marker: the project accent from a matching `package cuetty`,
/// or the theme accent. The colour only ever fills this small mark, so a project
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

fn sidebar_tab_label(label: &str) -> String {
    let path = label
        .split_whitespace()
        .rev()
        .find(|part| part.starts_with('/') || part.starts_with('~'))
        .unwrap_or(label);
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".into();
    }
    trimmed
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(trimmed)
        .to_string()
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
        let marker_color = active_tab_marker_color(
            self.active_state()
                .and_then(|state| state.cuenv.presentation.rail),
            &colors,
        );
        let mut tabs = div()
            .id("cuetty-rail-tabs")
            .track_scroll(&self.rail_scroll)
            .overflow_y_scroll()
            .flex_1()
            .flex()
            .flex_col()
            .gap(space::SM)
            .p(space::MD)
            .min_h_0();
        for (index, tab) in self
            .registry
            .workspace
            .workspace()
            .tabs()
            .iter()
            .enumerate()
        {
            let tab_id = tab.id;
            let selected = active_tab == Some(tab_id);
            let label = self
                .registry
                .sessions
                .get(&tab.focused)
                .map(|state| state.title.clone())
                .unwrap_or_else(|| tab.title.clone());
            let tooltip_label = self
                .registry
                .sessions
                .get(&tab.focused)
                .and_then(|state| state.cuenv.presentation.banner.clone())
                .unwrap_or_else(|| sidebar_tab_label(&label));
            let number = format!("{:02}", index + 1);
            let mut tab_button = div()
                .id(("cuetty-rail-tab", tab_id.get()))
                .relative()
                .w_full()
                .min_h(size::TAB_ROW_HEIGHT)
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .px(space::XXS)
                .rounded(radius::CONTROL)
                .hover(|this| this.bg(colors.state))
                .cursor_pointer()
                .on_click(cx.listener(move |view, _event, window, cx| {
                    view.select_terminal_tab(tab_id, window, cx);
                }))
                .tooltip(chrome_tooltip(tooltip_label, colors));
            tab_button = tab_button
                .when(selected, |this| {
                    this.child(
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
                .child(
                    div()
                        .w(size::TAB_MARK)
                        .h(size::TAB_MARK)
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(radius::CONTROL)
                        .when(selected, |this| this.bg(colors.state))
                        .text_center()
                        .text_size(text::LABEL)
                        .font_weight(if selected {
                            text::STRONG
                        } else {
                            text::REGULAR
                        })
                        .text_color(if selected {
                            colors.text
                        } else {
                            colors.text_secondary
                        })
                        .child(number),
                );
            tabs = tabs.child(tab_button);
        }
        let integration_notice = active_tab
            .and_then(|tab_id| {
                self.registry
                    .workspace
                    .workspace()
                    .tabs()
                    .iter()
                    .find(|tab| tab.id == tab_id)
            })
            .and_then(|tab| self.registry.sessions.get(&tab.focused))
            .and_then(|state| {
                state
                    .cuenv
                    .notice
                    .as_deref()
                    .or(state.cuenv.task_notice.as_deref())
            });
        // The rail is too narrow to read a message in, and an evaluation error can
        // run to hundreds of characters. A marker reports that there is one and
        // can never displace the tabs or controls.
        let notice_marker = self
            .registry
            .notice
            .as_deref()
            .or(integration_notice)
            .map(|notice| {
                div()
                    .id("cuetty-notice")
                    .w(size::CONTROL_WIDTH)
                    .h(size::CONTROL_HEIGHT)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(radius::CONTROL)
                    .hover(|this| this.bg(colors.state))
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
        let rail = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .bg(colors.material)
            .border_l(border::HAIRLINE)
            .border_color(colors.divider)
            .child(tabs);
        let mut footer = div()
            .w_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .gap(space::XS)
            .py(space::SM)
            .border_t(border::HAIRLINE)
            .border_color(colors.divider);
        let new_tab = div()
            .id("cuetty-new-tab")
            .w(size::CONTROL_WIDTH)
            .h(size::CONTROL_HEIGHT)
            .flex()
            .items_center()
            .justify_center()
            .rounded(radius::CONTROL)
            .text_color(colors.text_secondary)
            .text_size(text::LABEL)
            .hover(|this| this.bg(colors.state))
            .cursor_pointer()
            .tooltip(chrome_tooltip("New tab", colors))
            .on_click(cx.listener(|view, _event, window, cx| {
                view.new_tab(window, cx);
            }))
            .child("+");
        let palette_chord = self.palette_chord_label();
        let tasks = div()
            .id("cuetty-tasks")
            .w(size::CONTROL_WIDTH)
            .h(size::CONTROL_HEIGHT)
            .flex()
            .items_center()
            .justify_center()
            .rounded(radius::CONTROL)
            .text_color(colors.text_secondary)
            .text_size(text::BODY)
            .when(self.task_sidebar_open, |this| this.bg(colors.state))
            .hover(|this| this.bg(colors.state))
            .cursor_pointer()
            .tooltip(chrome_tooltip(
                format!("Cuenv tasks · {palette_chord} to search"),
                colors,
            ))
            .on_click(cx.listener(|view, _event, _window, cx| {
                view.task_sidebar_open = !view.task_sidebar_open;
                cx.notify();
            }))
            .child("▤");
        footer = footer
            .children(notice_marker)
            .child(new_tab)
            .when(has_tasks, |this| this.child(tasks));
        rail.child(footer).into_any_element()
    }
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

    #[test]
    fn sidebar_labels_use_a_stable_single_line_name() {
        assert_eq!(
            sidebar_tab_label("~/Code/src/github.com/cuenv/cuenv"),
            "cuenv"
        );
        assert_eq!(sidebar_tab_label("/"), "/");
        assert_eq!(sidebar_tab_label("build"), "build");
        assert_eq!(sidebar_tab_label("sleep 5 ~/Code/cuenv"), "cuenv");
    }
}
