//! What stands in for the terminal when a tab has none to show, and the way out.

use super::clamp::TextBudget;
use super::icons::Icon;
use super::tokens::{ChromeColors, border, faded, radius, size, space, text};
use super::{hover_fade, hover_probe};
use crate::terminal::engine::TerminalView;
use crate::terminal::motion::HoverTarget;
use crate::terminal::presentation::TerminalTheme;
use gpui::{
    Context, Div, InteractiveElement, IntoElement, ParentElement, Stateful,
    StatefulInteractiveElement, Styled, div,
};

/// How much of a problem's detail is shown. What a session reports is not ours and can run
/// to any number of lines, which must never push the way out off the screen.
const DETAIL_BUDGET: TextBudget = TextBudget {
    lines: text::SESSION_STATE_DETAIL_LINES,
    line_chars: text::SESSION_STATE_DETAIL_LINE_CHARS,
};

/// Why there is no terminal to show.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::terminal) enum SessionProblem<'a> {
    /// The window has no tabs. A failed attempt to start a shell says why here.
    NoSession { notice: Option<&'a str> },
    /// The active tab's session has reported an error.
    Failed { error: &'a str },
    /// The active tab has no session at all, which nothing should ever leave behind.
    Lost,
}

/// A way out of a [`SessionProblem`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Recovery {
    NewTab,
    RestartShell,
    CloseTab,
}

impl Recovery {
    const fn label(self) -> &'static str {
        match self {
            Self::NewTab => "New tab",
            Self::RestartShell => "Restart shell",
            Self::CloseTab => "Close tab",
        }
    }

    fn run(
        self,
        view: &mut TerminalView,
        window: &mut gpui::Window,
        cx: &mut Context<TerminalView>,
    ) {
        match self {
            Self::NewTab => view.new_tab(window, cx),
            Self::RestartShell => view.restart_shell(window, cx),
            Self::CloseTab => view.close_active_tab(window, cx),
        }
    }
}

/// One way out, and how it is drawn.
struct RecoveryButton {
    recovery: Recovery,
    /// Its place among those on offer.
    place: usize,
    colors: ChromeColors,
}

/// What a problem says to the user. Enter takes the first recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProblemCopy<'a> {
    alarm: Alarm,
    title: &'static str,
    detail: &'a str,
    recoveries: &'static [Recovery],
}

/// Whether a problem is something going wrong, or only nothing being open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Alarm {
    Quiet,
    Raised,
}

