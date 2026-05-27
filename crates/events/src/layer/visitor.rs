use crate::event::{
    CacheSkipReason, CiEvent, CommandEvent, CuenvEvent, EventCategory, EventSource,
    InteractiveEvent, OutputEvent, RestartReason, ServiceEvent, SkipReason, Stream, SystemEvent,
    TaskEvent, TaskKind,
};
use crate::metadata::correlation_id;
use crate::redaction::redact;
use tracing::field::{Field, Visit};

/// Visitor for extracting typed fields from tracing events.
pub(super) struct CuenvEventVisitor {
    target: String,
    event_type: Option<String>,

    // Task event fields
    task_name: Option<String>,
    command: Option<String>,
    hermetic: Option<bool>,
    cache_key: Option<String>,
    stream: Option<Stream>,
    content: Option<String>,
    success: Option<bool>,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    sequential: Option<bool>,
    task_count: Option<usize>,
    parent_group: Option<String>,
    task_kind: Option<TaskKind>,
    cache_skip_reason: Option<CacheSkipReason>,
    skip_reason: Option<SkipReason>,
    queue_position: Option<usize>,
    max_attempts: Option<u32>,
    max_concurrency: Option<u32>,
    succeeded_count: Option<usize>,
    failed_count: Option<usize>,
    skipped_count: Option<usize>,

    // Service event fields
    service_name: Option<String>,
    after_ms: Option<u64>,
    attempt: Option<u32>,
    changed: Option<Vec<String>>,

    // CI event fields
    provider: Option<String>,
    event_type_ci: Option<String>,
    ref_name: Option<String>,
    count: Option<usize>,
    path: Option<String>,
    project: Option<String>,
    task: Option<String>,
    reason: Option<String>,
    error: Option<String>,

    // Command event fields
    args: Option<Vec<String>>,
    progress: Option<f32>,
    message: Option<String>,

    // Interactive event fields
    prompt_id: Option<String>,
    options: Option<Vec<String>>,
    response: Option<String>,
    elapsed_secs: Option<u64>,

    // System event fields
    tag: Option<String>,
}

struct RedactedText {
    content: Option<String>,
    message: Option<String>,
    error: Option<String>,
}

impl CuenvEventVisitor {
    pub(super) fn new(target: &str) -> Self {
        Self {
            target: target.to_string(),
            event_type: None,
            task_name: None,
            command: None,
            hermetic: None,
            cache_key: None,
            stream: None,
            content: None,
            success: None,
            exit_code: None,
            duration_ms: None,
            sequential: None,
            task_count: None,
            parent_group: None,
            task_kind: None,
            cache_skip_reason: None,
            skip_reason: None,
            queue_position: None,
            max_attempts: None,
            max_concurrency: None,
            succeeded_count: None,
            failed_count: None,
            skipped_count: None,
            service_name: None,
            after_ms: None,
            attempt: None,
            changed: None,
            provider: None,
            event_type_ci: None,
            ref_name: None,
            count: None,
            path: None,
            project: None,
            task: None,
            reason: None,
            error: None,
            args: None,
            progress: None,
            message: None,
            prompt_id: None,
            options: None,
            response: None,
            elapsed_secs: None,
            tag: None,
        }
    }

    pub(super) fn build(self) -> Option<CuenvEvent> {
        let event_type = self.event_type.clone()?;
        let source = EventSource::new(&self.target);
        let correlation = correlation_id();

        let text = RedactedText {
            content: self.content.as_deref().map(redact),
            message: self.message.as_deref().map(redact),
            error: self.error.as_deref().map(redact),
        };

        let category = self.build_category(&event_type, text)?;
        Some(CuenvEvent::new(correlation, source, category))
    }

