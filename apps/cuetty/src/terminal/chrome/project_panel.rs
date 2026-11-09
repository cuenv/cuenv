//! The account of the project in the active tab's directory: what it is called, how it
//! runs, and the variables it declares. It reads what the project's CUE says and resolves
//! nothing, so a secret appears as the name of its resolver.
//!
//! It lives in a pill at the window's top right that opens over the terminal on hover,
//! or pinned to a sidebar the terminal makes room for.

use super::icons::Icon;
use super::tokens::{ChromeColors, Elevation, border, radius, size, space, text};
use super::{hover, tokens};

use crate::integrations::cuenv::{CuenvTask, CuenvTaskKind, EnvVariable, RailColor, SidebarMode};
use crate::terminal::engine;
use crate::terminal::engine::{SIDEBAR_TOGGLE_KEY, TerminalView};
use crate::terminal::model::Rgb;
use crate::terminal::motion::{HoverTarget, SidebarButton};
use crate::terminal::palette::Mode as PaletteMode;
use crate::terminal::presentation::TerminalTheme;
use gpui::prelude::FluentBuilder;
use gpui::{
    Context, Div, InteractiveElement, IntoElement, ParentElement, SharedString, Stateful,
    StatefulInteractiveElement, Styled,
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
    gpui::div()
        .mt(space::SM)
        .truncate()
        .text_size(text::CAPTION)
        .font_weight(text::MEDIUM)
        .text_color(colors.text_muted)
        .child(label.to_string())
}

/// Where one variable sits in the panel, and how it is drawn.
struct VariableRow<'row> {
    /// Its place among all the panel's variables, which tells GPUI's elements apart.
    place: usize,
    variable: &'row EnvVariable,
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
    gpui::div()
        .id(("cuetty-project-variable", place))
        .w_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .font_family(mono)
        .text_size(text::CAPTION)
        .line_height(text::LINE_HEIGHT)
        .tooltip(super::tooltip::chrome_tooltip(
            format!("{}\n{}", variable.name, variable.value.detail()),
            colors,
        ))
        .child(
            gpui::div()
                .truncate()
                .text_color(colors.text_secondary)
                .child(variable.name.clone()),
        )
        .child(
            gpui::div()
                .truncate()
                .text_color(colors.text_muted)
                .child(variable.value.summary()),
        )
}

