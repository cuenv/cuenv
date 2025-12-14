---
title: cuenv-events
description: Structured event system for multi-frontend applications
---

The `cuenv-events` crate provides a unified event system that enables multiple UI frontends (CLI, TUI, Web) to subscribe to a single event stream. Events are emitted using tracing macros and captured by a custom tracing Layer.

## Overview

When building tools with multiple output modes (CLI, JSON, TUI), you need a consistent way to emit and render events. This crate provides:

- Typed event schema for task execution, CI, commands, and more
- Broadcast-based event bus for multiple subscribers
- Tracing layer integration for automatic event capture
- CLI and JSON renderers out of the box

## Architecture

```text
┌─────────────────────────────────────────────────────────────────────────┐
│                           cuenv-events crate                            │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌─────────────┐ │
│  │ Event Schema │  │ EventBus     │  │ Tracing Layer│  │ Renderers   │ │
│  │ (typed)      │  │ (broadcast)  │  │ (capture)    │  │ (CLI/JSON)  │ │
│  └──────────────┘  └──────────────┘  └──────────────┘  └─────────────┘ │
└─────────────────────────────────────────────────────────────────────────┘

Event Flow:
┌──────────┐     ┌──────────────┐     ┌───────────────┐     ┌──────────┐
│emit_*!() │────►│ Tracing Layer│────►│   EventBus    │────►│ Renderer │
│ macros   │     │ (capture)    │     │ (broadcast)   │     │ (output) │
└──────────┘     └──────────────┘     └───────────────┘     └──────────┘
```

### Key Components

**Event Schema**
Typed event definitions for tasks, CI, commands, interactive prompts, and system events.

**EventBus**
Broadcast channel for distributing events to multiple subscribers.

**CuenvEventLayer**
Tracing layer that captures events emitted via tracing macros.

**Renderers**
CLI and JSON output formatters for events.

## API Reference

### Event Types

The crate defines several event categories:

```rust
use cuenv_events::{
    CuenvEvent,
    TaskEvent,      // Task execution events
    CiEvent,        // CI pipeline events
    CommandEvent,   // CLI command events
    InteractiveEvent, // Prompts and user interaction
    SystemEvent,    // Internal system events
    OutputEvent,    // stdout/stderr capture
};
```

### EventBus

Central event distribution:

```rust
use cuenv_events::EventBus;

// Create event bus
let bus = EventBus::new();

// Get sender for emitting events
let sender = bus.sender();

// Subscribe to events
let mut receiver = bus.subscribe();

// Receive events (async)
while let Ok(event) = receiver.recv().await {
    println!("Received: {:?}", event);
}
```

### CuenvEventLayer

Tracing layer for capturing events:

```rust
use cuenv_events::{EventBus, CuenvEventLayer};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

let bus = EventBus::new();
let layer = CuenvEventLayer::new(bus.sender().inner);

tracing_subscriber::registry()
    .with(layer)
    .init();
```

### Emit Macros

Type-safe macros for emitting events:

#### Task Events

```rust
use cuenv_events::{
    emit_task_started,
    emit_task_completed,
    emit_task_output,
    emit_task_cache_hit,
    emit_task_cache_miss,
    emit_task_group_started,
    emit_task_group_completed,
};

// Task lifecycle
emit_task_started!("build", "cargo build", true);
emit_task_output!("build", "stdout", "Compiling...");
emit_task_completed!("build", true, Some(0), 1234);

// Caching
emit_task_cache_hit!("build", "abc123");
emit_task_cache_miss!("test");

// Task groups
emit_task_group_started!("all", false, 3);
emit_task_group_completed!("all", true, 5000);
```

#### CI Events

```rust
use cuenv_events::{
    emit_ci_context,
    emit_ci_changed_files,
    emit_ci_projects_discovered,
    emit_ci_project_skipped,
    emit_ci_task_executing,
    emit_ci_task_result,
    emit_ci_report,
};

emit_ci_context!("github", "push", "main");
emit_ci_changed_files!(10);
emit_ci_projects_discovered!(3);
emit_ci_project_skipped!("/path", "no tasks");
emit_ci_task_executing!("/path", "build");
emit_ci_task_result!("/path", "build", true);
emit_ci_report!("/path/report.json");
```

#### Command Events

```rust
use cuenv_events::{
    emit_command_started,
    emit_command_progress,
    emit_command_completed,
};

emit_command_started!("env");
emit_command_started!("task", vec!["build".to_string()]);
emit_command_progress!("env", 0.5, "loading");
emit_command_completed!("env", true, 100);
```

#### Interactive Events

```rust
use cuenv_events::{
    emit_prompt_requested,
    emit_prompt_resolved,
    emit_wait_progress,
};

emit_prompt_requested!("p1", "Continue?", vec!["yes", "no"]);
emit_prompt_resolved!("p1", "yes");
emit_wait_progress!("hook", 5);
```

