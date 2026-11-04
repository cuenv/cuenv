use super::tokens::{ChromeColors, border, elevation, radius, size, space, text};
use crate::terminal::engine::TerminalView;
use crate::terminal::presentation::TerminalTheme;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div,
};

fn task_count_label(count: usize) -> String {
    match count {
        1 => "1 task".into(),
        count => format!("{count} tasks"),
    }
}

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
        for (index, task) in state.cuenv.tasks.iter().enumerate() {
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
        let palette_chord = self.palette_chord_label();
        let mono = self.mono_font_family();
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
                // The panel covers the terminal, so pointer input stops here
                // instead of selecting or scrolling text beneath it.
                .occlude()
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
                .line_height(text::LINE_HEIGHT)
                .child(
                    div()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(space::MD)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
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
                                        .font_family(mono.clone())
                                        .text_size(text::CAPTION)
                                        .text_color(colors.text_muted)
                                        .truncate()
                                        .child(format!(
                                            "{cwd} · {}",
                                            task_count_label(state.cuenv.tasks.len())
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(mono)
                                .text_size(text::CAPTION)
                                .text_color(colors.text_muted)
                                .child(palette_chord),
                        ),
                )
                .child(
                    div()
                        .id("cuetty-task-sidebar-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(list),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_count_reads_naturally_for_one_task() {
        assert_eq!(task_count_label(1), "1 task");
        assert_eq!(task_count_label(14), "14 tasks");
    }
}
