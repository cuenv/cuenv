use super::tokens::{ChromeColors, border, elevation, radius, size, space, text};
use crate::terminal::engine::TerminalView;
use crate::terminal::presentation::TerminalTheme;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, prelude::FluentBuilder,
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
        let colors = ChromeColors::from_theme(theme);
        let mut list = div().flex().flex_col().gap(space::XS);
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
                    .px(space::LG)
                    .py(space::SM)
                    .flex()
                    .flex_col()
                    .gap(space::XXS)
                    .rounded(radius::CONTROL)
                    .hover(|this| this.bg(colors.panel_hover))
                    .cursor_pointer()
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.launch_task(task_to_run.clone(), cx);
                    }))
                    .child(
                        div()
                            .text_size(text::LABEL)
                            .text_color(colors.text)
                            .child(task.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(text::CAPTION)
                            .text_color(colors.text_muted)
                            .child(detail),
                    ),
            );
        }
        let hidden = state.cuenv.tasks.len().saturating_sub(12);
        let palette_chord = self.palette_chord_label();
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
                .top(space::LG)
                .right(space::LG)
                .bottom(space::LG)
                .w(size::TASK_SIDEBAR_WIDTH)
                .p(space::LG)
                .flex()
                .flex_col()
                .gap(space::MD)
                .rounded(radius::PANEL)
                .border(border::HAIRLINE)
                .border_color(colors.panel_outline)
                .bg(colors.panel)
                .shadow(elevation::overlay())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(space::XXS)
                                .child(
                                    div()
                                        .text_size(text::TITLE)
                                        .font_weight(text::STRONG)
                                        .text_color(colors.text)
                                        .child("Cuenv tasks"),
                                )
                                .child(
                                    div()
                                        .text_size(text::CAPTION)
                                        .text_color(colors.text_muted)
                                        .child(cwd.to_string()),
                                ),
                        )
                        .child(
                            div()
                                .text_size(text::CAPTION)
                                .text_color(colors.text_muted)
                                .child(palette_chord.clone()),
                        ),
                )
                .child(div().flex_1().overflow_hidden().child(list))
                .when(hidden > 0, |this| {
                    this.child(
                        div()
                            .text_size(text::CAPTION)
                            .text_color(colors.text_muted)
                            .child(format!("{hidden} more · use {palette_chord}")),
                    )
                })
                .into_any_element(),
        )
    }
}
