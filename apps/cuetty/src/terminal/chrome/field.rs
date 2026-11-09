//! The one-line text field floating panels share.

use super::tokens::{ChromeColors, radius, size, space};
use gpui::prelude::FluentBuilder;
use gpui::{Div, ParentElement, Styled};

/// A field of text being typed. Text is only ever typed at its end, so the caret leads a
/// placeholder and follows a query, and a query too long for the field clips at its start
/// to keep the end being typed beside the caret.
pub(super) fn text_field(query: &str, placeholder: &str, colors: &ChromeColors) -> Div {
    let caret = || {
        gpui::div()
            .w(size::CARET_WIDTH)
            .h(size::CARET_HEIGHT)
            .flex_shrink_0()
            .bg(colors.accent)
    };
    let empty = query.is_empty();
    gpui::div()
        .flex()
        .items_center()
        .rounded(radius::CONTROL)
        .gap(space::XXS)
        .overflow_hidden()
        .bg(colors.well)
        .text_color(if empty {
            colors.text_muted
        } else {
            colors.text
        })
        .when(empty, |this| this.child(caret()))
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
                        .child(if empty { placeholder } else { query }.to_owned()),
                ),
        )
        .when(!empty, |this| this.child(caret()))
}
