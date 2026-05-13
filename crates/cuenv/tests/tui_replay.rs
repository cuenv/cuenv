//! Integration tests for the `cuenv tui-replay` subcommand and the
//! recorder/reader round-trip it depends on.
//!
//! These tests exercise the public surface end-to-end without booting the
//! interactive TUI:
//! - Recording a synthesised trace as JSONL via [`cuenv_events::EventRecorder`].
//! - Reading it back via [`cuenv_events::EventReplayReader`] and verifying
//!   every field survives the round-trip.
//! - Invoking the `cuenv tui-replay --help` CLI surface so the subcommand
//!   remains registered.
//! - Invoking `cuenv tui-replay` against an empty / missing trace to confirm
//!   the diagnostic paths return non-zero with a useful message.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Command;
use std::str;

use cuenv_events::{
    CacheSkipReason, CuenvEvent, EventCategory, EventRecorder, EventReplayReader, EventSource,
    Stream, SystemEvent, TaskEvent, TaskKind,
};
use uuid::Uuid;

const CUENV_BIN: &str = env!("CARGO_BIN_EXE_cuenv");

fn clean_command(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default());
    cmd
}

fn sample_trace() -> Vec<CuenvEvent> {
    let mk = |cat| CuenvEvent::new(Uuid::new_v4(), EventSource::new("cuenv::test"), cat);
    vec![
        mk(EventCategory::Task(TaskEvent::GroupStarted {
            name: "ci".to_string(),
            sequential: false,
            task_count: 2,
            parent_group: None,
            max_concurrency: Some(2),
        })),
        mk(EventCategory::Task(TaskEvent::Started {
            name: "ci.fmt".to_string(),
            command: "cuenv fmt".to_string(),
            hermetic: true,
            parent_group: Some("ci".to_string()),
            task_kind: TaskKind::Task,
        })),
        mk(EventCategory::Task(TaskEvent::CacheSkipped {
            name: "ci.fmt".to_string(),
            parent_group: Some("ci".to_string()),
            reason: CacheSkipReason::EmptyInputs,
        })),
        mk(EventCategory::Task(TaskEvent::Output {
            name: "ci.fmt".to_string(),
            stream: Stream::Stdout,
            content: "no changes".to_string(),
            parent_group: Some("ci".to_string()),
        })),
        mk(EventCategory::Task(TaskEvent::Completed {
            name: "ci.fmt".to_string(),
            success: true,
            exit_code: Some(0),
            duration_ms: 42,
            parent_group: Some("ci".to_string()),
        })),
        mk(EventCategory::Task(TaskEvent::GroupCompleted {
            name: "ci".to_string(),
            success: true,
            duration_ms: 100,
            parent_group: None,
            succeeded: 2,
            failed: 0,
            skipped: 0,
        })),
        mk(EventCategory::System(SystemEvent::Shutdown)),
    ]
}

#[test]
fn round_trip_preserves_every_phase_0_field() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trace.jsonl");

    let trace = sample_trace();
    {
        let mut recorder = EventRecorder::create(&path).unwrap();
        for event in &trace {
            recorder.write_event(event).unwrap();
        }
        assert_eq!(recorder.events_written(), trace.len() as u64);
    }

    let restored: Vec<CuenvEvent> = EventReplayReader::open(&path)
        .unwrap()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(restored.len(), trace.len());

    for (i, (left, right)) in trace.iter().zip(restored.iter()).enumerate() {
        let lj = serde_json::to_value(left).unwrap();
        let rj = serde_json::to_value(right).unwrap();
        assert_eq!(lj, rj, "event {i} round-trip diverged");
    }
}

#[test]
fn replay_reader_skips_no_events_with_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.jsonl");
    let _ = EventRecorder::create(&path).unwrap();

    let restored: Vec<_> = EventReplayReader::open(&path)
        .unwrap()
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    assert!(restored.is_empty());
}

#[test]
fn cli_tui_replay_subcommand_is_registered() {
    let mut cmd = clean_command(CUENV_BIN);
    let output = cmd.arg("tui-replay").arg("--help").output().unwrap();
    let stdout = str::from_utf8(&output.stdout).unwrap();
    assert!(
        output.status.success(),
        "expected `tui-replay --help` to succeed, stderr={}",
        str::from_utf8(&output.stderr).unwrap()
    );
    assert!(
        stdout.contains("Replay a recorded event trace"),
        "expected help to mention replay; got: {stdout}"
    );
    assert!(stdout.contains("--fast"), "missing --fast flag: {stdout}");
    assert!(
        stdout.contains("--snapshot-frames-to"),
        "missing --snapshot-frames-to flag: {stdout}"
    );
}

#[test]
fn cli_tui_replay_rejects_missing_file() {
    let missing = PathBuf::from("/definitely/not/here.jsonl");
    let mut cmd = clean_command(CUENV_BIN);
    let output = cmd.arg("tui-replay").arg(&missing).output().unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for missing recording"
    );
    let stderr = str::from_utf8(&output.stderr).unwrap();
    assert!(
        stderr.contains("failed to open event recording"),
        "stderr should explain the failure; got: {stderr}"
    );
}

#[test]
fn cli_tui_replay_rejects_empty_recording() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.jsonl");
    let _ = EventRecorder::create(&path).unwrap();

    let mut cmd = clean_command(CUENV_BIN);
    let output = cmd.arg("tui-replay").arg(&path).output().unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for empty recording"
    );
    let stderr = str::from_utf8(&output.stderr).unwrap();
    assert!(
        stderr.contains("is empty"),
        "stderr should mention empty recording; got: {stderr}"
    );
}
