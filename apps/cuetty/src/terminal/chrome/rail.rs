use super::icons::Icon;
use super::tokens;
use super::tokens::{ChromeColors, Elevation, border, radius, size, space, text};

use super::hover;
use crate::integrations::cuenv::RailColor;
use crate::terminal::engine;
use crate::terminal::engine::{TabClick, TerminalView};
use crate::terminal::model::Rgb;
use crate::terminal::motion::{HoverTarget, SidebarButton};
use crate::terminal::presentation::TerminalTheme;
use crate::terminal::tab_rename::TabRename;
use crate::terminal::tab_status::TabIndicator;
use crate::terminal::workspace_shell::TabNumber;
use crate::workspace::{PaneId, TabId};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, ClickEvent, Context, Div, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Render, Rgba, SharedString, Stateful, StatefulInteractiveElement, Styled,
    Window,
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
        tokens::elevated(gpui::div(), Elevation::Raised, &self.colors)
            .max_w(size::TAB_STACK_WIDTH)
            .h(size::TAB_ROW_HEIGHT)
            .px(space::LG)
            .flex()
            .items_center()
            .font_family(tokens::ui_font_family())
            .text_size(text::BODY)
            .text_color(self.colors.text)
            .child(gpui::div().truncate().child(self.label.clone()))
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

/// How a closed tab shows: the active one as a ring, the rest as dots coloured by what
/// they have to say, so a failure or a bell still catches the eye.
fn render_tab_dot(dot: TabDot, colors: &ChromeColors) -> Div {
    let fill = match dot.indicator {
        Some(TabIndicator::Failed) => Some(colors.danger),
        Some(TabIndicator::Bell | TabIndicator::Unread) => Some(colors.accent),
        Some(TabIndicator::Running) => Some(colors.text_secondary),
        Some(TabIndicator::Finished) | None => None,
    };
    let shape = match dot.ring {
        Some(ring) => gpui::div()
            .size(size::TAB_DOT_ACTIVE)
            .rounded_full()
            .border(size::TAB_DOT_RING)
            .border_color(ring)
            .when_some(fill, |this, fill| this.bg(fill)),
        None => gpui::div()
            .size(size::TAB_DOT)
            .rounded_full()
            .bg(fill.unwrap_or(colors.text_muted)),
    };
    gpui::div()
        .size(size::TAB_DOT_SLOT)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(shape)
}

/// One tab in the closed stack.
struct TabDot {
    /// The active tab's marker colour, which only it has.
    ring: Option<Rgba>,
    indicator: Option<TabIndicator>,
}

impl TerminalView {
    /// Whether the tab stack shows names. Besides the pointer coming near, anything that
    /// needs the names in sight holds it open: number hints, a name being typed, and a
    /// tab being dragged to a new place.
    pub(in crate::terminal) fn tab_stack_open(&self, cx: &App) -> bool {
        self.tab_hints_visible
            || self.tab_rename.is_some()
            || cx.has_active_drag()
            || self.hover_level(HoverTarget::TabStack) > 0.0
    }

