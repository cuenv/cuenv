//! Event recorder for cuenv events.
//!
//! Writes every event to a file as JSON Lines (one event per line). Pairs
//! with the `tui-replay` subcommand to deterministically replay captured
//! traces — useful for TUI snapshot testing and post-mortem analysis.

use crate::bus::EventReceiver;
use crate::event::{CuenvEvent, EventCategory, SystemEvent};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Errors that can occur while recording events.
#[derive(thiserror::Error, Debug)]
pub enum RecorderError {
    /// Failed to create the destination file.
    #[error("failed to create event recording at {path}: {source}")]
    Create {
        /// Path that failed to open.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
}

/// Writes events to a file as JSON Lines for later replay.
///
/// Each line is a single serialized [`CuenvEvent`] terminated by `\n`.
/// The recorder stops cleanly on [`SystemEvent::Shutdown`] and flushes the
/// underlying writer on drop.
#[derive(Debug)]
pub struct EventRecorder {
    writer: BufWriter<File>,
    path: PathBuf,
    events_written: u64,
}

impl EventRecorder {
    /// Create a new recorder writing to `path`.
    ///
    /// Truncates any existing file. Parent directories are not created
    /// automatically — callers should ensure the directory exists.
    ///
    /// # Errors
    ///
    /// Returns [`RecorderError::Create`] if the file cannot be opened.
    pub fn create(path: impl Into<PathBuf>) -> Result<Self, RecorderError> {
        let path = path.into();
        let file = File::create(&path).map_err(|source| RecorderError::Create {
            path: path.clone(),
            source,
        })?;
        Ok(Self {
            writer: BufWriter::new(file),
            path,
            events_written: 0,
        })
    }

    /// Path the recorder is writing to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Number of events written so far.
    #[must_use]
    pub const fn events_written(&self) -> u64 {
        self.events_written
    }

    /// Write a single event as a JSON line.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the underlying writer fails. Serialization
    /// failures are converted to `io::ErrorKind::InvalidData`.
    pub fn write_event(&mut self, event: &CuenvEvent) -> io::Result<()> {
        let line = serde_json::to_string(event)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.events_written += 1;
        Ok(())
    }

    /// Consume events from the receiver until [`SystemEvent::Shutdown`].
    ///
    /// I/O errors are surfaced via `tracing::warn!` so a recorder failure
    /// never crashes the host process. The receiver still drains to keep
    /// the broadcast bus from lagging.
    pub async fn run(mut self, mut receiver: EventReceiver) {
        while let Some(event) = receiver.recv().await {
            if let Err(err) = self.write_event(&event) {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %err,
                    "event recorder write failed; recording will be incomplete"
                );
            }
            if matches!(event.category, EventCategory::System(SystemEvent::Shutdown)) {
                break;
            }
        }
        if let Err(err) = self.writer.flush() {
            tracing::warn!(
                path = %self.path.display(),
                error = %err,
                "event recorder flush failed; recording may be truncated"
            );
        }
    }
}

/// Iterator over events recorded by [`EventRecorder`].
///
/// Reads a JSONL file produced by an `EventRecorder` and yields each event.
/// Use this from the replay subcommand to drive a TUI deterministically.
pub struct EventReplayReader {
    lines: std::io::Lines<std::io::BufReader<File>>,
}

impl EventReplayReader {
    /// Open the recording at `path` for replay.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`io::Error`] if the file cannot be opened.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        use std::io::BufRead;
        let file = File::open(path.as_ref())?;
        let reader = std::io::BufReader::new(file);
        Ok(Self {
            lines: reader.lines(),
        })
    }
}

impl Iterator for EventReplayReader {
    type Item = io::Result<CuenvEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        let line = self.lines.next()?;
        Some(line.and_then(|raw| {
            serde_json::from_str::<CuenvEvent>(&raw)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventCategory, EventSource, OutputEvent, TaskEvent, TaskKind};
    use tempfile::tempdir;
    use uuid::Uuid;

    fn make_event(category: EventCategory) -> CuenvEvent {
        CuenvEvent::new(Uuid::new_v4(), EventSource::new("test::target"), category)
    }

    #[test]
    fn write_and_read_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("trace.jsonl");

        let started = make_event(EventCategory::Task(TaskEvent::Started {
            name: "ci.build".to_string(),
            command: "cargo build".to_string(),
            hermetic: true,
            parent_group: Some("ci".to_string()),
            task_kind: TaskKind::Task,
        }));
        let completed = make_event(EventCategory::Task(TaskEvent::Completed {
            name: "ci.build".to_string(),
            success: true,
            exit_code: Some(0),
            duration_ms: 1234,
            parent_group: Some("ci".to_string()),
        }));

        {
            let mut recorder = EventRecorder::create(&path).unwrap();
            recorder.write_event(&started).unwrap();
            recorder.write_event(&completed).unwrap();
            assert_eq!(recorder.events_written(), 2);
            recorder.writer.flush().unwrap();
        }

        let events: Vec<CuenvEvent> = EventReplayReader::open(&path)
            .unwrap()
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, started.id);
        assert_eq!(events[1].id, completed.id);
    }

    #[test]
    fn replay_reader_surfaces_corrupt_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.jsonl");
        std::fs::write(&path, "{not valid json}\n").unwrap();

        let mut reader = EventReplayReader::open(&path).unwrap();
        let first = reader.next().expect("one entry");
        assert!(first.is_err());
    }

    #[test]
    fn skips_no_events_with_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        let _ = EventRecorder::create(&path).unwrap();

        let events: Vec<_> = EventReplayReader::open(&path)
            .unwrap()
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn run_drains_until_shutdown() {
        use crate::bus::EventBus;
        let dir = tempdir().unwrap();
        let path = dir.path().join("trace.jsonl");

        let bus = EventBus::new();
        let sender = bus.sender().unwrap();
        let receiver = bus.subscribe();

        let recorder = EventRecorder::create(&path).unwrap();
        let handle = tokio::spawn(recorder.run(receiver));

        sender
            .send(make_event(EventCategory::Output(OutputEvent::Stdout {
                content: "hello".to_string(),
            })))
            .unwrap();
        sender
            .send(make_event(EventCategory::System(SystemEvent::Shutdown)))
            .unwrap();

        handle.await.unwrap();

        let events: Vec<CuenvEvent> = EventReplayReader::open(&path)
            .unwrap()
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[1].category,
            EventCategory::System(SystemEvent::Shutdown)
        ));
    }
}
