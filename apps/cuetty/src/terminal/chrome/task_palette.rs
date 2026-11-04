use super::tokens::{ChromeColors, border, elevation, radius, size, space, text};
use crate::terminal::engine::{TerminalView, filtered_tasks};
use crate::terminal::presentation::TerminalTheme;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, prelude::FluentBuilder,
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
        let colors = ChromeColors::from_theme(theme);
        let caret = || {
            div()
                .w(size::CARET_WIDTH)
                .h(size::CARET_HEIGHT)
                .flex_shrink_0()
                .bg(colors.accent)
        };
        let tasks = filtered_tasks(&state.cuenv.tasks, &self.task_query);
        let mut results = div().flex().flex_col().gap(space::XS);
        for (index, task) in tasks.iter().take(9).enumerate() {
            let selected = index == self.task_selection;
            let task_to_run = (*task).clone();
            results = results.child(
                div()
                    .id(("cuetty-task-palette-row", index))
                    .w_full()
                    .px(space::LG)
                    .py(space::MD)
                    .flex()
                    .items_center()
                    .gap(space::MD)
                    .rounded(radius::CONTROL)
                    .when(selected, |this| this.bg(colors.panel_selected))
                    .when(!selected, |this| {
                        this.hover(|this| this.bg(colors.panel_hover))
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.launch_task(task_to_run.clone(), cx);
                    }))
                    .child(
                        div()
                            .w(size::PALETTE_KIND_WIDTH)
                            .text_size(text::CAPTION)
                            .text_color(if selected {
                                colors.text_secondary
                            } else {
                                colors.text_muted
                            })
                            .child(if task.requires_parameters {
                                "ARGS"
                            } else {
                                task.kind.label()
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(text::LABEL)
                            .text_color(colors.text)
                            .child(task.name.clone()),
                    ),
            );
        }
        if tasks.is_empty() {
            results = results.child(
                div()
                    .px(space::LG)
                    .py(space::XL)
                    .text_size(text::LABEL)
                    .text_color(colors.text_muted)
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
                .pt(size::PALETTE_TOP_OFFSET)
                .child(
                    div()
                        .w(size::PALETTE_WIDTH)
                        .max_h(size::PALETTE_MAX_HEIGHT)
                        .p(space::LG)
                        .flex()
                        .flex_col()
                        .gap(space::MD)
                        .rounded(radius::PANEL)
                        .border(border::HAIRLINE)
                        .border_color(colors.divider)
                        .bg(colors.panel)
                        .shadow(elevation::overlay())
                        .child(
                            div()
                                .w_full()
                                .h(size::PALETTE_FIELD_HEIGHT)
                                .px(space::LG)
                                .flex()
                                .items_center()
                                .rounded(radius::CONTROL)
                                .gap(space::XXS)
                                .bg(colors.well)
                                .text_size(text::BODY)
                                .text_color(if self.task_query.is_empty() {
                                    colors.text_muted
                                } else {
                                    colors.text
                                })
                                // Filter text is typed at the end of the query,
                                // so an empty query keeps the caret before the
                                // placeholder.
                                .when(self.task_query.is_empty(), |this| this.child(caret()))
                                .child(query)
                                .when(!self.task_query.is_empty(), |this| this.child(caret())),
                        )
                        .child(results)
                        .child(
                            div()
                                .text_size(text::CAPTION)
                                .text_color(colors.text_muted)
                                .child("↑↓ select · return run · esc close"),
                        ),
                )
                .into_any_element(),
        )
    }
}
