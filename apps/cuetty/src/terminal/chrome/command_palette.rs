use super::tokens::{ChromeColors, Elevation, radius, size, space, text};
use super::{hover, tokens};
use crate::terminal::engine;
use crate::terminal::engine::{PALETTE_VISIBLE_ROWS, TerminalView};
use crate::terminal::motion::HoverTarget;
use crate::terminal::palette::{Command, Item, Mode};
use crate::terminal::presentation::TerminalTheme;
use gpui::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
};

fn shortcut(command: Command) -> String {
    match command {
        Command::NewTab => engine::app_chord_label("t"),
        Command::CloseTab => engine::app_chord_label("w"),
        Command::RenameTab => engine::app_chord_label("r"),
        Command::ToggleSidebar => engine::app_chord_label("b"),
        Command::FindOnScreen => engine::app_chord_label("f"),
        Command::NextTab => "Ctrl+Page Down".into(),
        Command::PreviousTab => "Ctrl+Page Up".into(),
    }
}

impl TerminalView {
    pub(in crate::terminal) fn render_command_palette(
        &self,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.palette_open {
            return None;
        }
        let colors = ChromeColors::from_theme(theme);
        let mode = Mode::from_query(&self.palette_query);
        let items = self.palette_items();
        // The field and hint stay put while results scroll in a short window.
        let mut results = gpui::div()
            .id("cuetty-command-palette-results")
            .track_scroll(&self.palette_scroll)
            .overflow_y_scroll()
            .min_h_0()
            .flex()
            .flex_col()
            .gap(space::XS);
        for (index, item) in items.iter().enumerate() {
            let selected = index == self.palette_selection;
            let item_to_run = item.clone();
            let target = HoverTarget::PaletteRow(index);
            let (kind, name, detail, shortcut) = match item {
                Item::Command(command) => (
                    None,
                    command.name().to_owned(),
                    command.description().to_owned(),
                    Some(shortcut(*command)),
                ),
                Item::Task(task) => (
                    Some(if task.requires_parameters {
                        "ARGS"
                    } else {
                        task.kind.label()
                    }),
                    task.name.clone(),
                    task.description.clone().unwrap_or_else(|| {
                        if task.requires_parameters {
                            "Requires arguments".into()
                        } else {
                            task.kind.label().to_ascii_lowercase()
                        }
                    }),
                    None,
                ),
            };
            let metadata = if selected {
                colors.text_secondary
            } else {
                colors.text_muted
            };
            results = results.child(
                gpui::div()
                    .id(("cuetty-command-palette-row", index))
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
                        tokens::faded(colors.panel_hover, self.hover_level(target))
                    })
                    .on_hover(hover::hover_fade(target, cx))
                    .child(hover::hover_probe(target, cx))
                    .cursor_pointer()
                    .on_click(cx.listener(move |view, _event, window, cx| {
                        view.run_palette_item(item_to_run.clone(), window, cx);
                    }))
                    .children(kind.map(|kind| {
                        gpui::div()
                            .w(size::PALETTE_KIND_WIDTH)
                            .flex_shrink_0()
                            .text_size(text::CAPTION)
                            .text_color(metadata)
                            .child(kind)
                    }))
                    .child(
                        gpui::div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                gpui::div()
                                    .truncate()
                                    .text_size(text::LABEL)
                                    .text_color(colors.text)
                                    .child(name),
                            )
                            .child(
                                gpui::div()
                                    .truncate()
                                    .text_size(text::CAPTION)
                                    .text_color(metadata)
                                    .child(detail),
                            ),
                    )
                    .children(shortcut.map(|shortcut| {
                        gpui::div()
                            .flex_shrink_0()
                            .font_family(self.mono_font_family())
                            .text_size(text::CAPTION)
                            .text_color(metadata)
                            .child(shortcut)
                    })),
            );
        }
        if items.is_empty() {
            let message = match mode {
                Mode::Commands => "No matching commands",
                Mode::Tasks if self.palette_query.trim() == "$" => "No tasks available in this tab",
                Mode::Tasks => "No matching tasks",
            };
            results = results.child(
                gpui::div()
                    .px(space::LG)
                    .py(space::XL)
                    .text_size(text::LABEL)
                    .text_color(colors.text_muted)
                    .child(message),
            );
        }
        let hint = match mode {
            Mode::Commands => "$ tasks · ↑↓ select · return run · esc close",
            Mode::Tasks => "remove $ for commands · ↑↓ select · return run · esc close",
        };
        let panel = tokens::elevated(gpui::div(), Elevation::Modal, &colors)
            .occlude()
            .on_mouse_down_out(cx.listener(|view, _event, _window, cx| {
                view.close_command_palette(cx);
            }))
            .min_h_0()
            .p(space::LG)
            .flex()
            .flex_col()
            .gap(space::MD)
            .line_height(text::LINE_HEIGHT)
            .child(
                super::field::text_field(
                    &self.palette_query,
                    "Search commands or type $ for tasks…",
                    &colors,
                )
                .w_full()
                .h(size::PALETTE_FIELD_HEIGHT)
                .flex_shrink_0()
                .px(space::LG)
                .text_size(text::BODY),
            )
            .child(results)
            .child(
                gpui::div()
                    .flex_shrink_0()
                    .truncate()
                    .text_size(text::CAPTION)
                    .text_color(colors.text_muted)
                    .child(hint),
            );
        Some(
            gpui::div()
                .absolute()
                .inset_0()
                .occlude()
                .flex()
                .justify_center()
                .px(space::LG)
                .pt(size::PALETTE_TOP_OFFSET)
                .pb(space::LG)
                .child(
                    gpui::div()
                        .w_full()
                        .max_w(size::PALETTE_WIDTH)
                        .max_h(tokens::palette_height(PALETTE_VISIBLE_ROWS))
                        .flex()
                        .flex_col()
                        .child(hover::fading_in(
                            panel,
                            "cuetty-command-palette-fade",
                            self.motion_pace(),
                        )),
                )
                .into_any_element(),
        )
    }
}
