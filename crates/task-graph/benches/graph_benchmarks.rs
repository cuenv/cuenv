//! Benchmarks for task graph operations
//!
//! Run with: cargo bench -p cuenv-task-graph

#![allow(clippy::unwrap_used)]

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use cuenv_task_graph::{TaskGraph, TaskNodeData};
use std::hint::black_box;

/// Simple task type for benchmarking
#[derive(Debug, Clone)]
struct BenchTask {
    deps: Vec<String>,
}

impl TaskNodeData for BenchTask {
    fn depends_on(&self) -> &[String] {
        &self.deps
    }
}

/// Generate a wide graph with many tasks depending on a single root
fn generate_wide_graph(task_count: usize) -> TaskGraph<BenchTask> {
    let mut graph = TaskGraph::new();

    // Add root task
    let root = BenchTask { deps: vec![] };
    graph.add_task("root", root).unwrap();

    // Add many tasks that depend on root
    for i in 0..task_count {
        let task = BenchTask {
            deps: vec!["root".to_string()],
        };
        graph.add_task(&format!("task_{i}"), task).unwrap();
    }

    // Wire dependencies
    graph.add_dependency_edges().unwrap();

    graph
}

/// Generate a deep graph with linear dependency chain
fn generate_deep_graph(depth: usize) -> TaskGraph<BenchTask> {
    let mut graph = TaskGraph::new();

    // Add first task with no deps
    let first = BenchTask { deps: vec![] };
    graph.add_task("task_0", first).unwrap();

    // Add chain of tasks, each depending on the previous
    for i in 1..depth {
        let task = BenchTask {
            deps: vec![format!("task_{}", i - 1)],
        };
        graph.add_task(&format!("task_{i}"), task).unwrap();
    }

    // Wire dependencies
    graph.add_dependency_edges().unwrap();

    graph
}

/// Generate a diamond graph (fan-out then fan-in)
fn generate_diamond_graph(width: usize, depth: usize) -> TaskGraph<BenchTask> {
    let mut graph = TaskGraph::new();

    // Root task
    let root = BenchTask { deps: vec![] };
    graph.add_task("root", root).unwrap();

    // Fan out: many tasks depend on root
    let mut prev_level: Vec<String> = vec!["root".to_string()];

    for level in 0..depth {
        let mut current_level = Vec::new();

        for w in 0..width {
            let task_name = format!("level_{level}_task_{w}");
            let task = BenchTask {
                deps: prev_level.clone(),
            };
            graph.add_task(&task_name, task).unwrap();
            current_level.push(task_name);
        }

        prev_level = current_level;
    }

    // Final task depends on all leaf tasks
    let final_task = BenchTask { deps: prev_level };
    graph.add_task("final", final_task).unwrap();

    // Wire dependencies
    graph.add_dependency_edges().unwrap();

    graph
}

fn benchmark_get_parallel_groups(c: &mut Criterion) {
    let mut group = c.benchmark_group("get_parallel_groups");

    for count in [50, 100, 200, 500] {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            let graph = generate_wide_graph(count);
            b.iter(|| black_box(graph.get_parallel_groups().unwrap()));
        });
    }

    group.finish();
}

fn benchmark_deep_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("deep_chain_parallel_groups");

    for depth in [10, 20, 50, 100] {
        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, &depth| {
            let graph = generate_deep_graph(depth);
            b.iter(|| black_box(graph.get_parallel_groups().unwrap()));
        });
    }

    group.finish();
}

fn benchmark_diamond_graph(c: &mut Criterion) {
    let mut group = c.benchmark_group("diamond_graph");

    for (width, depth) in [(5, 5), (10, 5), (5, 10), (10, 10)] {
        let label = format!("w{width}_d{depth}");
        group.bench_with_input(
            BenchmarkId::from_parameter(&label),
            &(width, depth),
            |b, &(width, depth)| {
                let graph = generate_diamond_graph(width, depth);
                b.iter(|| black_box(graph.get_parallel_groups().unwrap()));
            },
        );
    }

    group.finish();
}

fn benchmark_cycle_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("cycle_detection");

    for count in [100, 500, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            let graph = generate_wide_graph(count);
            b.iter(|| black_box(graph.has_cycles()));
        });
    }

    group.finish();
}

fn benchmark_graph_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_construction");

    for count in [100, 500, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter(|| {
                let graph = generate_wide_graph(count);
                black_box(graph)
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    benchmark_get_parallel_groups,
    benchmark_deep_chain,
    benchmark_diamond_graph,
    benchmark_cycle_detection,
    benchmark_graph_construction,
);

criterion_main!(benches);
