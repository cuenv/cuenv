use super::field::text_field;
use super::tokens::{
    ChromeColors, Elevation, elevated, faded, palette_height, radius, size, space, text,
};
use super::{fading_in, hover_fade, hover_probe};
use crate::terminal::engine::{PALETTE_RESULT_LIMIT, TerminalView, filtered_tasks};
use crate::terminal::motion::HoverTarget;
use crate::terminal::presentation::TerminalTheme;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div,
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
        let tasks = filtered_tasks(&state.cuenv.tasks, &self.task_query);
        // The field and hint stay put while this list gives up height, so a short
        // window clips results and never pushes the hint out of the panel.
        let mut results = div()
            .id("cuetty-task-palette-results")
            .track_scroll(&self.task_palette_scroll)
            .overflow_y_scroll()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(space::XS);
        for (index, task) in tasks.iter().take(PALETTE_RESULT_LIMIT).enumerate() {
            let selected = index == self.task_selection;
            let task_to_run = (*task).clone();
            let target = HoverTarget::PaletteRow(index);
            results = results.child(
                div()
                    .id(("cuetty-task-palette-row", index))
                    .w_full()
                    .h(size::PALETTE_ROW_HEIGHT)
                    .flex_shrink_0()
                    .px(space::LG)
                    .flex()
                    .items_center()
                    .gap(space::MD)
                    .rounded(radius::CONTROL)
                    .bg(if selected {
                        colors.panel_selected
                    } else {
                        faded(colors.panel_hover, self.hover_level(target))
                    })
                    .on_hover(hover_fade(target, cx))
                    .child(hover_probe(target, cx))
                    .cursor_pointer()
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.launch_task(task_to_run.clone(), cx);
                    }))
                    .child(
                        div()
                            .w(size::PALETTE_KIND_WIDTH)
                            .flex_shrink_0()
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
                            .truncate()
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
        let panel = elevated(div(), Elevation::Modal, &colors)
            .occlude()
            .on_mouse_down_out(cx.listener(|view, _event, _window, cx| {
                view.close_task_palette(cx);
            }))
            .min_h_0()
            .p(space::LG)
            .flex()
            .flex_col()
            .gap(space::MD)
            .line_height(text::LINE_HEIGHT)
            .child(
                text_field(&self.task_query, "Type to filter tasks…", &colors)
                    .w_full()
                    .h(size::PALETTE_FIELD_HEIGHT)
                    .flex_shrink_0()
                    .px(space::LG)
                    .text_size(text::BODY),
            )
            .child(results)
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(text::CAPTION)
                    .text_color(colors.text_muted)
                    .child("↑↓ select · return run · esc close"),
            );
        Some(
            div()
                .absolute()
                .inset_0()
                // The palette is modal over the terminal: pointer input stops
                // here instead of selecting or scrolling text it covers.
                .occlude()
                .flex()
                .justify_center()
                .pt(size::PALETTE_TOP_OFFSET)
                .pb(space::LG)
                .child(
                    // Stretched to the height left in the window, up to the height
                    // of a full result list. The panel inside takes only what its
                    // content needs of that, so a short list gets a short panel.
                    div()
                        .w(size::PALETTE_WIDTH)
                        .max_h(palette_height(PALETTE_RESULT_LIMIT))
                        .flex()
                        .flex_col()
                        .child(fading_in(
                            panel,
                            "cuetty-task-palette-fade",
                            self.motion_pace(),
                        )),
                )
                .into_any_element(),
        )
    }
}
