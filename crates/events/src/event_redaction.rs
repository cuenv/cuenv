//! Redacting registered secrets from events, by type.
//!
//! Each event is rewritten field by field: the text it carries is redacted,
//! and its structure (variant, field names, tags, enumerations) is never
//! looked at, so no secret, whatever it equals, can rename a key, change a
//! variant or withhold an event. (Redacting the serialized form instead would
//! rewrite the tags `type`, `event` and `data` and every enumeration value,
//! as they are strings too.)

use std::borrow::Cow;

use crate::event::{
    CacheSkipReason, CiEvent, CommandEvent, EventCategory, InteractiveEvent, OutputEvent,
    ServiceEvent, SkipReason, SystemEvent, TaskEvent,
};
use crate::redaction::redact_cow;

/// Replace every registered secret in `text`.
fn scrub(text: &mut String) {
    if let Cow::Owned(redacted) = redact_cow(text) {
        *text = redacted;
    }
}

fn scrub_option(text: &mut Option<String>) {
    if let Some(text) = text {
        scrub(text);
    }
}

fn scrub_all(texts: &mut [String]) {
    for text in texts {
        scrub(text);
    }
}

impl EventCategory {
    /// Redact every registered secret from every text this category holds.
    pub(crate) fn redact_in_place(&mut self) {
        match self {
            Self::Task(event) => event.redact_in_place(),
            Self::Service(event) => event.redact_in_place(),
            Self::Ci(event) => event.redact_in_place(),
            Self::Command(event) => event.redact_in_place(),
            Self::Interactive(event) => event.redact_in_place(),
            Self::System(event) => event.redact_in_place(),
            Self::Output(event) => event.redact_in_place(),
        }
    }
}

impl TaskEvent {
    fn redact_in_place(&mut self) {
        match self {
            Self::Started {
                name,
                command,
                parent_group,
                ..
            } => {
                scrub(name);
                scrub(command);
                scrub_option(parent_group);
            }
            Self::CacheHit {
                name,
                cache_key,
                parent_group,
            } => {
                scrub(name);
                scrub(cache_key);
                scrub_option(parent_group);
            }
            Self::CacheMiss { name, parent_group }
            | Self::Queued {
                name, parent_group, ..
            }
            | Self::Retrying {
                name, parent_group, ..
            }
            | Self::Completed {
                name, parent_group, ..
            }
            | Self::GroupStarted {
                name, parent_group, ..
            }
            | Self::GroupCompleted {
                name, parent_group, ..
            } => {
                scrub(name);
                scrub_option(parent_group);
            }
            Self::CacheSkipped {
                name,
                parent_group,
                reason,
            } => {
                scrub(name);
                scrub_option(parent_group);
                reason.redact_in_place();
            }
            Self::Skipped {
                name,
                parent_group,
                reason,
            } => {
                scrub(name);
                scrub_option(parent_group);
                reason.redact_in_place();
            }
            Self::Output {
                name,
                content,
                parent_group,
                ..
            } => {
                scrub(name);
                scrub(content);
                scrub_option(parent_group);
            }
        }
    }
}

impl CacheSkipReason {
    fn redact_in_place(&mut self) {
        match self {
            Self::Disabled { reason } => scrub_option(reason),
            Self::HashFailed { reason } => scrub(reason),
            Self::UnknownProject { project } => scrub(project),
            Self::InputCollision { path } => scrub(path),
            Self::EmptyInputs
            | Self::NonPathRef
            | Self::NoResolvedInputs
            | Self::RuntimeEnv
            | Self::NeverMode
            | Self::HasherRootMismatch
            | Self::NonHermetic
            | Self::UnportableWorkdir
            | Self::SecretsWithoutCacheSalt => {}
        }
    }
}

impl SkipReason {
    fn redact_in_place(&mut self) {
        match self {
            Self::DependencyFailed { dep } => scrub(dep),
            Self::ManuallyDisabled => {}
        }
    }
}

impl ServiceEvent {
    fn redact_in_place(&mut self) {
        match self {
            Self::Pending { name }
            | Self::Ready { name, .. }
            | Self::ReadyTimeout { name, .. }
            | Self::Restarting { name, .. }
            | Self::Stopping { name }
            | Self::Stopped { name, .. } => scrub(name),
            Self::Starting { name, command } => {
                scrub(name);
                scrub(command);
            }
            Self::Output { name, line, .. } => {
                scrub(name);
                scrub(line);
            }
            Self::Failed { name, error } => {
                scrub(name);
                scrub(error);
            }
            Self::Watch { name, changed } => {
                scrub(name);
                scrub_all(changed);
            }
        }
    }
}

impl CiEvent {
    fn redact_in_place(&mut self) {
        match self {
            Self::ContextDetected {
                provider,
                event_type,
                ref_name,
            } => {
                scrub(provider);
                scrub(event_type);
                scrub(ref_name);
            }
            Self::ChangedFilesFound { .. } | Self::ProjectsDiscovered { .. } => {}
            Self::ProjectSkipped { path, reason } => {
                scrub(path);
                scrub(reason);
            }
            Self::TaskExecuting { project, task } => {
                scrub(project);
                scrub(task);
            }
            Self::TaskResult {
                project,
                task,
                error,
                ..
            } => {
                scrub(project);
                scrub(task);
                scrub_option(error);
            }
            Self::ReportGenerated { path } => scrub(path),
        }
    }
}

impl CommandEvent {
    fn redact_in_place(&mut self) {
        match self {
            Self::Started { command, args } => {
                scrub(command);
                scrub_all(args);
            }
            Self::Progress {
                command, message, ..
            } => {
                scrub(command);
                scrub(message);
            }
            Self::Completed { command, .. } => scrub(command),
        }
    }
}

impl InteractiveEvent {
    fn redact_in_place(&mut self) {
        match self {
            Self::PromptRequested {
                prompt_id,
                message,
                options,
            } => {
                scrub(prompt_id);
                scrub(message);
                scrub_all(options);
            }
            Self::PromptResolved {
                prompt_id,
                response,
            } => {
                scrub(prompt_id);
                scrub(response);
            }
            Self::WaitProgress { target, .. } => scrub(target),
        }
    }
}

impl SystemEvent {
    fn redact_in_place(&mut self) {
        match self {
            Self::SupervisorLog { tag, message } => {
                scrub(tag);
                scrub(message);
            }
            Self::Shutdown | Self::EventGap { .. } => {}
        }
    }
}

impl OutputEvent {
    fn redact_in_place(&mut self) {
        match self {
            Self::Stdout { content } | Self::Stderr { content } => scrub(content),
        }
    }
}