impl TerminalView {
    /// Nothing unless the active tab's directory holds a project with something to say.
    /// It stays with the tab's directory after the shell has gone, as that is still what
    /// the directory declares. It scrolls past whatever room it is given.
    fn render_project_panel(&self, colors: &ChromeColors) -> Option<Stateful<Div>> {
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
            gpui::div()
                .id("cuetty-project-panel")
                .w_full()
                .flex()
                .flex_col()
                .gap(space::XS)
                .min_h_0()
                .overflow_y_scroll()
                .px(space::LG)
                .py(space::SM)
                .line_height(text::LINE_HEIGHT)
                .children(overview.name.clone().map(|name| {
                    gpui::div()
                        .flex_shrink_0()
                        .truncate()
                        .text_size(text::BODY)
                        .font_weight(text::MEDIUM)
                        .text_color(colors.text)
                        .child(name)
                }))
                .when(!summary.is_empty(), |this| {
                    this.child(
                        gpui::div()
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

/// What the closed pill calls the project: its name, then how it runs, so two projects
/// with no name can still be told apart.
fn pill_label(name: Option<&str>, runtime: Option<&str>) -> Option<String> {
    match (name, runtime) {
        (Some(name), Some(runtime)) => Some(format!("{name} · {runtime}")),
        (Some(label), None) | (None, Some(label)) => Some(label.to_string()),
        (None, None) => None,
    }
}

impl TerminalView {
    /// What the window has to say: its own trouble first, then the active tab's.
    fn chrome_notice(&self) -> Option<&str> {
        let integration = self.active_state().and_then(|state| state.marker_notice());
        self.registry.notice.as_deref().or(integration)
    }

    fn has_tasks(&self) -> bool {
        self.active_state()
            .is_some_and(|state| !state.task_catalogue().is_empty())
    }

    /// Whether the floating pill has anything to show. With no project, no tasks, and
    /// nothing to report it steps aside for the wordmark.
    pub(in crate::terminal) fn project_pill_shown(&self) -> bool {
        self.sidebar_mode() == SidebarMode::Floating
            && (self
                .active_state()
                .is_some_and(|state| !state.cuenv.overview.is_empty())
                || self.has_tasks()
                || self.chrome_notice().is_some())
    }

    /// The project's own colour for its mark: its `rail`, or the window's accent.
    fn project_mark_color(&self, colors: &ChromeColors) -> gpui::Rgba {
        self.active_state()
            .and_then(|state| state.cuenv.presentation.rail)
            .map_or(colors.accent, |RailColor(red, green, blue)| {
                colors.mark(Rgb(red, green, blue))
            })
    }

    /// The hint naming the command palette's chord, which opens it when clicked.
    fn render_palette_hint(&self, colors: &ChromeColors, cx: &mut Context<Self>) -> Stateful<Div> {
        let target = HoverTarget::Button(SidebarButton::PaletteHint);
        gpui::div()
            .id("cuetty-palette-hint")
            .relative()
            .flex_shrink_0()
            .mx(space::SM)
            .mt(space::SM)
            .px(space::SM)
            .py(space::XS)
            .flex()
            .items_center()
            .justify_between()
            .gap(space::MD)
            .rounded(radius::CONTROL)
            .bg(tokens::faded(colors.state, self.hover_level(target)))
            .on_hover(hover::hover_fade(target, cx))
            .child(hover::hover_probe(target, cx))
            .cursor_pointer()
            .text_size(text::CAPTION)
            .text_color(colors.text_muted)
            .on_click(cx.listener(|view, _event, window, cx| {
                view.open_command_palette(PaletteMode::Commands);
                view.focus_terminal(window);
                cx.notify();
            }))
            .child(gpui::div().min_w_0().truncate().child("Commands"))
            .child(
                gpui::div()
                    .flex_none()
                    .font_family(self.mono_font_family())
                    .child(self.palette_chord_label()),
            )
    }

    /// The project's controls: what there is to report, its tasks, and where it lives.
    fn render_project_footer(&self, colors: &ChromeColors, cx: &mut Context<Self>) -> Div {
        let colors = *colors;
        // A message does not fit here, and an evaluation error can run to hundreds of
        // characters. A marker reports that there is one and can never displace the
        // controls.
        let notice_marker = self.chrome_notice().map(|notice| {
            self.control(SidebarButton::Notice, &colors, cx)
                .tooltip(super::tooltip::chrome_tooltip(notice.to_string(), colors))
                .child(
                    gpui::div()
                        .size(size::NOTICE_MARK)
                        .rounded(radius::CONTROL)
                        .bg(colors.accent),
                )
        });
        let palette_chord = self.palette_chord_label();
        let tasks = self.has_tasks().then(|| {
            self.control(SidebarButton::Tasks, &colors, cx)
                .when(
                    self.palette_open
                        && PaletteMode::from_query(&self.palette_query) == PaletteMode::Tasks,
                    |this| this.bg(colors.state),
                )
                .cursor_pointer()
                .tooltip(super::tooltip::chrome_tooltip(
                    format!("Search Cuenv tasks · {palette_chord}, then $"),
                    colors,
                ))
                .on_click(cx.listener(|view, _event, window, cx| {
                    view.open_command_palette(PaletteMode::Tasks);
                    view.focus_terminal(window);
                    cx.notify();
                }))
                .child(Icon::Tasks.element(colors.text_secondary))
        });
        let (toggle_label, toggle_icon) = match self.sidebar_mode() {
            SidebarMode::Pinned => ("Unpin from the sidebar", Icon::SidebarCollapse),
            SidebarMode::Floating => ("Pin to the sidebar", Icon::SidebarExpand),
        };
        let toggle = self
            .control(SidebarButton::Toggle, &colors, cx)
            .cursor_pointer()
            .tooltip(super::tooltip::chrome_tooltip(
                format!(
                    "{toggle_label} · {}",
                    engine::app_chord_label(SIDEBAR_TOGGLE_KEY)
                ),
                colors,
            ))
            .on_click(cx.listener(|view, _event, _window, cx| view.toggle_sidebar(cx)))
            .child(toggle_icon.element(colors.text_secondary));
        gpui::div()
            .w_full()
            .flex_shrink_0()
            .flex()
            .flex_row()
            .items_center()
            .gap(space::XS)
            .p(space::SM)
            .children(notice_marker)
            .children(tasks)
            // The toggle sits at the far end, where the sidebar's edge is.
            .child(gpui::div().flex_1())
            .child(toggle)
    }

    /// What the project panel says when the directory has no project to describe.
    fn render_no_project(colors: &ChromeColors) -> Div {
        gpui::div()
            .px(space::LG)
            .py(space::SM)
            .text_size(text::CAPTION)
            .text_color(colors.text_muted)
            .child("No Cuenv project here")
    }

    /// The project pinned to a sidebar, which the terminal makes room for.
    pub(in crate::terminal) fn render_project_sidebar(
        &self,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = ChromeColors::from_theme(theme);
        let body = match self.render_project_panel(&colors) {
            Some(panel) => panel.flex_1().into_any_element(),
            None => gpui::div()
                .flex_1()
                .child(Self::render_no_project(&colors))
                .into_any_element(),
        };
        gpui::div()
            .relative()
            .w_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(radius::PANEL)
            .bg(colors.sidebar)
            .border(border::HAIRLINE)
            .border_color(colors.divider)
            .pt(space::SM)
            .child(body)
            .child(self.render_palette_hint(&colors, cx))
            .child(self.render_project_footer(&colors, cx))
            .into_any_element()
    }

    /// The project floating at the window's top right: a pill naming it, which opens on
    /// hover into the same panel the sidebar holds. `None` while the project is pinned,
    /// or there is nothing to show.
    pub(in crate::terminal) fn render_project_pill(
        &self,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.project_pill_shown() {
            return None;
        }
        let colors = ChromeColors::from_theme(theme);
        let overview = self.active_state().map(|state| &state.cuenv.overview);
        let label = overview
            .and_then(|overview| pill_label(overview.name.as_deref(), overview.runtime.as_deref()))
            .unwrap_or_else(|| "Cuenv".to_string());
        let reveal = self.hover_level(HoverTarget::ProjectPill);
        let pill = tokens::elevated(gpui::div(), Elevation::Raised, &colors)
            .id("cuetty-project-pill")
            .occlude()
            .rounded_full()
            .h(size::PROJECT_PILL_HEIGHT)
            .flex_none()
            .flex()
            .items_center()
            .gap(space::SM)
            .px(space::LG)
            .text_size(text::LABEL)
            .text_color(if reveal > 0.0 {
                colors.text
            } else {
                colors.text_secondary
            })
            .child(
                gpui::div()
                    .size(size::NOTICE_MARK)
                    .flex_none()
                    .rounded(radius::CONTROL)
                    .bg(self.project_mark_color(&colors)),
            )
            .child(
                gpui::div()
                    .max_w(size::PROJECT_POPOVER_WIDTH)
                    .truncate()
                    .child(label),
            )
            .when(self.chrome_notice().is_some(), |this| {
                this.child(
                    gpui::div()
                        .size(size::NOTICE_MARK)
                        .flex_none()
                        .rounded_full()
                        .bg(colors.accent),
                )
            });
        let popover = (reveal > 0.0).then(|| {
            tokens::elevated(gpui::div(), Elevation::Floating, &colors)
                .id("cuetty-project-popover")
                .occlude()
                .w(size::PROJECT_POPOVER_WIDTH)
                .max_h(size::PROJECT_POPOVER_MAX_HEIGHT)
                .min_h_0()
                .flex()
                .flex_col()
                .overflow_hidden()
                .opacity(reveal)
                .pt(space::SM)
                .child(match self.render_project_panel(&colors) {
                    Some(panel) => panel.flex_shrink().into_any_element(),
                    None => Self::render_no_project(&colors).into_any_element(),
                })
                .child(self.render_palette_hint(&colors, cx))
                .child(self.render_project_footer(&colors, cx))
        });
        Some(
            gpui::div()
                .relative()
                .flex()
                .flex_col()
                .items_end()
                .gap(space::XS)
                // A little reach, so the pointer can cross to the panel without it closing.
                .pl(space::MD)
                .pb(space::MD)
                .font_family(tokens::ui_font_family())
                .child(pill)
                .children(popover)
                .child(hover::hover_region(HoverTarget::ProjectPill, cx))
                .into_any_element(),
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
    fn the_pill_names_the_project_then_how_it_runs() {
        assert_eq!(
            pill_label(Some("cuenv"), Some("nix")).as_deref(),
            Some("cuenv · nix")
        );
        assert_eq!(pill_label(Some("cuenv"), None).as_deref(), Some("cuenv"));
        assert_eq!(pill_label(None, Some("nix")).as_deref(), Some("nix"));
        assert_eq!(pill_label(None, None), None);
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
