//! The sidebar's account of the project in the active tab's directory: what it is called,
//! how it runs, and the variables it declares. It reads what the project's CUE says and
//! resolves nothing, so a secret appears as the name of its resolver.

use super::tokens::{ChromeColors, border, size, space, text};
use super::tooltip::chrome_tooltip;
use crate::integrations::cuenv::{CuenvTask, CuenvTaskKind, EnvVariable};
use crate::terminal::engine::TerminalView;
use gpui::{
    Div, InteractiveElement, IntoElement, ParentElement, SharedString, Stateful,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder,
};

/// The muted line under the project's name, such as `nix · 12 tasks`.
fn project_summary(runtime: Option<&str>, tasks: &[CuenvTask]) -> String {
    let runnable = tasks
        .iter()
        .filter(|task| task.kind == CuenvTaskKind::Task)
        .count();
    let tasks = match runnable {
        0 => None,
        1 => Some("1 task".to_string()),
        many => Some(format!("{many} tasks")),
    };
    runtime
        .map(str::to_string)
        .into_iter()
        .chain(tasks)
        .collect::<Vec<_>>()
        .join(" · ")
}

/// A heading over a group of variables.
fn group_label(label: &str, colors: &ChromeColors) -> Div {
    div()
        .mt(space::SM)
        .truncate()
        .text_size(text::CAPTION)
        .font_weight(text::MEDIUM)
        .text_color(colors.text_muted)
        .child(label.to_string())
}

/// Where one variable sits in the panel, and how it is drawn.
struct VariableRow<'a> {
    /// Its place among all the panel's variables, which tells GPUI's elements apart.
    place: usize,
    variable: &'a EnvVariable,
    colors: ChromeColors,
    mono: SharedString,
}

/// A variable's name above its value, each cut to the sidebar's width, with the whole of
/// both in its tooltip.
fn render_variable(row: VariableRow<'_>) -> Stateful<Div> {
    let VariableRow {
        place,
        variable,
        colors,
        mono,
    } = row;
    div()
        .id(("cuetty-project-variable", place))
        .w_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .font_family(mono)
        .text_size(text::CAPTION)
        .line_height(text::LINE_HEIGHT)
        .tooltip(chrome_tooltip(
            format!("{}\n{}", variable.name, variable.value.detail()),
            colors,
        ))
        .child(
            div()
                .truncate()
                .text_color(colors.text_secondary)
                .child(variable.name.clone()),
        )
        .child(
            div()
                .truncate()
                .text_color(colors.text_muted)
                .child(variable.value.summary()),
        )
}

impl TerminalView {
    /// Nothing unless the active tab's directory holds a project with something to say.
    /// It stays with the tab's directory after the shell has gone, as that is still what
    /// the directory declares. It gives way to the tab list when the window is short.
    pub(super) fn render_project_panel(&self, colors: &ChromeColors) -> Option<Stateful<Div>> {
        let state = self.active_state()?;
        let overview = &state.cuenv.overview;
        if overview.is_empty() {
            return None;
        }
        let summary = project_summary(overview.runtime.as_deref(), state.task_catalogue());
        let groups =
            (!overview.variables.is_empty())
                .then_some(("Variables", overview.variables.as_slice()))
                .into_iter()
                .chain(overview.environments.iter().map(|environment| {
                    (environment.name.as_str(), environment.variables.as_slice())
                }));
        let mut place = 0;
        let mut body = Vec::new();
        for (label, variables) in groups {
            body.push(group_label(label, colors).into_any_element());
            for variable in variables {
                body.push(
                    render_variable(VariableRow {
                        place,
                        variable,
                        colors: *colors,
                        mono: self.mono_font_family(),
                    })
                    .into_any_element(),
                );
                place += 1;
            }
        }
        Some(
            div()
                .id("cuetty-project-panel")
                .w_full()
                .flex()
                .flex_col()
                .gap(space::XS)
                .min_h_0()
                .max_h(size::PROJECT_PANEL_MAX_HEIGHT)
                .overflow_y_scroll()
                .px(space::LG)
                .py(space::SM)
                .border_t(border::HAIRLINE)
                .border_color(colors.divider)
                .line_height(text::LINE_HEIGHT)
                .children(overview.name.clone().map(|name| {
                    div()
                        .flex_shrink_0()
                        .truncate()
                        .text_size(text::BODY)
                        .font_weight(text::MEDIUM)
                        .text_color(colors.text)
                        .child(name)
                }))
                .when(!summary.is_empty(), |this| {
                    this.child(
                        div()
                            .flex_shrink_0()
                            .truncate()
                            .text_size(text::CAPTION)
                            .text_color(colors.text_muted)
                            .child(summary),
                    )
                })
                .children(body),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(name: &str, kind: CuenvTaskKind) -> CuenvTask {
        CuenvTask {
            name: name.into(),
            description: None,
            kind,
            requires_parameters: false,
        }
    }

    #[test]
    fn the_summary_counts_what_can_run_and_leaves_out_what_is_not_known() {
        let tasks = [
            task("ci", CuenvTaskKind::Group),
            task("ci.lint", CuenvTaskKind::Task),
            task("ci.test", CuenvTaskKind::Task),
        ];
        assert_eq!(project_summary(Some("nix"), &tasks), "nix · 2 tasks");
        assert_eq!(project_summary(None, &tasks[..2]), "1 task");
        assert_eq!(project_summary(Some("nix"), &tasks[..1]), "nix");
        assert_eq!(project_summary(None, &[]), "");
    }
}
