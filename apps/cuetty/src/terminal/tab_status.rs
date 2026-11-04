//! What a tab has to report beside its name. Everything here is pure, so what each
//! event does to a tab can be tested without a window or a terminal session.

/// Whether a tab's terminal is the one on screen. A bell or new output in a tab being
/// looked at has already been seen, so only a hidden tab has anything to report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TabPresence {
    Shown,
    Hidden,
}

/// How a tab's shell ended, read from its raw wait status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SessionExit {
    Clean,
    Failed(ExitFailure),
    /// The platform did not say, which is no reason to keep a tab around.
    Unknown,
}

impl SessionExit {
    pub(super) fn from_wait_status(status: Option<i32>) -> Self {
        let Some(status) = status else {
            return Self::Unknown;
        };
        ExitFailure::from_wait_status(status).map_or(Self::Clean, Self::Failed)
    }
}

/// What went wrong with a shell that did not exit cleanly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExitFailure {
    Code(i32),
    Signal(i32),
}

impl ExitFailure {
    #[cfg(unix)]
    fn from_wait_status(status: i32) -> Option<Self> {
        use std::os::unix::process::ExitStatusExt;
        let status = std::process::ExitStatus::from_raw(status);
        match (status.code(), status.signal()) {
            (Some(0), _) => None,
            (Some(code), _) => Some(Self::Code(code)),
            (None, Some(signal)) => Some(Self::Signal(signal)),
            (None, None) => None,
        }
    }

    #[cfg(not(unix))]
    fn from_wait_status(status: i32) -> Option<Self> {
        (status != 0).then_some(Self::Code(status))
    }

    /// What happened, in a sentence fragment for the user.
    pub(super) fn summary(self) -> String {
        match self {
            Self::Code(code) => format!("Shell exited with status {code}"),
            Self::Signal(signal) => format!("Shell ended by signal {signal}"),
        }
    }
}

/// How far the task launched in a tab has got.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum TaskPhase {
    #[default]
    Idle,
    Running,
    /// The task handed the foreground back. It stays reported until seen.
    Finished,
}

/// Something that happened to a tab.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StatusEvent {
    /// A task launched from Cuetty took over the tab.
    TaskStarted,
    /// That task handed the foreground back to the shell.
    TaskEnded,
    Bell(TabPresence),
    /// The terminal's visible text changed.
    Output(TabPresence),
    /// The user arrived at this tab.
    Seen,
    /// The user typed into this tab.
    Input,
    /// The tab's shell ended badly, which outlasts any visit to the tab.
    SessionFailed(ExitFailure),
}

/// What a tab's indicator shows. Each has its own shape and description, so colour is
/// never the only way to tell them apart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TabIndicator {
    Failed,
    Bell,
    Running,
    Finished,
    Unread,
}

impl TabIndicator {
    /// What this indicator means, in the words its tooltip uses.
    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::Failed => "Shell failed",
            Self::Bell => "Rang the bell",
            Self::Running => "Task running",
            Self::Finished => "Task finished",
            Self::Unread => "New output",
        }
    }
}

/// What has happened in a tab that its indicator has to say.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct TabStatus {
    task: TaskPhase,
    bell: bool,
    unread: bool,
    failure: Option<ExitFailure>,
}

impl TabStatus {
    pub(super) fn apply(&mut self, event: StatusEvent) {
        match event {
            StatusEvent::TaskStarted => self.task = TaskPhase::Running,
            StatusEvent::TaskEnded if self.task == TaskPhase::Running => {
                self.task = TaskPhase::Finished;
            }
            StatusEvent::TaskEnded => {}
            StatusEvent::Bell(TabPresence::Hidden) => self.bell = true,
            StatusEvent::Output(TabPresence::Hidden) => self.unread = true,
            StatusEvent::Bell(TabPresence::Shown) | StatusEvent::Output(TabPresence::Shown) => {}
            StatusEvent::Seen => {
                self.bell = false;
                self.unread = false;
                self.clear_finished();
            }
            StatusEvent::Input => self.clear_finished(),
            StatusEvent::SessionFailed(failure) => {
                self.failure = Some(failure);
                self.task = TaskPhase::Idle;
            }
        }
    }

    fn clear_finished(&mut self) {
        if self.task == TaskPhase::Finished {
            self.task = TaskPhase::Idle;
        }
    }

    /// How the tab's shell failed, if it did. Such a tab stays open with its final
    /// output on screen until the user closes it.
    pub(super) const fn failure(self) -> Option<ExitFailure> {
        self.failure
    }

