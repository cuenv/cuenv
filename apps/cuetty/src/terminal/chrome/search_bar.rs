//! The bar that shows a search of the screen is open and where it has got to.

use super::icons::Icon;
use super::tokens::{ChromeColors, Elevation, radius, size, space, text};
use super::{field, tokens};

use super::hover;
use crate::terminal::engine::TerminalView;
use crate::terminal::interaction::SearchKey;
use crate::terminal::motion::{HoverTarget, SearchButton};
use crate::terminal::presentation::TerminalTheme;
use crate::terminal::search::LiteralSearch;
use gpui::{
    Context, Div, InteractiveElement, IntoElement, ParentElement, Stateful,
    StatefulInteractiveElement, Styled,
};

/// Says how a search is going, with nothing to say before anything has been typed.
fn count_label(search: &LiteralSearch) -> Option<String> {
    if search.query().is_empty() {
        return None;
    }
    let total = search.matches().len();
    Some(match (search.current_index(), total) {
        (_, 0) => "No matches".into(),
        (Some(index), total) => format!("{} of {total}", index + 1),
        (None, 1) => "1 match".into(),
        (None, total) => format!("{total} matches"),
    })
}

impl SearchButton {
    const fn element_id(self) -> &'static str {
        match self {
            Self::Previous => "cuetty-search-previous",
            Self::Next => "cuetty-search-next",
            Self::Close => "cuetty-search-close",
        }
    }

    const fn icon(self) -> Icon {
        match self {
            Self::Previous => Icon::ChevronUp,
            Self::Next => Icon::ChevronDown,
            Self::Close => Icon::Close,
        }
    }

    const fn tooltip(self) -> &'static str {
        match self {
            Self::Previous => "Previous match · Shift+Enter",
            Self::Next => "Next match · Enter",
            Self::Close => "Close · Esc",
        }
    }

    const fn key(self) -> SearchKey {
        match self {
            Self::Previous => SearchKey::Enter { reverse: true },
            Self::Next => SearchKey::Enter { reverse: false },
            Self::Close => SearchKey::Escape,
        }
    }
}

impl TerminalView {
    /// Floats at the terminal's top right and gives up width in a narrow terminal.
    pub(in crate::terminal) fn render_search_bar(
        &self,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let search = self.active_state()?.search()?;
        let colors = ChromeColors::from_theme(theme);
        let bar = tokens::elevated(gpui::div(), Elevation::Floating, &colors)
            // The bar covers the terminal, so pointer input stops here instead of
            // selecting text beneath it.
            .occlude()
            .w_full()
            .max_w(size::SEARCH_BAR_WIDTH)
            .p(space::XS)
            .flex()
            .items_center()
            .gap(space::XS)
            .line_height(text::LINE_HEIGHT)
            .child(
                field::text_field(search.query(), "Find on screen", &colors)
                    .flex_1()
                    .min_w_0()
                    .h(size::CONTROL_HEIGHT)
                    .px(space::MD)
                    .text_size(text::BODY),
            )
            .children(count_label(search).map(|count| {
                gpui::div()
                    .flex_none()
                    .px(space::XS)
                    .text_size(text::CAPTION)
                    .text_color(colors.text_muted)
                    .child(count)
            }))
            .child(self.search_button(SearchButton::Previous, &colors, cx))
            .child(self.search_button(SearchButton::Next, &colors, cx))
            .child(self.search_button(SearchButton::Close, &colors, cx));
        Some(
            // Both edges are set so a narrow terminal limits the bar, which keeps to the right.
            gpui::div()
                .absolute()
                .top(space::MD)
                .left(space::MD)
                .right(space::MD)
                .flex()
                .justify_end()
                .child(hover::fading_in(
                    bar,
                    "cuetty-search-fade",
                    self.motion_pace(),
                ))
                .into_any_element(),
        )
    }

    fn search_button(
        &self,
        button: SearchButton,
        colors: &ChromeColors,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let target = HoverTarget::SearchButton(button);
        gpui::div()
            .id(button.element_id())
            .flex_none()
            .size(size::SEARCH_BUTTON)
            .flex()
            .items_center()
            .justify_center()
            .rounded(radius::CONTROL)
            .bg(tokens::faded(colors.panel_hover, self.hover_level(target)))
            .on_hover(hover::hover_fade(target, cx))
            .child(hover::hover_probe(target, cx))
            .cursor_pointer()
            .tooltip(super::tooltip::chrome_tooltip(button.tooltip(), *colors))
            .on_click(cx.listener(move |view, _event, window, cx| {
                view.search_key(button.key());
                view.focus_terminal(window);
                cx.notify();
            }))
            .child(
                button
                    .icon()
                    .element(colors.text_secondary)
                    .size(size::STATUS_ICON),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::model::TerminalCell;
    use crate::terminal::search::SearchOptions;
    use crate::terminal::selection::TextSource;

    struct Line(Vec<TerminalCell>);

    impl TextSource for Line {
        fn line_count(&self) -> usize {
            1
        }
        fn line_cells(&self, _: usize) -> &[TerminalCell] {
            &self.0
        }
        fn line_soft_wrapped(&self, _: usize) -> bool {
            false
        }
    }

    fn searched(query: &str, text: &str) -> LiteralSearch {
        let line = Line(text.chars().map(TerminalCell::narrow).collect());
        let mut search = LiteralSearch::new(query, SearchOptions::default());
        search.rebuild(&line).unwrap();
        search
    }

    #[test]
    fn the_count_says_nothing_until_something_has_been_typed() {
        assert_eq!(count_label(&searched("", "one one")), None);
    }

    #[test]
    fn the_count_says_when_nothing_matches() {
        assert_eq!(
            count_label(&searched("two", "one one")).as_deref(),
            Some("No matches")
        );
    }

    #[test]
    fn the_count_reads_naturally_before_navigation_has_begun() {
        assert_eq!(
            count_label(&searched("on", "one")).as_deref(),
            Some("1 match")
        );
        assert_eq!(
            count_label(&searched("on", "one one")).as_deref(),
            Some("2 matches")
        );
    }

    #[test]
    fn the_count_says_which_match_navigation_is_on() {
        let mut search = searched("on", "one one one");
        search.next_match();
        search.next_match();
        assert_eq!(count_label(&search).as_deref(), Some("2 of 3"));
    }
}