    fn build_category(self, event_type: &str, text: RedactedText) -> Option<EventCategory> {
        match event_type.split_once('.')?.0 {
            "task" => self
                .build_task_event(event_type, text)
                .map(EventCategory::Task),
            "service" => self
                .build_service_event(event_type, text)
                .map(EventCategory::Service),
            "ci" => self.build_ci_event(event_type, text).map(EventCategory::Ci),
            "command" => self
                .build_command_event(event_type, text)
                .map(EventCategory::Command),
            "interactive" => self
                .build_interactive_event(event_type, text)
                .map(EventCategory::Interactive),
            "system" => self
                .build_system_event(event_type, text)
                .map(EventCategory::System),
            "output" => Self::build_output_event(event_type, text).map(EventCategory::Output),
            _ => None,
        }
    }

    fn build_task_event(self, event_type: &str, text: RedactedText) -> Option<TaskEvent> {
        let content = text.content;
        let event = match event_type {
            "task.started" => TaskEvent::Started {
                name: self.task_name?,
                command: self.command?,
                hermetic: self.hermetic.unwrap_or(false),
                parent_group: self.parent_group,
                task_kind: self.task_kind.unwrap_or_default(),
            },
            "task.cache_hit" => TaskEvent::CacheHit {
                name: self.task_name?,
                cache_key: self.cache_key?,
                parent_group: self.parent_group,
            },
            "task.cache_miss" => TaskEvent::CacheMiss {
                name: self.task_name?,
                parent_group: self.parent_group,
            },
            "task.cache_skipped" => TaskEvent::CacheSkipped {
                name: self.task_name?,
                parent_group: self.parent_group,
                reason: self.cache_skip_reason?,
            },
            "task.queued" => TaskEvent::Queued {
                name: self.task_name?,
                parent_group: self.parent_group,
                queue_position: self.queue_position.unwrap_or(0),
            },
            "task.skipped" => TaskEvent::Skipped {
                name: self.task_name?,
                parent_group: self.parent_group,
                reason: self.skip_reason?,
            },
            "task.retrying" => TaskEvent::Retrying {
                name: self.task_name?,
                parent_group: self.parent_group,
                attempt: self.attempt.unwrap_or(1),
                max_attempts: self.max_attempts.unwrap_or(1),
            },
            "task.output" => TaskEvent::Output {
                name: self.task_name?,
                stream: self.stream.unwrap_or(Stream::Stdout),
                content: content?,
                parent_group: self.parent_group,
            },
            "task.completed" => TaskEvent::Completed {
                name: self.task_name?,
                success: self.success?,
                exit_code: self.exit_code,
                duration_ms: self.duration_ms.unwrap_or(0),
                parent_group: self.parent_group,
            },
            "task.group_started" => TaskEvent::GroupStarted {
                name: self.task_name?,
                sequential: self.sequential.unwrap_or(false),
                task_count: self.task_count.unwrap_or(0),
                parent_group: self.parent_group,
                max_concurrency: self.max_concurrency,
            },
            "task.group_completed" => TaskEvent::GroupCompleted {
                name: self.task_name?,
                success: self.success?,
                duration_ms: self.duration_ms.unwrap_or(0),
                parent_group: self.parent_group,
                succeeded: self.succeeded_count.unwrap_or(0),
                failed: self.failed_count.unwrap_or(0),
                skipped: self.skipped_count.unwrap_or(0),
            },
            _ => return None,
        };
        Some(event)
    }

