use super::icons::Icon;
use super::tokens::{
    ChromeColors, Elevation, border, elevated, faded, radius, size, space, tab_marker_top, text,
    ui_font_family,
};
use super::tooltip::chrome_tooltip;
use super::{hover_fade, hover_probe};
use crate::integrations::cuenv::{RailColor, SidebarMode};
use crate::terminal::engine::{
    SIDEBAR_TOGGLE_KEY, TabClick, TerminalView, app_chord_label, tab_number_hint,
};
use crate::terminal::model::Rgb;
use crate::terminal::motion::{HoverTarget, SidebarButton};
use crate::terminal::palette::Mode as PaletteMode;
use crate::terminal::presentation::TerminalTheme;
use crate::terminal::tab_label::{TabMarkSources, tab_mark, tab_subtitle, tab_tooltip};
use crate::terminal::tab_rename::TabRename;
use crate::terminal::tab_status::TabIndicator;
use crate::terminal::workspace_shell::TabNumber;
use crate::workspace::{PaneId, TabId};
use gpui::{
    AppContext, ClickEvent, Context, Div, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Render, Rgba, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Window, div, prelude::FluentBuilder,
};

/// A tab being dragged to another place in the list, and what follows the pointer.
#[derive(Clone)]
struct DraggedTab {
    id: TabId,
    label: SharedString,
    colors: ChromeColors,
}

impl Render for DraggedTab {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        elevated(div(), Elevation::Raised, &self.colors)
            .max_w(size::SIDEBAR_EXPANDED_WIDTH)
            .h(size::TAB_ROW_HEIGHT)
            .px(space::LG)
            .flex()
            .items_center()
            .font_family(ui_font_family())
            .text_size(text::BODY)
            .text_color(self.colors.text)
            .child(div().truncate().child(self.label.clone()))
    }
}

/// The active tab's marker: the project's `rail` from a matching `package cuetty`,
/// or the window's accent. The colour only ever fills this small mark, so a project
/// can never choose a fill the rail's text has to survive, and it moves only as far as
/// showing on the user's theme takes.
fn active_tab_marker_color(project_accent: Option<RailColor>, colors: &ChromeColors) -> Rgba {
    project_accent.map_or(colors.accent, |RailColor(red, green, blue)| {
        colors.mark(Rgb(red, green, blue))
    })
}

/// Where the tab list has to scroll to: the active tab's position, once for each
/// time it changes, so wheel scrolling through the list is left alone in between.
fn rail_scroll_target(
    active_tab: Option<TabId>,
    revealed_tab: Option<TabId>,
    mut tabs: impl Iterator<Item = TabId>,
) -> Option<usize> {
    active_tab
        .filter(|_| active_tab != revealed_tab)
        .and_then(|active| tabs.position(|tab| tab == active))
}

impl TerminalView {
    /// Keeps the active tab's marker on screen when the list is taller than the
    /// rail, whichever way the tab became active.
    pub(in crate::terminal) fn reveal_active_rail_tab(&mut self, active_tab: Option<TabId>) {
        let tabs = self.registry.workspace.workspace().tabs();
        if let Some(index) = rail_scroll_target(
            active_tab,
            self.rail_revealed_tab,
            tabs.iter().map(|tab| tab.id),
        ) {
            self.rail_scroll.scroll_to_item(index);
        }
        self.rail_revealed_tab = active_tab;
    }

