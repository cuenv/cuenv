use super::tokens::{
    ChromeColors, Elevation, elevated, size, space, text, tooltip_max_height, ui_font_family,
};
use gpui::{
    AnyView, App, AppContext, Context, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, prelude::FluentBuilder,
};

/// GPUI lays tooltips out at their min-content size, where text never wraps, so
/// a long label only wraps once the tooltip has an explicit width.
fn tooltip_needs_wrapping(label: &str) -> bool {
    label.contains('\n') || label.chars().count() > text::TOOLTIP_LINE_CHARS
}

/// Lines `line` takes up once wrapped. An estimate from its length: wrapping
/// happens on word boundaries in a proportional font.
fn tooltip_line_cost(line: &str) -> usize {
    line.chars()
        .count()
        .div_ceil(text::TOOLTIP_LINE_CHARS)
        .max(1)
}

/// Cuts `label` to the tooltip's line limit, ending in an ellipsis when it has to
/// cut. GPUI's `line_clamp` only limits soft wraps, so each newline in a message
/// would otherwise add a line it never counts.
fn clamped_tooltip_label(label: &str) -> String {
    let label = label.trim_end();
    let lines: usize = label.lines().map(tooltip_line_cost).sum();
    if lines <= text::TOOLTIP_MAX_LINES {
        return label.to_string();
    }
    label
        .lines()
        .scan(text::TOOLTIP_MAX_LINES - 1, |remaining, line| {
            let kept = tooltip_line_cost(line).min(*remaining);
            *remaining -= kept;
            (kept > 0).then(|| {
                line.chars()
                    .take(kept * text::TOOLTIP_LINE_CHARS)
                    .collect::<String>()
            })
        })
        .chain(std::iter::once("…".to_string()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A tooltip painted from the chrome tokens. GPUI draws tooltips as their own
/// view, so this one carries the colours it was built with.
struct ChromeTooltip {
    label: SharedString,
    colors: ChromeColors,
}

impl Render for ChromeTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // The outer element keeps the margin from being absorbed by the
        // tooltip's own positioned root.
        div().font_family(ui_font_family()).child(
            elevated(div(), Elevation::Raised, &self.colors)
                .m(space::MD)
                .px(space::MD)
                .py(space::XS)
                .when(tooltip_needs_wrapping(&self.label), |this| {
                    this.w(size::TOOLTIP_WRAP_WIDTH)
                })
                .line_height(text::LINE_HEIGHT)
                // The label is already cut to the limit by estimate. These hold
                // the limit when word wrapping runs over that estimate.
                .line_clamp(text::TOOLTIP_MAX_LINES)
                .max_h(tooltip_max_height())
                .text_size(text::LABEL)
                .text_color(self.colors.text)
                .child(self.label.clone()),
        )
    }
}

/// A builder for `.tooltip(...)` showing `label` in the chrome's own style.
pub(super) fn chrome_tooltip(
    label: impl Into<SharedString>,
    colors: ChromeColors,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let label = label.into();
    move |_window, cx| {
        let label = clamped_tooltip_label(&label).into();
        cx.new(|_cx| ChromeTooltip { label, colors }).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_long_or_multiline_labels_wrap() {
        assert!(!tooltip_needs_wrapping("New tab"));
        assert!(tooltip_needs_wrapping(
            &"x".repeat(text::TOOLTIP_LINE_CHARS + 1)
        ));
        assert!(tooltip_needs_wrapping("first\nsecond"));
    }

    fn visual_lines(label: &str) -> usize {
        label.lines().map(tooltip_line_cost).sum()
    }

    #[test]
    fn labels_within_the_limit_are_left_whole() {
        let at_limit = ["line"; text::TOOLTIP_MAX_LINES].join("\n");
        assert_eq!(clamped_tooltip_label(&at_limit), at_limit);
        assert_eq!(clamped_tooltip_label("New tab\n"), "New tab");
    }

    #[test]
    fn newlines_count_against_the_limit() {
        let failures = (0..40)
            .map(|index| format!("failure {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let clamped = clamped_tooltip_label(&failures);
        let lines = clamped.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), text::TOOLTIP_MAX_LINES);
        assert_eq!(lines.first(), Some(&"failure 0"));
        assert_eq!(lines.last(), Some(&"…"));
    }

    #[test]
    fn wrapped_lines_count_against_the_limit() {
        let unbroken = "é".repeat(text::TOOLTIP_LINE_CHARS * 100);
        let mixed = format!("{unbroken}\nnever reached");
        for label in [unbroken.as_str(), mixed.as_str()] {
            let clamped = clamped_tooltip_label(label);
            assert_eq!(visual_lines(&clamped), text::TOOLTIP_MAX_LINES);
            assert!(clamped.ends_with('…'));
            assert!(!clamped.contains("never reached"));
        }
    }

    #[test]
    fn a_line_crossing_the_limit_keeps_what_fits() {
        let early = ["line"; text::TOOLTIP_MAX_LINES - 3].join("\n");
        let long = "x".repeat(text::TOOLTIP_LINE_CHARS * 5);
        let clamped = clamped_tooltip_label(&format!("{early}\n{long}"));
        assert_eq!(visual_lines(&clamped), text::TOOLTIP_MAX_LINES);
        assert!(clamped.contains(&"x".repeat(text::TOOLTIP_LINE_CHARS * 2)));
    }
}