    fn build_service_event(self, event_type: &str, text: RedactedText) -> Option<ServiceEvent> {
        let RedactedText { content, error, .. } = text;
        let event = match event_type {
            "service.pending" => ServiceEvent::Pending {
                name: self.service_name?,
            },
            "service.starting" => ServiceEvent::Starting {
                name: self.service_name?,
                command: self.command.unwrap_or_default(),
            },
            "service.output" => ServiceEvent::Output {
                name: self.service_name?,
                stream: self.stream.unwrap_or(Stream::Stdout),
                line: content?,
            },
            "service.ready" => ServiceEvent::Ready {
                name: self.service_name?,
                after_ms: self.after_ms.unwrap_or(0),
            },
            "service.stopping" => ServiceEvent::Stopping {
                name: self.service_name?,
            },
            "service.stopped" => ServiceEvent::Stopped {
                name: self.service_name?,
                exit_code: self.exit_code,
            },
            "service.failed" => ServiceEvent::Failed {
                name: self.service_name?,
                error: error?,
            },
            "service.ready_timeout" => ServiceEvent::ReadyTimeout {
                name: self.service_name?,
                after_ms: self.after_ms.unwrap_or(0),
            },
            "service.restarting" => ServiceEvent::Restarting {
                name: self.service_name?,
                reason: match self.reason.as_deref() {
                    Some("watch" | "watch_triggered") => RestartReason::WatchTriggered,
                    Some("manual") => RestartReason::Manual,
                    _ => RestartReason::Crashed,
                },
                attempt: self.attempt.unwrap_or(0),
            },
            "service.watch" => ServiceEvent::Watch {
                name: self.service_name?,
                changed: self.changed.unwrap_or_default(),
            },
            _ => return None,
        };
        Some(event)
    }

    fn build_ci_event(self, event_type: &str, text: RedactedText) -> Option<CiEvent> {
        let error = text.error;
        let event = match event_type {
            "ci.context_detected" => CiEvent::ContextDetected {
                provider: self.provider?,
                event_type: self.event_type_ci?,
                ref_name: self.ref_name?,
            },
            "ci.changed_files" => CiEvent::ChangedFilesFound { count: self.count? },
            "ci.projects_discovered" => CiEvent::ProjectsDiscovered { count: self.count? },
            "ci.project_skipped" => CiEvent::ProjectSkipped {
                path: self.path?,
                reason: self.reason?,
            },
            "ci.task_executing" => CiEvent::TaskExecuting {
                project: self.project?,
                task: self.task?,
            },
            "ci.task_result" => CiEvent::TaskResult {
                project: self.project?,
                task: self.task?,
                success: self.success?,
                error,
            },
            "ci.report_generated" => CiEvent::ReportGenerated { path: self.path? },
            _ => return None,
        };
        Some(event)
    }

    fn build_command_event(self, event_type: &str, text: RedactedText) -> Option<CommandEvent> {
        let message = text.message;
        let event = match event_type {
            "command.started" => CommandEvent::Started {
                command: self.command?,
                args: self.args.unwrap_or_default(),
            },
            "command.progress" => CommandEvent::Progress {
                command: self.command?,
                progress: self.progress?,
                message: message?,
            },
            "command.completed" => CommandEvent::Completed {
                command: self.command?,
                success: self.success?,
                duration_ms: self.duration_ms.unwrap_or(0),
            },
            _ => return None,
        };
        Some(event)
    }

    fn build_interactive_event(
        self,
        event_type: &str,
        text: RedactedText,
    ) -> Option<InteractiveEvent> {
        let message = text.message;
        let event = match event_type {
            "interactive.prompt_requested" => InteractiveEvent::PromptRequested {
                prompt_id: self.prompt_id?,
                message: message?,
                options: self.options.unwrap_or_default(),
            },
            "interactive.prompt_resolved" => InteractiveEvent::PromptResolved {
                prompt_id: self.prompt_id?,
                response: self.response?,
            },
            "interactive.wait_progress" => InteractiveEvent::WaitProgress {
                target: self.task_name.or(self.path)?,
                elapsed_secs: self.elapsed_secs?,
            },
            _ => return None,
        };
        Some(event)
    }

    fn build_system_event(self, event_type: &str, text: RedactedText) -> Option<SystemEvent> {
        let message = text.message;
        let event = match event_type {
            "system.supervisor_log" => SystemEvent::SupervisorLog {
                tag: self.tag?,
                message: message?,
            },
            "system.shutdown" => SystemEvent::Shutdown,
            _ => return None,
        };
        Some(event)
    }