    pub(in crate::terminal) fn render_sidebar(
        &self,
        active_tab: Option<TabId>,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = ChromeColors::from_theme(theme);
        let mode = self.sidebar_mode();
        let marker_color = active_tab_marker_color(
            self.active_state()
                .and_then(|state| state.cuenv.presentation.rail),
            &colors,
        );
        let now = std::time::Instant::now();
        let tab_list = self.registry.workspace.workspace().tabs();
        let tabs = tab_list
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let label = self.registry.tab_label(tab);
                let cuenv = self
                    .registry
                    .sessions
                    .get(&tab.focused)
                    .map(|state| &state.cuenv);
                let banner = cuenv.and_then(|cuenv| cuenv.presentation.banner.as_deref());
                let state = self.registry.sessions.get(&tab.focused);
                let indicator = state.and_then(|state| state.status.indicator());
                let subtitle = tab_subtitle(
                    cuenv.and_then(|cuenv| cuenv.cwd.as_deref()),
                    state.and_then(|state| state.branch.branch()),
                    &label,
                );
                let tooltip = tab_tooltip(&label, banner);
                let tooltip = match indicator {
                    Some(indicator) => format!("{tooltip}\n{}", indicator.description()),
                    None => tooltip,
                };
                let mark = tab_mark(TabMarkSources {
                    custom_name: tab.custom_name.as_deref(),
                    banner,
                    directory: cuenv.and_then(|cuenv| cuenv.cwd.as_deref()),
                    label: &label,
                });
                render_tab_row(
                    TabRow {
                        id: tab.id,
                        index,
                        pane: tab.focused,
                        label,
                        subtitle,
                        editing: self
                            .tab_rename
                            .as_ref()
                            .filter(|rename| rename.tab() == tab.id)
                            .map(TabRename::text)
                            .map(str::to_owned),
                        mark,
                        tooltip,
                        indicator,
                        number: self
                            .tab_hints_visible
                            .then(|| TabNumber::of_position(index, tab_list.len()))
                            .flatten(),
                        selected: active_tab == Some(tab.id),
                        hover: self.motion.hover_level(HoverTarget::Tab(tab.id), now),
                        close_hover: self.motion.hover_level(HoverTarget::TabClose(tab.id), now),
                        active: self.motion.active_level(tab.id, now),
                        mode,
                        colors,
                        mono: self.mono_font_family(),
                    },
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let tabs = div()
            .id("cuetty-rail-tabs")
            .track_scroll(&self.rail_scroll)
            .overflow_y_scroll()
            .flex_1()
            .flex()
            .flex_col()
            .gap(space::XXS)
            .p(space::MD)
            .min_h_0()
            .children(tabs)
            // One marker for the whole list, which travels between rows. It follows them as
            // the list scrolls, being a child of what scrolls, and comes after them so a
            // scroll to a tab's position still counts rows.
            .children(self.motion.marker_place(now).map(|place| {
                div()
                    .absolute()
                    .left(space::MD + border::HAIRLINE)
                    .top(tab_marker_top(place))
                    .w(size::TAB_MARKER_WIDTH)
                    .h(size::TAB_MARKER_HEIGHT)
                    .rounded(radius::CONTROL)
                    .bg(marker_color)
            }));
        let integration_notice = active_tab
            .and_then(|tab_id| tab_list.iter().find(|tab| tab.id == tab_id))
            .and_then(|tab| self.registry.sessions.get(&tab.focused))
            .and_then(|state| state.marker_notice());
        // A message does not fit here, and an evaluation error can run to hundreds
        // of characters. A marker reports that there is one and can never displace
        // the tabs or controls.
        let notice_marker = self
            .registry
            .notice
            .as_deref()
            .or(integration_notice)
            .map(|notice| {
                self.control(SidebarButton::Notice, &colors, cx)
                    .tooltip(chrome_tooltip(notice.to_string(), colors))
                    .child(
                        div()
                            .size(size::NOTICE_MARK)
                            .rounded(radius::CONTROL)
                            .bg(colors.accent),
                    )
            });
        let has_tasks = self
            .active_state()
            .is_some_and(|state| !state.task_catalogue().is_empty());
        let new_tab = self
            .control(SidebarButton::NewTab, &colors, cx)
            .cursor_pointer()
            .tooltip(chrome_tooltip(
                format!("New tab · {}", app_chord_label("t")),
                colors,
            ))
            .on_click(cx.listener(|view, _event, window, cx| {
                view.new_tab(window, cx);
            }))
            .child(Icon::Plus.element(colors.text_secondary));
        let palette_chord = self.palette_chord_label();
        let tasks = self
            .control(SidebarButton::Tasks, &colors, cx)
            .when(
                self.palette_open
                    && PaletteMode::from_query(&self.palette_query) == PaletteMode::Tasks,
                |this| this.bg(colors.state),
            )
            .cursor_pointer()
            .tooltip(chrome_tooltip(
                format!("Search Cuenv tasks · {palette_chord}, then $"),
                colors,
            ))
            .on_click(cx.listener(|view, _event, window, cx| {
                view.open_command_palette(PaletteMode::Tasks);
                view.focus_terminal(window);
                cx.notify();
            }))
            .child(Icon::Tasks.element(colors.text_secondary));
        let (toggle_label, toggle_icon) = match mode {
            SidebarMode::Expanded => ("Collapse sidebar", Icon::SidebarCollapse),
            SidebarMode::Collapsed => ("Expand sidebar", Icon::SidebarExpand),
        };
        let toggle = self
            .control(SidebarButton::Toggle, &colors, cx)
            .cursor_pointer()
            .tooltip(chrome_tooltip(
                format!("{toggle_label} · {}", app_chord_label(SIDEBAR_TOGGLE_KEY)),
                colors,
            ))
            .on_click(cx.listener(|view, _event, _window, cx| view.toggle_sidebar(cx)))
            .child(toggle_icon.element(colors.text_secondary));
        let palette_hint = (mode == SidebarMode::Expanded).then(|| {
            let target = HoverTarget::Button(SidebarButton::PaletteHint);
            div()
                .id("cuetty-palette-hint")
                .mx(space::SM)
                .mt(space::SM)
                .px(space::SM)
                .py(space::XS)
                .flex()
                .items_center()
                .justify_between()
                .gap(space::MD)
                .rounded(radius::CONTROL)
                .bg(faded(colors.state, self.hover_level(target)))
                .on_hover(hover_fade(target, cx))
                .child(hover_probe(target, cx))
                .cursor_pointer()
                .text_size(text::CAPTION)
                .text_color(colors.text_muted)
                .on_click(cx.listener(|view, _event, window, cx| {
                    view.open_command_palette(PaletteMode::Commands);
                    view.focus_terminal(window);
                    cx.notify();
                }))
                .child(div().min_w_0().truncate().child("Commands"))
                .child(
                    div()
                        .flex_none()
                        .font_family(self.mono_font_family())
                        .child(palette_chord.clone()),
                )
        });
        let footer = div()
            .w_full()
            .flex_shrink_0()
            .flex()
            .gap(space::XS)
            .p(space::SM)
            .map(|this| match mode {
                SidebarMode::Expanded => this.flex_row().items_center(),
                SidebarMode::Collapsed => this.flex_col().items_center(),
            })
            .children(notice_marker)
            .child(new_tab)
            .when(has_tasks, |this| this.child(tasks))
            // Expanded, the toggle sits at the far end of the row, where the
            // collapsed sidebar's edge will be.
            .when(mode == SidebarMode::Expanded, |this| {
                this.child(div().flex_1())
            })
            .child(toggle);
        div()
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
            .child(tabs)
            .children(palette_hint)
            .child(footer)
            .into_any_element()
    }

    /// The square-ish hit target every sidebar control shares.
    fn control(
        &self,
        button: SidebarButton,
        colors: &ChromeColors,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let target = HoverTarget::Button(button);
        div()
            .id(match button {
                SidebarButton::Notice => "cuetty-notice",
                SidebarButton::NewTab => "cuetty-new-tab",
                SidebarButton::Tasks => "cuetty-tasks",
                SidebarButton::Toggle => "cuetty-sidebar-toggle",
                SidebarButton::PaletteHint => "cuetty-palette-hint",
            })
            .w(size::CONTROL_WIDTH)
            .h(size::CONTROL_HEIGHT)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(radius::CONTROL)
            .bg(faded(colors.state, self.hover_level(target)))
            .on_hover(hover_fade(target, cx))
            .child(hover_probe(target, cx))
    }
}

/// A tab's status at a glance. Every state has its own shape, so it reads the same
/// whatever colours the theme gives it.
fn render_indicator(indicator: TabIndicator, colors: &ChromeColors) -> Div {
    let slot = div()
        .size(size::STATUS_ICON)
        .flex_none()
        .flex()
        .items_center()
        .justify_center();
    let icon = |icon: Icon, color: Rgba| icon.element(color).size(size::STATUS_ICON);
    match indicator {
        TabIndicator::Failed => slot.child(icon(Icon::Failed, colors.danger)),
        TabIndicator::Bell => slot.child(icon(Icon::Bell, colors.accent)),
        TabIndicator::Running => slot.child(icon(Icon::Running, colors.text_secondary)),
        TabIndicator::Finished => slot.child(icon(Icon::Finished, colors.text_secondary)),
        TabIndicator::Unread => slot.child(
            div()
                .size(size::NOTICE_MARK)
                .rounded(radius::CONTROL)
                .bg(colors.accent),
        ),
    }
}

/// A tab's name being edited, in the palette's manner: text is typed at its end, so
/// the caret leads a placeholder and follows typed text, and a name too long for the
/// field clips at its start to keep the end being typed in view.
fn render_name_field(editing: String, label: String, colors: &ChromeColors) -> Div {
    let placeholder = editing.is_empty();
    let caret = || {
        div()
            .flex_none()
            .w(size::CARET_WIDTH)
            .h(size::CARET_HEIGHT)
            .bg(colors.accent)
    };
    div()
        .flex()
        .items_center()
        .min_w_0()
        .px(space::XS)
        .rounded(radius::CONTROL)
        .bg(colors.well)
        .text_color(if placeholder {
            colors.text_muted
        } else {
            colors.text
        })
        .when(placeholder, |this| this.child(caret()))
        .child(
            div()
                .min_w_0()
                .flex()
                .justify_end()
                .overflow_hidden()
                .child(
                    div()
                        .flex_shrink_0()
                        .whitespace_nowrap()
                        .child(if placeholder { label } else { editing }),
                ),
        )
        .when(!placeholder, |this| this.child(caret()))
}

/// Which tab a close button closes, and how it is drawn.
struct CloseButton {
    id: TabId,
    pane: PaneId,
    /// How far its row's hover has come, which it fades in with.
    reveal: f32,
    /// How far the pointer's hover over the button itself has come.
    hover: f32,
    colors: ChromeColors,
}

/// The button that closes a tab, which stays out of sight until its row is hovered.
/// The press stops here, so it neither selects the tab nor starts dragging it.
fn render_close_button(button: CloseButton, cx: &mut Context<TerminalView>) -> Stateful<Div> {
    let CloseButton {
        id,
        pane,
        reveal,
        hover,
        colors,
    } = button;
    div()
        .id(("cuetty-rail-tab-close", id.get()))
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius::CONTROL)
        .opacity(reveal)
        // The row beneath is already filled with `state` while the pointer is on it.
        .bg(faded(colors.state_on_row, hover))
        .on_hover(hover_fade(HoverTarget::TabClose(id), cx))
        .child(hover_probe(HoverTarget::TabClose(id), cx))
        .cursor_pointer()
        .tooltip(chrome_tooltip(
            format!("Close tab · {}", app_chord_label("w")),
            colors,
        ))
        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
            cx.stop_propagation();
        })
        .on_click(cx.listener(move |view, _event, window, cx| {
            cx.stop_propagation();
            view.close_pane(pane, cx);
            view.focus_terminal(window);
            cx.notify();
        }))
        .child(
            Icon::Close
                .element(colors.text_secondary)
                .size(size::STATUS_ICON),
        )
}