    /// Keeps the active tab's marker on screen when the open list is taller than the
    /// window, whichever way the tab became active. The closed stack has nothing to
    /// scroll, so the list reveals the active tab again each time it opens.
    pub(in crate::terminal) fn reveal_active_rail_tab(
        &mut self,
        active_tab: Option<TabId>,
        cx: &App,
    ) {
        if !self.tab_stack_open(cx) {
            self.rail_revealed_tab = None;
            return;
        }
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

    /// The tabs, floating over the right of the terminal and centred on it. Closed, they
    /// are a column of dots that takes almost nothing from the terminal; the pointer
    /// coming near opens them into a list of names.
    pub(in crate::terminal) fn render_tab_stack(
        &self,
        active_tab: Option<TabId>,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = ChromeColors::from_theme(theme);
        let marker_color = self.tab_marker_color(&colors);
        let stack = if self.tab_stack_open(cx) {
            self.render_tab_list(active_tab, &colors, cx)
                .into_any_element()
        } else {
            let dots = self
                .registry
                .workspace
                .workspace()
                .tabs()
                .iter()
                .map(|tab| {
                    render_tab_dot(
                        TabDot {
                            ring: (active_tab == Some(tab.id)).then_some(marker_color),
                            indicator: self
                                .registry
                                .sessions
                                .get(&tab.focused)
                                .and_then(|state| state.status.indicator()),
                        },
                        &colors,
                    )
                })
                .collect::<Vec<_>>();
            tokens::elevated(gpui::div(), Elevation::Raised, &colors)
                .rounded_full()
                .max_h_full()
                .overflow_hidden()
                .flex()
                .flex_col()
                .items_center()
                .gap(space::XXS)
                .py(space::SM)
                .px(space::XXS)
                .children(dots)
                .into_any_element()
        };
        // Takes the window's height so the stack can centre in it, and has no listeners,
        // so the pointer passes through it to the terminal everywhere but the stack.
        gpui::div()
            .absolute()
            .top_0()
            .bottom_0()
            .right(space::LG)
            .py(space::LG)
            .flex()
            .flex_col()
            .justify_center()
            .child(
                gpui::div()
                    .relative()
                    .max_h_full()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .items_end()
                    // The reach: near enough the stack counts as over it.
                    .pl(size::TAB_STACK_REACH)
                    .py(space::LG)
                    .child(stack)
                    .child(hover::hover_region(HoverTarget::TabStack, cx)),
            )
            .into_any_element()
    }

    /// The active tab's marker colour, from the active tab's project.
    fn tab_marker_color(&self, colors: &ChromeColors) -> Rgba {
        active_tab_marker_color(
            self.active_state()
                .and_then(|state| state.cuenv.presentation.rail),
            colors,
        )
    }

    /// The open stack: a row for each tab, then a way to start another.
    fn render_tab_list(
        &self,
        active_tab: Option<TabId>,
        colors: &ChromeColors,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let marker_color = self.tab_marker_color(colors);
        let colors = *colors;
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
                let subtitle = crate::terminal::tab_label::tab_subtitle(
                    cuenv.and_then(|cuenv| cuenv.cwd.as_deref()),
                    state.and_then(|state| state.branch.branch()),
                    &label,
                );
                let tooltip = crate::terminal::tab_label::tab_tooltip(&label, banner);
                let tooltip = match indicator {
                    Some(indicator) => format!("{tooltip}\n{}", indicator.description()),
                    None => tooltip,
                };
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
                        colors,
                        mono: self.mono_font_family(),
                    },
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let tabs = gpui::div()
            .id("cuetty-rail-tabs")
            .track_scroll(&self.rail_scroll)
            .overflow_y_scroll()
            .relative()
            .min_h_0()
            .flex_shrink()
            .flex()
            .flex_col()
            .gap(space::XXS)
            .p(space::MD)
            .children(tabs)
            // One marker for the whole list, which travels between rows. It follows them as
            // the list scrolls, being a child of what scrolls, and comes after them so a
            // scroll to a tab's position still counts rows.
            .children(self.motion.marker_place(now).map(|place| {
                gpui::div()
                    .absolute()
                    .left(space::MD + border::HAIRLINE)
                    .top(tokens::tab_marker_top(place))
                    .w(size::TAB_MARKER_WIDTH)
                    .h(size::TAB_MARKER_HEIGHT)
                    .rounded(radius::CONTROL)
                    .bg(marker_color)
            }));
        let new_tab_target = HoverTarget::Button(SidebarButton::NewTab);
        let new_tab = gpui::div()
            .id("cuetty-new-tab")
            .relative()
            .mx(space::MD)
            .mb(space::MD)
            .h(size::CONTROL_HEIGHT)
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(space::MD)
            .px(space::LG)
            .rounded(radius::CONTROL)
            .bg(tokens::faded(
                colors.state,
                self.hover_level(new_tab_target),
            ))
            .on_hover(hover::hover_fade(new_tab_target, cx))
            .child(hover::hover_probe(new_tab_target, cx))
            .cursor_pointer()
            .text_size(text::BODY)
            .text_color(colors.text_secondary)
            .on_click(cx.listener(|view, _event, window, cx| {
                view.new_tab(window, cx);
            }))
            .child(Icon::Plus.element(colors.text_secondary))
            .child(gpui::div().flex_1().child("New tab"))
            .child(
                gpui::div()
                    .flex_none()
                    .font_family(self.mono_font_family())
                    .text_size(text::CAPTION)
                    .text_color(colors.text_muted)
                    .child(engine::app_chord_label("t")),
            );
        // Whatever holds the stack open other than the pointer shows it whole at once.
        let reveal = if self.tab_hints_visible || self.tab_rename.is_some() || cx.has_active_drag()
        {
            1.0
        } else {
            self.hover_level(HoverTarget::TabStack)
        };
        tokens::elevated(gpui::div(), Elevation::Floating, &colors)
            .id("cuetty-tab-stack")
            // Keeps clicks from falling through to start a selection in the terminal.
            .occlude()
            .w(size::TAB_STACK_WIDTH)
            .max_h_full()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .opacity(reveal)
            .font_family(tokens::ui_font_family())
            .child(tabs)
            .child(new_tab)
    }

