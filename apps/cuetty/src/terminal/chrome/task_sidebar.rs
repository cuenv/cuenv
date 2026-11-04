use super::{TASK_SIDEBAR_WIDTH, chrome_glass_color};
use crate::terminal::engine::TerminalView;
use crate::terminal::presentation::{TerminalTheme, gpui_rgb};
use gpui::{
    Context, FontWeight, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};

impl TerminalView {
    pub(in crate::terminal) fn render_task_sidebar(
        &self,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.task_sidebar_open {
            return None;
        }
        let state = self.active_state()?;
        if state.cuenv.tasks.is_empty() {
            return None;
        }
        let mut list = div().flex().flex_col().gap(px(4.0));
        for (index, task) in state.cuenv.tasks.iter().take(12).enumerate() {
            let task_to_run = task.clone();
            let detail = task
                .description
                .clone()
                .unwrap_or_else(|| task.kind.label().to_ascii_lowercase());
            list = list.child(
                div()
                    .id(("cuetty-task-sidebar-row", index))
                    .w_full()
                    .px(px(10.0))
                    .py(px(7.0))
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .rounded(px(7.0))
                    .hover(|this| this.bg(chrome_glass_color(theme.surface)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.launch_task(task_to_run.clone(), cx);
                    }))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(gpui::rgb(gpui_rgb(theme.text)))
                            .child(task.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                            .child(detail),
                    ),
            );
        }
        let hidden = state.cuenv.tasks.len().saturating_sub(12);
        let cwd = state
            .cuenv
            .cwd
            .as_deref()
            .and_then(std::path::Path::file_name)
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("cuenv");
        Some(
            div()
                .absolute()
                .top(px(12.0))
                .right(px(12.0))
                .bottom(px(12.0))
                .w(px(TASK_SIDEBAR_WIDTH))
                .p(px(12.0))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(gpui::rgb(gpui_rgb(theme.host_background)))
                .bg(chrome_glass_color(theme.title_surface))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(gpui::rgb(gpui_rgb(theme.text)))
                                        .child("Cuenv tasks"),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.0))
                                        .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                                        .child(cwd.to_string()),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                                .child("⌘K"),
                        ),
                )
                .child(div().flex_1().overflow_hidden().child(list))
                .when(hidden > 0, |this| {
                    this.child(
                        div()
                            .text_size(px(10.0))
                            .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                            .child(format!("{hidden} more · use ⌘K")),
                    )
                })
                .into_any_element(),
        )
    }
}
