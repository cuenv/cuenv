//! Benchmarks for JSON event rendering.
//!
//! Run with: cargo bench -p cuenv-events --bench json_renderer

use criterion::{Criterion, criterion_group, criterion_main};
use cuenv_events::{CuenvEvent, EventCategory, EventSource, JsonRenderer, OutputEvent, TaskEvent};
use std::hint::black_box;
use uuid::Uuid;

fn task_output_event(bytes: usize) -> CuenvEvent {
    CuenvEvent::new(
        Uuid::nil(),
        EventSource::new("cuenv::task"),
        EventCategory::Task(TaskEvent::Output {
            name: "build".to_string(),
            stream: cuenv_events::Stream::Stdout,
            content: "x".repeat(bytes),
            parent_group: None,
        }),
    )
}

fn output_event(bytes: usize) -> CuenvEvent {
    CuenvEvent::new(
        Uuid::nil(),
        EventSource::new("cuenv::output"),
        EventCategory::Output(OutputEvent::Stdout {
            content: "x".repeat(bytes),
        }),
    )
}

fn benchmark_json_render_to_writer(c: &mut Criterion) {
    let renderer = JsonRenderer::new();
    let task_event = task_output_event(64 * 1024);
    let generic_event = output_event(64 * 1024);

    let mut group = c.benchmark_group("json_render_to_writer");
    group.bench_function("task_output_64k", |b| {
        let mut output = Vec::with_capacity(70 * 1024);
        b.iter(|| {
            output.clear();
            black_box(renderer.render_to_writer(black_box(&task_event), &mut output))
        });
    });
    group.bench_function("generic_output_64k", |b| {
        let mut output = Vec::with_capacity(70 * 1024);
        b.iter(|| {
            output.clear();
            black_box(renderer.render_to_writer(black_box(&generic_event), &mut output))
        });
    });
    group.finish();
}

criterion_group!(benches, benchmark_json_render_to_writer);
criterion_main!(benches);
