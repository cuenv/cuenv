use super::chrome_glass_color;
use crate::terminal::engine::{TerminalView, filtered_tasks};
use crate::terminal::presentation::{TerminalTheme, gpui_rgb};
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, prelude::FluentBuilder, px,
};

impl TerminalView {
    pub(in crate::terminal) fn render_task_palette(
        &self,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.task_palette_open {
            return None;
        }
        let state = self.active_state()?;
        let tasks = filtered_tasks(&state.cuenv.tasks, &self.task_query);
        let mut results = div().flex().flex_col().gap(px(3.0));
        for (index, task) in tasks.iter().take(9).enumerate() {
            let selected = index == self.task_selection;
            let task_to_run = (*task).clone();
            results = results.child(
                div()
                    .id(("cuetty-task-palette-row", index))
                    .w_full()
                    .px(px(10.0))
                    .py(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .rounded(px(7.0))
                    .when(selected, |this| this.bg(chrome_glass_color(theme.surface)))
                    .hover(|this| this.bg(chrome_glass_color(theme.surface)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.launch_task(task_to_run.clone(), cx);
                    }))
                    .child(
                        div()
                            .w(px(64.0))
                            .text_size(px(9.0))
                            .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                            .child(if task.requires_parameters {
                                "ARGS"
                            } else {
                                task.kind.label()
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.0))
                            .text_color(gpui::rgb(gpui_rgb(theme.text)))
                            .child(task.name.clone()),
                    ),
            );
        }
        if tasks.is_empty() {
            results = results.child(
                div()
                    .px(px(10.0))
                    .py(px(18.0))
                    .text_size(px(12.0))
                    .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                    .child("No matching tasks"),
            );
        }
        let query = if self.task_query.is_empty() {
            "Type to filter tasks…".into()
        } else {
            self.task_query.clone()
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(54.0))
                .child(
                    div()
                        .w(px(520.0))
                        .max_h(px(430.0))
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
                                .w_full()
                                .h(px(38.0))
                                .px(px(12.0))
                                .flex()
                                .items_center()
                                .rounded(px(8.0))
                                .bg(chrome_glass_color(theme.surface))
                                .text_size(px(13.0))
                                .text_color(gpui::rgb(gpui_rgb(if self.task_query.is_empty() {
                                    theme.title_text
                                } else {
                                    theme.text
                                })))
                                .child(query),
                        )
                        .child(results)
                        .child(
                            div()
                                .text_size(px(9.0))
                                .text_color(gpui::rgb(gpui_rgb(theme.title_text)))
                                .child("↑↓ select · return run · esc close"),
                        ),
                )
                .into_any_element(),
        )
    }
}