fn problem_copy(problem: SessionProblem<'_>) -> ProblemCopy<'_> {
    match problem {
        SessionProblem::NoSession { notice } => ProblemCopy {
            alarm: notice.map_or(Alarm::Quiet, |_| Alarm::Raised),
            title: "No terminal open",
            detail: notice.unwrap_or("Open a tab to start a shell."),
            recoveries: &[Recovery::NewTab],
        },
        SessionProblem::Failed { error } => ProblemCopy {
            alarm: Alarm::Raised,
            title: "This terminal stopped working",
            detail: error,
            recoveries: &[Recovery::RestartShell, Recovery::CloseTab],
        },
        SessionProblem::Lost => ProblemCopy {
            alarm: Alarm::Raised,
            title: "This tab lost its terminal",
            detail: "Its session has gone. A new tab starts a fresh shell.",
            recoveries: &[Recovery::NewTab],
        },
    }
}

impl TerminalView {
    /// The state a tab shows in place of its terminal. It takes the keyboard the terminal
    /// would have had, so Enter and the application's chords keep working here.
    pub(in crate::terminal) fn render_session_problem(
        &self,
        problem: SessionProblem<'_>,
        theme: &TerminalTheme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let colors = ChromeColors::from_theme(theme);
        let copy = problem_copy(problem);
        let buttons = copy
            .recoveries
            .iter()
            .enumerate()
            .map(|(place, recovery)| {
                self.recovery_button(
                    RecoveryButton {
                        recovery: *recovery,
                        place,
                        colors,
                    },
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let state = self
            .focused_for_keys(div(), cx)
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(space::LG)
            .p(space::XL)
            .overflow_hidden()
            .line_height(text::LINE_HEIGHT)
            .children((copy.alarm == Alarm::Raised).then(|| Icon::Failed.element(colors.danger)))
            .child(
                div()
                    .text_size(text::TITLE)
                    .font_weight(text::STRONG)
                    .text_color(colors.text)
                    .child(copy.title),
            )
            .child(
                div()
                    .max_w(size::SESSION_STATE_WIDTH)
                    .min_h_0()
                    .overflow_hidden()
                    .line_clamp(text::SESSION_STATE_DETAIL_LINES)
                    .text_size(text::BODY)
                    .text_color(colors.text_secondary)
                    .text_center()
                    .child(DETAIL_BUDGET.clamp(copy.detail)),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(space::MD)
                    .children(buttons),
            );
        // The window moves by the same band as it does above a terminal.
        div()
            .relative()
            .size_full()
            .child(state)
            .children(self.window_controls_band(self.top_padding()))
            .into_any_element()
    }

    /// The first recovery on offer is what Enter takes, which it says, and it alone has a
    /// fill: one obvious way out, with the alternative beside it.
    fn recovery_button(&self, button: RecoveryButton, cx: &mut Context<Self>) -> Stateful<Div> {
        let RecoveryButton {
            recovery,
            place,
            colors,
        } = button;
        let target = HoverTarget::SessionAction(place);
        let primary = place == 0;
        let clear = gpui::transparent_black().to_rgb();
        div()
            .id(("cuetty-session-action", place))
            .relative()
            .h(size::CONTROL_HEIGHT)
            .px(space::LG)
            .flex()
            .items_center()
            .gap(space::MD)
            .rounded(radius::CONTROL)
            .border(border::HAIRLINE)
            .border_color(if primary { colors.panel_outline } else { clear })
            .bg(if primary { colors.panel } else { clear })
            .text_size(text::LABEL)
            .font_weight(text::MEDIUM)
            .text_color(if primary {
                colors.text
            } else {
                colors.text_secondary
            })
            .cursor_pointer()
            .on_hover(hover_fade(target, cx))
            .child(hover_probe(target, cx))
            .on_click(cx.listener(move |view, _event, window, cx| {
                recovery.run(view, window, cx);
                cx.notify();
            }))
            // Underneath the label, which keeps its colours as the hover fades in.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(radius::CONTROL)
                    .bg(faded(colors.panel_hover, self.hover_level(target))),
            )
            .child(recovery.label())
            .children(primary.then(|| {
                div()
                    .text_size(text::CAPTION)
                    .font_weight(text::REGULAR)
                    .text_color(colors.text_muted)
                    .child("Enter")
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_window_offers_a_tab_without_alarm_until_starting_one_has_failed() {
        let quiet = problem_copy(SessionProblem::NoSession { notice: None });
        assert_eq!(quiet.alarm, Alarm::Quiet);
        assert_eq!(quiet.recoveries, [Recovery::NewTab]);

        let failed = problem_copy(SessionProblem::NoSession {
            notice: Some("no pty"),
        });
        assert_eq!(failed.alarm, Alarm::Raised);
        assert_eq!(failed.detail, "no pty");
        assert_eq!(failed.recoveries, [Recovery::NewTab]);
    }

    #[test]
    fn a_failed_terminal_says_why_and_leads_with_restarting_it() {
        let copy = problem_copy(SessionProblem::Failed { error: "pipe gone" });
        assert_eq!(copy.detail, "pipe gone");
        assert_eq!(copy.recoveries.first(), Some(&Recovery::RestartShell));
        assert!(copy.recoveries.contains(&Recovery::CloseTab));
    }

    #[test]
    fn a_long_error_is_cut_to_the_room_its_way_out_leaves() {
        let error = (0..40)
            .map(|line| format!("spawn failed at step {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let shown = DETAIL_BUDGET.clamp(&error);
        assert_eq!(shown.lines().count(), text::SESSION_STATE_DETAIL_LINES);
        assert_eq!(shown.lines().last(), Some("…"));
    }

    #[test]
    fn a_tab_with_no_session_cannot_restart_what_is_not_there() {
        let copy = problem_copy(SessionProblem::Lost);
        assert!(!copy.recoveries.contains(&Recovery::RestartShell));
        assert!(!copy.recoveries.is_empty());
    }
}