    /// The square-ish hit target every chrome control shares.
    pub(super) fn control(
        &self,
        button: SidebarButton,
        colors: &ChromeColors,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let target = HoverTarget::Button(button);
        gpui::div()
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
            .bg(tokens::faded(colors.state, self.hover_level(target)))
            .on_hover(hover::hover_fade(target, cx))
            .child(hover::hover_probe(target, cx))
    }
}

/// A tab's status at a glance. Every state has its own shape, so it reads the same
/// whatever colours the theme gives it.
fn render_indicator(indicator: TabIndicator, colors: &ChromeColors) -> Div {
    let slot = gpui::div()
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
            gpui::div()
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
        gpui::div()
            .flex_none()
            .w(size::CARET_WIDTH)
            .h(size::CARET_HEIGHT)
            .bg(colors.accent)
    };
    gpui::div()
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
            gpui::div()
                .min_w_0()
                .flex()
                .justify_end()
                .overflow_hidden()
                .child(
                    gpui::div()
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
    gpui::div()
        .id(("cuetty-rail-tab-close", id.get()))
        .flex()
        .items_center()
        .justify_center()
        .rounded(radius::CONTROL)
        .opacity(reveal)
        // The row beneath is already filled with `state` while the pointer is on it.
        .bg(tokens::faded(colors.state_on_row, hover))
        .on_hover(hover::hover_fade(HoverTarget::TabClose(id), cx))
        .child(hover::hover_probe(HoverTarget::TabClose(id), cx))
        .cursor_pointer()
        .tooltip(super::tooltip::chrome_tooltip(
            format!("Close tab · {}", engine::app_chord_label("w")),
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
        tooltip,
        indicator,
        number,
        selected,
        hover,
        close_hover,
        active,
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
    gpui::div()
        .id(("cuetty-rail-tab", id.get()))
        .relative()
        .w_full()
        .h(size::TAB_ROW_TWO_LINE_HEIGHT)
        .flex_shrink_0()
        .flex()
        .items_center()
        .rounded(radius::CONTROL)
        .bg(tokens::faded(colors.state, hover.max(active)))
        .on_hover(hover::hover_fade(HoverTarget::Tab(id), cx))
        .child(hover::hover_probe(HoverTarget::Tab(id), cx))
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
        .tooltip(super::tooltip::chrome_tooltip(tooltip, colors))
        .text_size(text::BODY)
        .text_color(text_color)
        .font_weight(if selected {
            text::MEDIUM
        } else {
            text::REGULAR
        })
        .gap(space::MD)
        .px(space::LG)
        .child(
            gpui::div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .line_height(text::LINE_HEIGHT)
                .child(match editing {
                    Some(editing) => render_name_field(editing, label, &colors),
                    None => gpui::div().truncate().child(label),
                })
                .when(!subtitle.is_empty(), |this| {
                    this.child(
                        gpui::div()
                            .truncate()
                            .text_size(text::CAPTION)
                            .text_color(subtitle_color)
                            .font_weight(text::REGULAR)
                            .child(subtitle),
                    )
                }),
        )
        .children(number.map(|number| {
            gpui::div()
                .flex_none()
                .font_family(mono.clone())
                .text_size(text::CAPTION)
                .text_color(colors.text_muted)
                .font_weight(text::REGULAR)
                .child(engine::tab_number_hint(number))
        }))
        .child(
            // The button takes over from the indicator while the row is hovered.
            gpui::div()
                .relative()
                .size(size::TAB_ACTION)
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .children(
                    indicator
                        .map(|indicator| render_indicator(indicator, &colors).opacity(1.0 - hover)),
                )
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
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::config::TerminalConfig;

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
