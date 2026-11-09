//! Helpers referenced by the exported event macros.

use crate::event::{RestartReason, Stream, TaskKind};

/// Map the string `task_kind` discriminator the old tracing macros
/// accepted to the typed [`TaskKind`] used by the direct emit path.
/// Unknown values default to [`TaskKind::Task`].
#[must_use]
pub fn parse_task_kind(kind: &str) -> TaskKind {
    match kind {
        "group" => TaskKind::Group,
        "sequence" => TaskKind::Sequence,
        _ => TaskKind::Task,
    }
}

/// Map the string `stream` discriminator to the typed [`Stream`] enum.
/// Unknown values fall back to [`Stream::Stdout`].
#[must_use]
pub fn parse_stream(stream: &str) -> Stream {
    match stream {
        "stderr" => Stream::Stderr,
        _ => Stream::Stdout,
    }
}

/// Map a free-form `restart_reason` string to a typed
/// [`RestartReason`]. Unknown values default to `Manual`.
#[must_use]
pub fn parse_restart_reason(reason: &(impl ::std::fmt::Display + ?Sized)) -> RestartReason {
    match reason.to_string().as_str() {
        "crashed" | "Crashed" => RestartReason::Crashed,
        "watch_triggered" | "WatchTriggered" => RestartReason::WatchTriggered,
        _ => RestartReason::Manual,
    }
}