    /// What the tab shows when several things are true at once: what went wrong, then
    /// what asks for the user, then what is still going on, then what merely happened.
    pub(super) fn indicator(self) -> Option<TabIndicator> {
        if self.failure.is_some() {
            return Some(TabIndicator::Failed);
        }
        if self.bell {
            return Some(TabIndicator::Bell);
        }
        match self.task {
            TaskPhase::Running => Some(TabIndicator::Running),
            TaskPhase::Finished => Some(TabIndicator::Finished),
            TaskPhase::Idle => self.unread.then_some(TabIndicator::Unread),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(events: &[StatusEvent]) -> TabStatus {
        let mut status = TabStatus::default();
        events.iter().for_each(|event| status.apply(*event));
        status
    }

    #[test]
    fn a_tab_with_nothing_to_report_shows_nothing() {
        assert_eq!(TabStatus::default().indicator(), None);
    }

    #[test]
    fn only_a_hidden_tab_collects_a_bell_or_output() {
        for event in [
            StatusEvent::Bell(TabPresence::Shown),
            StatusEvent::Output(TabPresence::Shown),
        ] {
            assert_eq!(after(&[event]).indicator(), None);
        }
        assert_eq!(
            after(&[StatusEvent::Bell(TabPresence::Hidden)]).indicator(),
            Some(TabIndicator::Bell)
        );
        assert_eq!(
            after(&[StatusEvent::Output(TabPresence::Hidden)]).indicator(),
            Some(TabIndicator::Unread)
        );
    }

    #[test]
    fn arriving_at_a_tab_clears_what_was_waiting_for_the_user() {
        for waiting in [
            StatusEvent::Bell(TabPresence::Hidden),
            StatusEvent::Output(TabPresence::Hidden),
            StatusEvent::TaskEnded,
        ] {
            let status = after(&[StatusEvent::TaskStarted, waiting, StatusEvent::TaskEnded]);
            assert!(status.indicator().is_some());
            let mut seen = status;
            seen.apply(StatusEvent::Seen);
            assert_eq!(seen.indicator(), None, "after {waiting:?}");
        }
    }

    #[test]
    fn arriving_at_a_tab_does_not_stop_its_running_task_being_reported() {
        let status = after(&[
            StatusEvent::TaskStarted,
            StatusEvent::Output(TabPresence::Hidden),
            StatusEvent::Seen,
        ]);
        assert_eq!(status.indicator(), Some(TabIndicator::Running));
    }

    #[test]
    fn a_task_runs_until_it_ends_and_then_reports_finishing() {
        let started = after(&[StatusEvent::TaskStarted]);
        assert_eq!(started.indicator(), Some(TabIndicator::Running));
        let ended = after(&[StatusEvent::TaskStarted, StatusEvent::TaskEnded]);
        assert_eq!(ended.indicator(), Some(TabIndicator::Finished));
    }

    #[test]
    fn nothing_finishes_without_having_started() {
        assert_eq!(after(&[StatusEvent::TaskEnded]).indicator(), None);
    }

    #[test]
    fn typing_moves_on_from_a_finished_task_but_not_a_running_one() {
        let finished = after(&[
            StatusEvent::TaskStarted,
            StatusEvent::TaskEnded,
            StatusEvent::Input,
        ]);
        assert_eq!(finished.indicator(), None);
        let running = after(&[StatusEvent::TaskStarted, StatusEvent::Input]);
        assert_eq!(running.indicator(), Some(TabIndicator::Running));
    }

    #[test]
    fn a_new_task_replaces_a_finished_one() {
        let status = after(&[
            StatusEvent::TaskStarted,
            StatusEvent::TaskEnded,
            StatusEvent::TaskStarted,
        ]);
        assert_eq!(status.indicator(), Some(TabIndicator::Running));
    }

    #[test]
    fn only_a_failure_keeps_a_session_exit_from_passing_unremarked() {
        assert_eq!(SessionExit::from_wait_status(None), SessionExit::Unknown);
        assert_eq!(SessionExit::from_wait_status(Some(0)), SessionExit::Clean);
    }

    #[cfg(unix)]
    #[test]
    fn a_wait_status_is_read_as_an_exit_code_or_a_signal() {
        // A process's exit code sits in the high byte of its wait status.
        assert_eq!(
            SessionExit::from_wait_status(Some(7 << 8)),
            SessionExit::Failed(ExitFailure::Code(7))
        );
        assert_eq!(
            SessionExit::from_wait_status(Some(9)),
            SessionExit::Failed(ExitFailure::Signal(9))
        );
    }

    #[test]
    fn a_failed_session_outranks_everything_and_outlasts_a_visit() {
        let failure = ExitFailure::Code(1);
        let mut status = after(&[
            StatusEvent::TaskStarted,
            StatusEvent::Bell(TabPresence::Hidden),
            StatusEvent::SessionFailed(failure),
        ]);
        assert_eq!(status.indicator(), Some(TabIndicator::Failed));
        status.apply(StatusEvent::Seen);
        assert_eq!(status.indicator(), Some(TabIndicator::Failed));
        assert_eq!(status.failure(), Some(failure));
    }

    #[test]
    fn a_task_cannot_finish_in_a_shell_that_failed_under_it() {
        let status = after(&[
            StatusEvent::TaskStarted,
            StatusEvent::SessionFailed(ExitFailure::Signal(9)),
            StatusEvent::TaskEnded,
            StatusEvent::Seen,
        ]);
        assert_eq!(status.indicator(), Some(TabIndicator::Failed));
    }

    #[test]
    fn a_bell_outranks_a_task_and_a_task_outranks_unread_output() {
        let bell = after(&[
            StatusEvent::TaskStarted,
            StatusEvent::Output(TabPresence::Hidden),
            StatusEvent::Bell(TabPresence::Hidden),
        ]);
        assert_eq!(bell.indicator(), Some(TabIndicator::Bell));
        let running = after(&[
            StatusEvent::Output(TabPresence::Hidden),
            StatusEvent::TaskStarted,
        ]);
        assert_eq!(running.indicator(), Some(TabIndicator::Running));
        let finished = after(&[
            StatusEvent::TaskStarted,
            StatusEvent::Output(TabPresence::Hidden),
            StatusEvent::TaskEnded,
        ]);
        assert_eq!(finished.indicator(), Some(TabIndicator::Finished));
    }
}
