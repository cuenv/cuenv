//! What stands in for the terminal when a tab has none to show, and the way out.

use super::clamp::TextBudget;
use super::icons::Icon;
use super::tokens::{ChromeColors, border, radius, size, space, text};
use super::{hover, tokens};
use crate::terminal::engine::TerminalView;
use crate::terminal::motion::HoverTarget;
use crate::terminal::presentation::TerminalTheme;
use gpui::{
    Context, Div, InteractiveElement, IntoElement, ParentElement, Stateful,
    StatefulInteractiveElement, Styled,
};

/// How much of a problem's detail is shown. What a session reports is not ours and can run
/// to any number of lines, which must never push the way out off the screen.
const DETAIL_BUDGET: TextBudget = TextBudget {
    lines: text::SESSION_STATE_DETAIL_LINES,
    line_chars: text::SESSION_STATE_DETAIL_LINE_CHARS,
};

/// Why there is no terminal to show.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::terminal) enum SessionProblem<'detail> {
    /// The window has no tabs. A failed attempt to start a shell says why here.
    NoSession { notice: Option<&'detail str> },
    /// The active tab's session has reported an error.
    Failed { error: &'detail str },
    /// A tab's measured geometry cannot be used safely.
    PaneGeometry,
    /// The window's shared workspace geometry cannot be used safely.
    WorkspaceGeometry,
    /// The active tab has no session at all, which nothing should ever leave behind.
    Lost,
}

/// A way out of a [`SessionProblem`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Recovery {
    NewTab,
    RestartShell,
    CloseTab,
    CloseWindow,
}

impl Recovery {
    const fn label(self) -> &'static str {
        match self {
            Self::NewTab => "New tab",
            Self::RestartShell => "Restart shell",
            Self::CloseTab => "Close tab",
            Self::CloseWindow => "Close window",
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
            Self::CloseWindow => window.remove_window(),
        }
    }
}

/// One way out, and how it is drawn.
struct RecoveryButton {
    recovery: Recovery,
    /// Its place among those on offer.
    place: usize,
    enter_key: bool,
    colors: ChromeColors,
}

/// What a problem says to the user and whether Enter takes its first recovery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProblemCopy<'message> {
    alarm: Alarm,
    title: &'static str,
    detail: &'message str,
    recoveries: &'static [Recovery],
    enter_key: bool,
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
            enter_key: true,
        },
        SessionProblem::Failed { error } => ProblemCopy {
            alarm: Alarm::Raised,
            title: "This terminal stopped working",
            detail: error,
            recoveries: &[Recovery::RestartShell, Recovery::CloseTab],
            enter_key: true,
        },
        SessionProblem::PaneGeometry => ProblemCopy {
            alarm: Alarm::Raised,
            title: "This terminal cannot be displayed",
            detail: "Close this tab. If it is the last tab, reopen the window.",
            recoveries: &[Recovery::CloseTab],
            enter_key: false,
        },
        SessionProblem::WorkspaceGeometry => ProblemCopy {
            alarm: Alarm::Raised,
            title: "This window cannot be displayed",
            detail: "Close and reopen the window to continue.",
            recoveries: &[Recovery::CloseWindow],
            enter_key: false,
        },
        SessionProblem::Lost => ProblemCopy {
            alarm: Alarm::Raised,
            title: "This tab lost its terminal",
            detail: "Its session has gone. A new tab starts a fresh shell.",
            recoveries: &[Recovery::NewTab],
            enter_key: true,
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
                        enter_key: copy.enter_key,
                        colors,
                    },
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let state = self
            .focused_for_keys(gpui::div(), cx)
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
                gpui::div()
                    .text_size(text::TITLE)
                    .font_weight(text::STRONG)
                    .text_color(colors.text)
                    .child(copy.title),
            )
            .child(
                gpui::div()
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
                gpui::div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(space::MD)
                    .children(buttons),
            );
        // The window moves by the same band as it does above a terminal.
        gpui::div()
            .relative()
            .size_full()
            .child(state)
            .children(self.window_controls_band(self.top_padding()))
            .into_any_element()
    }

    /// The first recovery is emphasized. When Enter takes it, the button says so.
    fn recovery_button(&self, button: RecoveryButton, cx: &mut Context<Self>) -> Stateful<Div> {
        let RecoveryButton {
            recovery,
            place,
            enter_key,
            colors,
        } = button;
        let target = HoverTarget::SessionAction(place);
        let primary = place == 0;
        let clear = gpui::transparent_black().to_rgb();
        gpui::div()
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
            .on_hover(hover::hover_fade(target, cx))
            .child(hover::hover_probe(target, cx))
            .on_click(cx.listener(move |view, _event, window, cx| {
                recovery.run(view, window, cx);
                cx.notify();
            }))
            // Underneath the label, which keeps its colours as the hover fades in.
            .child(
                gpui::div()
                    .absolute()
                    .inset_0()
                    .rounded(radius::CONTROL)
                    .bg(tokens::faded(colors.panel_hover, self.hover_level(target))),
            )
            .child(recovery.label())
            .children((primary && enter_key).then(|| {
                gpui::div()
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
        assert!(copy.enter_key);
    }

    #[test]
    fn unavailable_geometry_offers_only_a_working_explicit_close_action() {
        let pane = problem_copy(SessionProblem::PaneGeometry);
        assert_eq!(pane.recoveries, [Recovery::CloseTab]);
        assert!(!pane.enter_key);

        let window = problem_copy(SessionProblem::WorkspaceGeometry);
        assert_eq!(window.recoveries, [Recovery::CloseWindow]);
        assert!(!window.enter_key);
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
