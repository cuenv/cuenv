//! Delayed status for long-running preparation work.
//!
//! Work such as activating a Nix runtime can take anywhere from a few
//! milliseconds (cached) to many minutes (building a devShell). An
//! [`Activity`] stays silent while the work is fast and announces itself on
//! the event bus once it outlasts [`ANNOUNCE_DELAY`], so renderers can show a
//! spinner or a status line instead of a terminal that looks hung.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::time::Instant;

use crate::event::{ActivityEvent, ActivityOutcome, EventCategory};

/// How long work may run before its activity is announced.
pub const ANNOUNCE_DELAY: Duration = Duration::from_millis(200);

/// Longest progress detail kept, in characters.
const MAX_DETAIL_CHARS: usize = 200;

/// Nix store path prefix, shortened out of progress lines.
const NIX_STORE_PREFIX: &str = "/nix/store/";

/// Length of the hash part of a Nix store path.
const NIX_STORE_HASH_LEN: usize = 32;

/// A callback that receives each line a subprocess writes, as progress.
pub type ActivityLines = Arc<dyn Fn(&str) + Send + Sync>;

/// Long-running preparation work, announced only once it is slow.
#[derive(Debug, Clone)]
pub struct Activity {
    inner: Arc<ActivityInner>,
}

#[derive(Debug)]
struct ActivityInner {
    id: String,
    message: String,
    started: Instant,
    state: Mutex<ActivityState>,
    sink: ActivitySink,
}

/// Where activity events go.
#[derive(Debug)]
enum ActivitySink {
    /// The process-wide event bus.
    Bus,
    /// A test's own list, so tests do not race on the global bus.
    #[cfg(test)]
    Captured(Mutex<Vec<ActivityEvent>>),
}

#[derive(Debug, Default)]
struct ActivityState {
    announced: bool,
    latest: Option<String>,
}

impl Activity {
    /// Describe new work, such as `Activating Nix flake runtime (.)`.
    ///
    /// Nothing is shown until the work is tracked and outlasts
    /// [`ANNOUNCE_DELAY`].
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self::with_sink(message.into(), ActivitySink::Bus)
    }

    fn with_sink(message: String, sink: ActivitySink) -> Self {
        Self {
            inner: Arc::new(ActivityInner {
                id: uuid::Uuid::new_v4().to_string(),
                message,
                started: Instant::now(),
                state: Mutex::new(ActivityState::default()),
                sink,
            }),
        }
    }

    /// The message describing this activity.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.inner.message
    }

    /// Record a progress line, such as a line Nix wrote to stderr.
    ///
    /// Before the activity is announced only the latest line is kept, and
    /// it is shown with the announcement, so fast work stays silent. After
    /// that every line is forwarded: plain renderers print each one, so CI
    /// logs keep the full record of what was built.
    pub fn progress(&self, line: &str) {
        let Some(detail) = progress_detail(line) else {
            return;
        };
        {
            let mut state = self.state();
            if !state.announced {
                state.latest = Some(detail);
                return;
            }
        }
        self.emit_progress(detail);
    }

    /// A line callback that forwards each line to [`Activity::progress`].
    #[must_use]
    pub fn lines(&self) -> ActivityLines {
        let activity = self.clone();
        Arc::new(move |line| activity.progress(line))
    }

    /// Run `work`, announcing this activity if it outlasts
    /// [`ANNOUNCE_DELAY`] and reporting how it ended if it was announced.
    ///
    /// # Errors
    ///
    /// Returns the error `work` returns.
    pub async fn track<T, E>(&self, work: impl Future<Output = Result<T, E>>) -> Result<T, E> {
        self.track_after(ANNOUNCE_DELAY, work).await
    }

    async fn track_after<T, E>(
        &self,
        delay: Duration,
        work: impl Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        let mut work = std::pin::pin!(work);
        let result = tokio::select! {
            biased;
            result = &mut work => result,
            () = tokio::time::sleep(delay) => {
                self.announce();
                work.await
            }
        };
        let outcome = if result.is_ok() {
            ActivityOutcome::Succeeded
        } else {
            ActivityOutcome::Failed
        };
        self.complete(outcome);
        result
    }

    fn announce(&self) {
        let latest = {
            let mut state = self.state();
            state.announced = true;
            state.latest.take()
        };
        self.emit(ActivityEvent::Started {
            id: self.inner.id.clone(),
            message: self.inner.message.clone(),
        });
        if let Some(detail) = latest {
            self.emit_progress(detail);
        }
    }

    fn complete(&self, outcome: ActivityOutcome) {
        if !self.state().announced {
            return;
        }
        let duration_ms =
            u64::try_from(self.inner.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.emit(ActivityEvent::Completed {
            id: self.inner.id.clone(),
            message: self.inner.message.clone(),
            outcome,
            duration_ms,
        });
    }

    fn emit_progress(&self, detail: String) {
        self.emit(ActivityEvent::Progress {
            id: self.inner.id.clone(),
            detail,
        });
    }

    fn emit(&self, event: ActivityEvent) {
        match &self.inner.sink {
            ActivitySink::Bus => {
                // No bus (library use) is not an error: the status is
                // best-effort and the work continues either way.
                let _ = crate::bus::emit(EventCategory::Activity(event));
            }
            #[cfg(test)]
            ActivitySink::Captured(events) => events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event),
        }
    }

    fn state(&self) -> MutexGuard<'_, ActivityState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Turn a raw subprocess line into a short progress detail.