/// What a row in the tab list shows.
struct TabRow {
    id: TabId,
    /// Where the tab is in the list, which is where another dropped on it goes.
    index: usize,
    pane: PaneId,
    label: String,
    /// The muted second line, left out when empty.
    subtitle: String,
    /// The name being typed, while the user is renaming this tab.
    editing: Option<String>,
    /// What stands in for the label when the sidebar is collapsed.
    mark: String,
    tooltip: String,
    indicator: Option<TabIndicator>,
    number: Option<TabNumber>,
    selected: bool,
    /// How far the pointer's hover over this row has faded in.
    hover: f32,
    /// How far the pointer's hover over its close button has faded in.
    close_hover: f32,
    /// How strongly the row shows as the active one, which fades as tabs switch.
    active: f32,
    mode: SidebarMode,
    colors: ChromeColors,
    mono: SharedString,
}

fn render_tab_row(row: TabRow, cx: &mut Context<TerminalView>) -> Stateful<Div> {
    let TabRow {
        id,
        index,
        pane,
        label,
        subtitle,
        editing,
        mark,
        tooltip,
        indicator,
        number,
        selected,
        hover,
        close_hover,
        active,
        mode,
        colors,
        mono,
    } = row;
    let drop_outline = colors.accent;
    let dragged = DraggedTab {
        id,
        label: label.clone().into(),
        colors,
    };
    let text_color = if selected {
        colors.text
    } else {
        colors.text_secondary
    };
    // The active row's fill is a step lighter, which muted text would fade into.
    let subtitle_color = if selected {
        colors.text_secondary
    } else {
        colors.text_muted
    };
    div()
        .id(("cuetty-rail-tab", id.get()))
        .relative()
        .w_full()
        .h(size::TAB_ROW_TWO_LINE_HEIGHT)
        .flex_shrink_0()
        .flex()
        .items_center()
        .rounded(radius::CONTROL)
        .bg(faded(colors.state, hover.max(active)))
        .on_hover(hover_fade(HoverTarget::Tab(id), cx))
        .child(hover_probe(HoverTarget::Tab(id), cx))
        .cursor_pointer()
        .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
            view.tab_row_clicked(
                TabClick {
                    tab: id,
                    click_count: event.click_count(),
                },
                window,
                cx,
            );
        }))
        // Runs before whatever was clicked, so it finds the edit settled. The row is what
        // counts as inside, where a click leaves the edit alone.
        .when(editing.is_some(), |this| {
            this.on_mouse_down_out(cx.listener(|view, _event, _window, cx| {
                view.commit_tab_rename();
                cx.notify();
            }))
        })
        // A dragged tab takes the place of the row it is dropped on, so the outline
        // marks that row. It is there all along to keep the row's size from changing.
        .border(border::HAIRLINE)
        .border_color(gpui::transparent_black())
        .on_drag(dragged, |dragged, _offset, _window, cx| {
            cx.new(|_| dragged.clone())
        })
        .drag_over::<DraggedTab>(move |this, _dragged, _window, _cx| {
            this.border_color(drop_outline)
        })
        .on_drop(cx.listener(move |view, dragged: &DraggedTab, _window, cx| {
            view.move_tab(dragged.id, index, cx);
        }))
        .tooltip(chrome_tooltip(tooltip, colors))
        .text_size(text::BODY)
        .text_color(text_color)
        .font_weight(if selected {
            text::MEDIUM
        } else {
            text::REGULAR
        })
        .map(|this| match mode {
            SidebarMode::Expanded => this
                .gap(space::MD)
                .px(space::LG)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .line_height(text::LINE_HEIGHT)
                        .child(match editing {
                            Some(editing) => render_name_field(editing, label, &colors),
                            None => div().truncate().child(label),
                        })
                        .when(!subtitle.is_empty(), |this| {
                            this.child(
                                div()
                                    .truncate()
                                    .text_size(text::CAPTION)
                                    .text_color(subtitle_color)
                                    .font_weight(text::REGULAR)
                                    .child(subtitle),
                            )
                        }),
                )
                .children(number.map(|number| {
                    div()
                        .flex_none()
                        .font_family(mono.clone())
                        .text_size(text::CAPTION)
                        .text_color(colors.text_muted)
                        .font_weight(text::REGULAR)
                        .child(tab_number_hint(number))
                }))
                .child(
                    // The button takes over from the indicator while the row is hovered.
                    div()
                        .relative()
                        .size(size::TAB_ACTION)
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .children(indicator.map(|indicator| {
                            render_indicator(indicator, &colors).opacity(1.0 - hover)
                        }))
                        .child(
                            render_close_button(
                                CloseButton {
                                    id,
                                    pane,
                                    reveal: hover,
                                    hover: close_hover,
                                    colors,
                                },
                                cx,
                            )
                            .absolute()
                            .inset_0(),
                        ),
                ),
            // Only a character fits, so the hint stands in for the mark. The row is too
            // narrow for a close button to clear the mark, where a click on the tab lands,
            // so a collapsed tab has none: selecting it and the close chord do that.
            SidebarMode::Collapsed => this
                .justify_center()
                .child(
                    div()
                        .h(size::TAB_MARK_HEIGHT)
                        .line_height(size::TAB_MARK_HEIGHT)
                        .child(match number {
                            Some(number) => div()
                                .font_family(mono.clone())
                                .text_size(text::LABEL)
                                .text_color(colors.text_muted)
                                .font_weight(text::REGULAR)
                                .child(number.digit().to_string()),
                            None => div().child(mark),
                        }),
                )
                .children(indicator.map(|indicator| {
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(render_indicator(indicator, &colors))
                })),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::config::TerminalConfig;

    #[test]
    fn a_collapsed_tabs_indicator_sits_clear_of_its_mark() {
        let row = size::TAB_ROW_TWO_LINE_HEIGHT - border::HAIRLINE * 2.0;
        let mark_bottom = (row + size::TAB_MARK_HEIGHT) / 2.0;
        let indicator_top = row - size::STATUS_ICON;
        assert!(mark_bottom <= indicator_top);
    }

    #[test]
    fn active_tab_marker_prefers_the_project_accent_over_the_theme_accent() {
        let theme = TerminalTheme::from_config(&TerminalConfig::default());
        let colors = ChromeColors::from_theme(&theme);
        assert_eq!(active_tab_marker_color(None, &colors), colors.accent);
        assert_eq!(
            active_tab_marker_color(Some(RailColor(255, 0, 0)), &colors),
            gpui::rgb(0xff0000)
        );
    }

    #[test]
    fn the_tab_list_scrolls_once_for_each_change_of_active_tab() {
        let (first, second, closed) = (TabId::new(1), TabId::new(2), TabId::new(3));
        let tabs = || [first, second].into_iter();
        assert_eq!(rail_scroll_target(Some(second), None, tabs()), Some(1));
        assert_eq!(
            rail_scroll_target(Some(first), Some(second), tabs()),
            Some(0)
        );
        assert_eq!(rail_scroll_target(Some(second), Some(second), tabs()), None);
        assert_eq!(rail_scroll_target(None, Some(second), tabs()), None);
        assert_eq!(rail_scroll_target(Some(closed), Some(first), tabs()), None);
    }
}