    fn build_output_event(event_type: &str, text: RedactedText) -> Option<OutputEvent> {
        let content = text.content;
        let event = match event_type {
            "output.stdout" => OutputEvent::Stdout { content: content? },
            "output.stderr" => OutputEvent::Stderr { content: content? },
            _ => return None,
        };
        Some(event)
    }
}

impl Visit for CuenvEventVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "event_type" => self.event_type = Some(value.to_string()),
            "task_name" | "name" => self.task_name = Some(value.to_string()),
            "service_name" => self.service_name = Some(value.to_string()),
            "command" | "cmd" => self.command = Some(value.to_string()),
            "cache_key" => self.cache_key = Some(value.to_string()),
            "content" => self.content = Some(value.to_string()),
            "provider" => self.provider = Some(value.to_string()),
            "ci_event_type" => self.event_type_ci = Some(value.to_string()),
            "ref_name" => self.ref_name = Some(value.to_string()),
            "path" => self.path = Some(value.to_string()),
            "project" => self.project = Some(value.to_string()),
            "task" => self.task = Some(value.to_string()),
            "reason" => self.reason = Some(value.to_string()),
            "error" => self.error = Some(value.to_string()),
            "message" => self.message = Some(value.to_string()),
            "prompt_id" => self.prompt_id = Some(value.to_string()),
            "response" => self.response = Some(value.to_string()),
            "tag" => self.tag = Some(value.to_string()),
            "stream" => self.stream = parse_stream(value),
            "task_kind" => self.task_kind = parse_task_kind(value),
            "cache_skip_reason" => {
                self.cache_skip_reason = serde_json::from_str(value).ok();
            }
            "skip_reason" => {
                self.skip_reason = serde_json::from_str(value).ok();
            }
            _ => {}
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        match field.name() {
            "exit_code" => self.exit_code = i32::try_from(value).ok(),
            "duration_ms" => self.duration_ms = u64::try_from(value).ok(),
            "after_ms" => self.after_ms = u64::try_from(value).ok(),
            "attempt" => self.attempt = u32::try_from(value).ok(),
            "max_attempts" => self.max_attempts = u32::try_from(value).ok(),
            "count" => self.count = usize::try_from(value).ok(),
            "task_count" => self.task_count = usize::try_from(value).ok(),
            "queue_position" => self.queue_position = usize::try_from(value).ok(),
            "max_concurrency" => self.max_concurrency = u32::try_from(value).ok(),
            "succeeded" => self.succeeded_count = usize::try_from(value).ok(),
            "failed_count" => self.failed_count = usize::try_from(value).ok(),
            "skipped_count" => self.skipped_count = usize::try_from(value).ok(),
            "elapsed_secs" => self.elapsed_secs = u64::try_from(value).ok(),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "duration_ms" => self.duration_ms = Some(value),
            "after_ms" => self.after_ms = Some(value),
            "attempt" => self.attempt = u32::try_from(value).ok(),
            "max_attempts" => self.max_attempts = u32::try_from(value).ok(),
            "count" => self.count = usize::try_from(value).ok(),
            "task_count" => self.task_count = usize::try_from(value).ok(),
            "queue_position" => self.queue_position = usize::try_from(value).ok(),
            "max_concurrency" => self.max_concurrency = u32::try_from(value).ok(),
            "succeeded" => self.succeeded_count = usize::try_from(value).ok(),
            "failed_count" => self.failed_count = usize::try_from(value).ok(),
            "skipped_count" => self.skipped_count = usize::try_from(value).ok(),
            "elapsed_secs" => self.elapsed_secs = Some(value),
            _ => {}
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "progress" {
            self.progress = Some(progress_from_f64(value));
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        match field.name() {
            "hermetic" => self.hermetic = Some(value),
            "success" => self.success = Some(value),
            "sequential" => self.sequential = Some(value),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let value_str = format!("{value:?}");
        match field.name() {
            "args" => {
                if let Ok(args) = serde_json::from_str::<Vec<String>>(&value_str) {
                    self.args = Some(args);
                }
            }
            "options" => {
                if let Ok(options) = serde_json::from_str::<Vec<String>>(&value_str) {
                    self.options = Some(options);
                }
            }
            "changed" => {
                if let Ok(changed) = serde_json::from_str::<Vec<String>>(&value_str) {
                    self.changed = Some(changed);
                }
            }
            // Fallback: try to extract string fields from debug formatting.
            // When tracing uses Display formatting (%), it wraps values in a
            // DisplayValue which then gets passed to record_debug instead of
            // record_str.
            "event_type" | "task_name" | "service_name" | "name" | "command" | "cmd"
            | "content" | "cache_key" | "stream" | "error" | "reason" | "task_kind" => {
                let cleaned = value_str.trim_matches('"');
                match field.name() {
                    "event_type" => self.event_type = Some(cleaned.to_string()),
                    "task_name" | "name" => self.task_name = Some(cleaned.to_string()),
                    "service_name" => self.service_name = Some(cleaned.to_string()),
                    "command" | "cmd" => self.command = Some(cleaned.to_string()),
                    "content" => self.content = Some(cleaned.to_string()),
                    "cache_key" => self.cache_key = Some(cleaned.to_string()),
                    "error" => self.error = Some(cleaned.to_string()),
                    "reason" => self.reason = Some(cleaned.to_string()),
                    "stream" => self.stream = parse_stream(cleaned),
                    "task_kind" => self.task_kind = parse_task_kind(cleaned),
                    _ => {}
                }
            }
            // These fields carry JSON-encoded enum values via Display
            // formatting (%). When the JSON itself is already-quoted, serde
            // needs those quotes to deserialize the unit variant.
            "cache_skip_reason" => {
                let cleaned = value_str.trim_matches('"');
                self.cache_skip_reason = serde_json::from_str(cleaned)
                    .ok()
                    .or_else(|| serde_json::from_str(&value_str).ok());
            }
            "skip_reason" => {
                let cleaned = value_str.trim_matches('"');
                self.skip_reason = serde_json::from_str(cleaned)
                    .ok()
                    .or_else(|| serde_json::from_str(&value_str).ok());
            }
            "exit_code" => {
                self.exit_code = parse_optional_debug_i32(&value_str);
            }
            "parent_group" => {
                self.parent_group = parse_optional_debug_str(&value_str);
            }
            "max_concurrency" => {
                self.max_concurrency = parse_optional_debug_u32(&value_str);
            }
            _ => {}
        }
    }
}

fn parse_stream(value: &str) -> Option<Stream> {
    match value {
        "stdout" => Some(Stream::Stdout),
        "stderr" => Some(Stream::Stderr),
        _ => None,
    }
}

fn parse_task_kind(value: &str) -> Option<TaskKind> {
    match value {
        "task" => Some(TaskKind::Task),
        "group" => Some(TaskKind::Group),
        "sequence" => Some(TaskKind::Sequence),
        _ => None,
    }
}

fn parse_optional_debug_i32(value: &str) -> Option<i32> {
    if value == "None" {
        return None;
    }

    let number = value
        .strip_prefix("Some(")
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(value);
    number.parse::<i32>().ok()
}

/// Parse a Rust-`Debug`-formatted `Option<impl Debug>` containing a string.
///
/// Handles `Some("foo")`, `Some(foo)`, and `None`.
fn parse_optional_debug_str(value: &str) -> Option<String> {
    if value == "None" {
        return None;
    }
    let inner = value.strip_prefix("Some(")?.strip_suffix(')')?;
    Some(inner.trim_matches('"').to_string())
}

fn parse_optional_debug_u32(value: &str) -> Option<u32> {
    if value == "None" {
        return None;
    }
    let number = value.strip_prefix("Some(")?.strip_suffix(')')?;
    number.parse::<u32>().ok()
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "tracing records progress as f64 while CommandEvent stores the original f32-sized value"
)]
fn progress_from_f64(value: f64) -> f32 {
    value as f32
}