///
/// Terminal escapes and control characters are dropped, Nix store paths are
/// shortened to their names, and blank lines yield `None`.
fn progress_detail(line: &str) -> Option<String> {
    let plain = strip_terminal_escapes(line);
    let short = shorten_store_paths(plain.trim());
    if short.is_empty() {
        return None;
    }
    Some(match short.char_indices().nth(MAX_DETAIL_CHARS) {
        Some((cut, _)) => format!("{}…", &short[..cut]),
        None => short,
    })
}

fn strip_terminal_escapes(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            // CSI sequences: ESC '[' parameters final-byte.
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            continue;
        }
        if character == '\t' {
            plain.push(' ');
        } else if !character.is_control() {
            plain.push(character);
        }
    }
    plain
}

fn shorten_store_paths(line: &str) -> String {
    let mut short = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find(NIX_STORE_PREFIX) {
        let after = &rest[start + NIX_STORE_PREFIX.len()..];
        let hash_end = after
            .char_indices()
            .take_while(|(_, character)| character.is_ascii_alphanumeric())
            .count();
        if hash_end == NIX_STORE_HASH_LEN && after[hash_end..].starts_with('-') {
            short.push_str(&rest[..start]);
            rest = &after[hash_end + 1..];
        } else {
            short.push_str(&rest[..start + NIX_STORE_PREFIX.len()]);
            rest = after;
        }
    }
    short.push_str(rest);
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_detail_shortens_store_paths() {
        assert_eq!(
            progress_detail(
                "building '/nix/store/0123456789abcdfghijklmnpqrsvwxyz-devshell.drv'..."
            )
            .as_deref(),
            Some("building 'devshell.drv'...")
        );
    }

    #[test]
    fn progress_detail_keeps_paths_that_are_not_store_paths() {
        assert_eq!(
            progress_detail("reading /nix/store/short-name").as_deref(),
            Some("reading /nix/store/short-name")
        );
    }

    #[test]
    fn progress_detail_strips_escapes_and_blank_lines() {
        assert_eq!(
            progress_detail("\u{1b}[35;1mwarning:\u{1b}[0m Git tree is dirty\r").as_deref(),
            Some("warning: Git tree is dirty")
        );
        assert_eq!(progress_detail("  \u{1b}[0m \r"), None);
    }

    #[test]
    fn progress_detail_truncates_long_lines() {
        let detail = progress_detail(&"x".repeat(500)).unwrap_or_default();
        assert_eq!(detail.chars().count(), MAX_DETAIL_CHARS + 1);
        assert!(detail.ends_with('…'));
    }

    fn captured(message: &str) -> Activity {
        Activity::with_sink(
            message.to_string(),
            ActivitySink::Captured(Mutex::new(Vec::new())),
        )
    }

    fn events(activity: &Activity) -> Vec<ActivityEvent> {
        match &activity.inner.sink {
            ActivitySink::Captured(events) => events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            ActivitySink::Bus => Vec::new(),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn fast_work_is_silent() {
        let fast = captured("fast");
        fast.progress("building '/nix/store/x.drv'");
        let result: Result<u8, ()> = fast.track(async { Ok(1) }).await;
        assert_eq!(result, Ok(1));
        assert_eq!(events(&fast), Vec::new());
    }

    #[tokio::test(start_paused = true)]
    async fn slow_work_is_announced_with_progress_and_outcome() {
        let slow = captured("Activating Nix flake runtime (.)");
        slow.progress("evaluating flake");
        let lines = slow.lines();
        let result: Result<(), &str> = slow
            .track(async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                lines("copying path '/nix/store/0123456789abcdfghijklmnpqrsvwxyz-bash' ...");
                Err("boom")
            })
            .await;
        assert_eq!(result, Err("boom"));

        let id = slow.inner.id.clone();
        let recorded = events(&slow);
        assert_eq!(
            recorded.first(),
            Some(&ActivityEvent::Started {
                id: id.clone(),
                message: "Activating Nix flake runtime (.)".to_string(),
            })
        );
        assert!(recorded.contains(&ActivityEvent::Progress {
            id: id.clone(),
            detail: "evaluating flake".to_string(),
        }));
        assert!(recorded.contains(&ActivityEvent::Progress {
            id,
            detail: "copying path 'bash' ...".to_string(),
        }));
        assert!(matches!(
            recorded.last(),
            Some(ActivityEvent::Completed {
                outcome: ActivityOutcome::Failed,
                ..
            })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn every_line_after_announcement_is_forwarded() {
        let activity = captured("busy");
        let lines = activity.lines();
        lines("before one");
        lines("before two");
        let result: Result<(), ()> = activity
            .track(async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                for index in 0..3 {
                    lines(&format!("line {index}"));
                }
                Ok(())
            })
            .await;
        assert_eq!(result, Ok(()));
        let details: Vec<String> = events(&activity)
            .into_iter()
            .filter_map(|event| match event {
                ActivityEvent::Progress { detail, .. } => Some(detail),
                _ => None,
            })
            .collect();
        assert_eq!(details, ["before two", "line 0", "line 1", "line 2"]);
    }
}