#### System Events

```rust
use cuenv_events::{
    emit_supervisor_log,
    emit_shutdown,
    emit_stdout,
    emit_stderr,
};

emit_supervisor_log!("supervisor", "started");
emit_shutdown!();
emit_stdout!("hello");
emit_stderr!("error");
```

### Renderers

Built-in output formatters:

```rust
use cuenv_events::{CliRenderer, JsonRenderer, CuenvEvent};

// CLI renderer for human-readable output
let cli = CliRenderer::new();

// JSON renderer for machine-readable output
let json = JsonRenderer::new();
```

### Correlation IDs

Track related events across requests:

```rust
use cuenv_events::{correlation_id, set_correlation_id};

// Set correlation ID for current context
set_correlation_id("req-123");

// Get current correlation ID
let id = correlation_id();
```

## Integration Patterns

### Basic Setup

```rust
use cuenv_events::{EventBus, CuenvEventLayer, emit_task_started, emit_task_completed};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn main() {
    // Create event bus
    let bus = EventBus::new();
    let layer = CuenvEventLayer::new(bus.sender().inner);

    // Initialize tracing with event layer
    tracing_subscriber::registry()
        .with(layer)
        .init();

    // Now events are captured
    emit_task_started!("build", "cargo build", false);
    // ... do work ...
    emit_task_completed!("build", true, Some(0), 1000);
}
```

### Multi-Frontend Architecture

```rust
use cuenv_events::{EventBus, CuenvEventLayer, CliRenderer, JsonRenderer};
use tokio::sync::mpsc;

async fn run_with_output(json_mode: bool) {
    let bus = EventBus::new();

    // Spawn renderer based on mode
    let mut receiver = bus.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = receiver.recv().await {
            if json_mode {
                // Output as JSON
                println!("{}", serde_json::to_string(&event).unwrap());
            } else {
                // Output as human-readable CLI
                println!("{:?}", event);
            }
        }
    });

    // Setup tracing layer
    let layer = CuenvEventLayer::new(bus.sender().inner);
    // ... initialize subscriber ...
}
```

### Event Filtering

```rust
use cuenv_events::{EventBus, CuenvEvent, EventCategory};

async fn filtered_subscriber(bus: &EventBus) {
    let mut receiver = bus.subscribe();

    while let Ok(event) = receiver.recv().await {
        // Only handle task events
        if let CuenvEvent::Task(_) = event {
            println!("Task event: {:?}", event);
        }
    }
}
```

### Progress Reporting

```rust
use cuenv_events::{emit_command_started, emit_command_progress, emit_command_completed};
use std::time::Instant;

fn process_files(files: &[String]) {
    emit_command_started!("process");
    let start = Instant::now();

    for (i, file) in files.iter().enumerate() {
        let progress = (i + 1) as f32 / files.len() as f32;
        emit_command_progress!("process", progress, format!("Processing {}", file));
        // ... process file ...
    }

    let duration_ms = start.elapsed().as_millis() as u64;
    emit_command_completed!("process", true, duration_ms);
}
```

## Event Schema Reference

### TaskEvent Variants

| Event Type       | Fields                                    |
| ---------------- | ----------------------------------------- |
| `Started`        | name, command, hermetic                   |
| `CacheHit`       | name, cache_key                           |
| `CacheMiss`      | name                                      |
| `Output`         | name, stream (stdout/stderr), content     |
| `Completed`      | name, success, exit_code, duration_ms     |
| `GroupStarted`   | name, sequential, task_count              |
| `GroupCompleted` | name, success, duration_ms                |

### CiEvent Variants

| Event Type          | Fields                              |
| ------------------- | ----------------------------------- |
| `ContextDetected`   | provider, event_type, ref_name      |
| `ChangedFiles`      | count                               |
| `ProjectsDiscovered`| count                               |
| `ProjectSkipped`    | path, reason                        |
| `TaskExecuting`     | project, task                       |
| `TaskResult`        | project, task, success, error?      |
| `ReportGenerated`   | path                                |

### CommandEvent Variants

| Event Type  | Fields                              |
| ----------- | ----------------------------------- |
| `Started`   | command, args?                      |
| `Progress`  | command, progress (0.0-1.0), message|
| `Completed` | command, success, duration_ms       |

## Testing

```bash
# Run all event tests
cargo test -p cuenv-events

# Run with tokio runtime
cargo test -p cuenv-events --features tokio
```

## See Also

- [cuengine](/explanation/cuengine/) - CUE evaluation engine
- [Architecture](/explanation/architecture/) - System architecture overview
- [API Reference](/reference/rust-api/) - Complete API documentation
