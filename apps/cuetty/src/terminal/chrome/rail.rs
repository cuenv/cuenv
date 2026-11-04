use super::{TAB_RAIL_FOOTER_HEIGHT, chrome_glass_color};
use crate::integrations::cuenv::RailColor;
use crate::terminal::engine::TerminalView;
use crate::terminal::model::Rgb;
use crate::terminal::presentation::{TerminalTheme, gpui_rgb};
use crate::workspace::TabId;
use gpui::{
    Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};
use gpui_component::tooltip::Tooltip;

fn shell_notice_text(notice: Option<&str>) -> Option<String> {
    notice.map(|notice| format!("• {notice}"))
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
    pub(in crate::terminal) fn render_sidebar(
        &self,
        active_tab: Option<TabId>,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut tabs = div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .p(px(8.0))
            .overflow_hidden();
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
                .w_full()
                .min_h(px(40.0))
                .flex()
                .items_center()
                .justify_center()
                .px(px(2.0))
                .rounded(px(7.0))
                .hover(|this| this.bg(chrome_glass_color(theme.surface)))
                .cursor_pointer()
                .on_click(cx.listener(move |view, _event, window, cx| {
                    view.select_terminal_tab(tab_id, window, cx);
                }))
                .tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx));
            tab_button = tab_button.child(
                div()
                    .w(px(30.0))
                    .h(px(30.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.0))
                    .when(selected, |this| this.bg(gpui::rgb(gpui_rgb(theme.cursor))))
                    .text_center()
                    .text_size(px(12.0))
                    .font_weight(if selected {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(gpui::rgb(gpui_rgb(if selected {
                        theme.cursor_text
                    } else {
                        theme.title_text
                    })))
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
        let notice = shell_notice_text(self.registry.notice.as_deref().or(integration_notice));
        let has_tasks = self
            .active_state()
            .is_some_and(|state| !state.cuenv.tasks.is_empty());
        // A matching `package cuetty` tints the rail for the active tab;
        // terminal content itself is never framed.
        let rail_background = self
            .active_state()
            .and_then(|state| state.cuenv.presentation.rail)
            .map_or(theme.title_surface, |RailColor(red, green, blue)| {
                Rgb(red, green, blue)
            });
        let mut rail = div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .bg(chrome_glass_color(rail_background))
            .border_l_1()
            .border_color(gpui::rgb(gpui_rgb(theme.host_background)))
            .child(tabs);
        if let Some(notice) = notice {
            rail = rail.child(
                div()
                    .px(px(12.0))
                    .py(px(6.0))
                    .text_size(px(10.0))
                    .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                    .child(notice),
            );
        }
        let mut footer = div()
            .h(px(TAB_RAIL_FOOTER_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .gap(px(6.0))
            .px(px(6.0))
            .border_t_1()
            .border_color(gpui::rgb(gpui_rgb(theme.host_background)));
        let new_tab = div()
            .id("cuetty-new-tab")
            .flex_1()
            .h(px(30.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.0))
            .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
            .text_size(px(12.0))
            .hover(|this| this.bg(chrome_glass_color(theme.surface)))
            .cursor_pointer()
            .tooltip(|window, cx| Tooltip::new("New tab").build(window, cx))
            .on_click(cx.listener(|view, _event, window, cx| {
                view.new_tab(window, cx);
            }))
            .child("+");
        let tasks = div()
            .id("cuetty-tasks")
            .w(px(34.0))
            .h(px(30.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.0))
            .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
            .text_size(px(13.0))
            .when(self.task_sidebar_open, |this| {
                this.bg(chrome_glass_color(theme.surface))
            })
            .hover(|this| this.bg(chrome_glass_color(theme.surface)))
            .cursor_pointer()
            .tooltip(|window, cx| Tooltip::new("Cuenv tasks · ⌘K to search").build(window, cx))
            .on_click(cx.listener(|view, _event, _window, cx| {
                view.task_sidebar_open = !view.task_sidebar_open;
                cx.notify();
            }))
            .child("▤");
        footer = footer
            .flex_col()
            .child(new_tab)
            .when(has_tasks, |this| this.child(tasks));
        rail.child(footer).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_notice_keeps_live_action_failures_visible() {
        assert_eq!(
            shell_notice_text(Some(
                "workspace action unavailable: split panes are unavailable"
            )),
            Some("• workspace action unavailable: split panes are unavailable".into())
        );
        assert_eq!(shell_notice_text(None), None);
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
