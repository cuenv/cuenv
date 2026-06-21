//! Benchmarks for task graph operations
//!
//! Run with: cargo bench -p cuenv-task-graph

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use cuenv_task_graph::{Result as GraphResult, TaskGraph, TaskNodeData};
use std::hint::black_box;

/// Simple task type for benchmarking
#[derive(Debug, Clone)]
struct BenchTask {
    deps: Vec<String>,
    affected: bool,
}

impl TaskNodeData for BenchTask {
    fn dependency_names(&self) -> impl Iterator<Item = &str> {
        self.deps.iter().map(String::as_str)
    }
}

/// Generate a wide graph with many tasks depending on a single root
fn generate_wide_graph(task_count: usize) -> GraphResult<TaskGraph<BenchTask>> {
    let mut graph = TaskGraph::new();

    // Add root task
    let root = BenchTask {
        deps: vec![],
        affected: false,
    };
    graph.add_task("root", root)?;

    // Add many tasks that depend on root
    for i in 0..task_count {
        let task = BenchTask {
            deps: vec!["root".to_string()],
            affected: false,
        };
        graph.add_task(&format!("task_{i}"), task)?;
    }

    // Wire dependencies
    graph.add_dependency_edges()?;

    Ok(graph)
}

/// Generate a deep graph with linear dependency chain
fn generate_deep_graph(depth: usize) -> GraphResult<TaskGraph<BenchTask>> {
    let mut graph = TaskGraph::new();

    // Add first task with no deps
    let first = BenchTask {
        deps: vec![],
        affected: false,
    };
    graph.add_task("task_0", first)?;

    // Add chain of tasks, each depending on the previous
    for i in 1..depth {
        let task = BenchTask {
            deps: vec![format!("task_{}", i - 1)],
            affected: false,
        };
        graph.add_task(&format!("task_{i}"), task)?;
    }

    // Wire dependencies
    graph.add_dependency_edges()?;

    Ok(graph)
}

/// Generate a diamond graph (fan-out then fan-in)
fn generate_diamond_graph(width: usize, depth: usize) -> GraphResult<TaskGraph<BenchTask>> {
    let mut graph = TaskGraph::new();

    // Root task
    let root = BenchTask {
        deps: vec![],
        affected: false,
    };
    graph.add_task("root", root)?;

    // Fan out: many tasks depend on root
    let mut prev_level: Vec<String> = vec!["root".to_string()];

    for level in 0..depth {
        let mut current_level = Vec::new();

        for w in 0..width {
            let task_name = format!("level_{level}_task_{w}");
            let task = BenchTask {
                deps: prev_level.clone(),
                affected: false,
            };
            graph.add_task(&task_name, task)?;
            current_level.push(task_name);
        }

        prev_level = current_level;
    }

    // Final task depends on all leaf tasks
    let final_task = BenchTask {
        deps: prev_level,
        affected: false,
    };
    graph.add_task("final", final_task)?;

    // Wire dependencies
    graph.add_dependency_edges()?;

    Ok(graph)
}

/// Generate a deep pipeline where the first task is directly affected and every
/// later task depends on the previous task.
fn generate_affected_chain(task_count: usize) -> GraphResult<(TaskGraph<BenchTask>, Vec<String>)> {
    let mut graph = TaskGraph::new();
    let mut pipeline_tasks = Vec::with_capacity(task_count);

    for i in 0..task_count {
        let name = format!("task_{i}");
        let deps = if i == 0 {
            Vec::new()
        } else {
            vec![format!("task_{}", i - 1)]
        };
        let task = BenchTask {
            deps,
            affected: i == 0,
        };
        graph.add_task(&name, task)?;
        pipeline_tasks.push(name);
    }

    graph.add_dependency_edges()?;

    Ok((graph, pipeline_tasks))
}

/// Generate the same dependency chain as [`generate_affected_chain`], but with
/// pipeline order reversed to exercise order-independent propagation cost.
fn generate_reverse_affected_chain(
    task_count: usize,
) -> GraphResult<(TaskGraph<BenchTask>, Vec<String>)> {
    let (graph, mut pipeline_tasks) = generate_affected_chain(task_count)?;
    pipeline_tasks.reverse();
    Ok((graph, pipeline_tasks))
}

fn benchmark_get_parallel_groups(c: &mut Criterion) {
    let mut group = c.benchmark_group("get_parallel_groups");

    for count in [50, 100, 200, 500] {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter_batched(
                || generate_wide_graph(count),
                |graph| black_box(graph.and_then(|graph| graph.get_parallel_groups())),
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn benchmark_deep_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("deep_chain_parallel_groups");

    for depth in [10, 20, 50, 100] {
        group.bench_with_input(BenchmarkId::from_parameter(depth), &depth, |b, &depth| {
            b.iter_batched(
                || generate_deep_graph(depth),
                |graph| black_box(graph.and_then(|graph| graph.get_parallel_groups())),
                BatchSize::SmallInput,
            );
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
                b.iter_batched(
                    || generate_diamond_graph(width, depth),
                    |graph| black_box(graph.and_then(|graph| graph.get_parallel_groups())),
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn benchmark_cycle_detection(c: &mut Criterion) {
    let mut group = c.benchmark_group("cycle_detection");

    for count in [100, 500, 1000] {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter_batched(
                || generate_wide_graph(count),
                |graph| black_box(graph.map(|graph| graph.has_cycles())),
                BatchSize::SmallInput,
            );
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

fn benchmark_compute_affected(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_affected_ordered_deep_chain");

    for count in [100, 500, 1000, 2000] {
        let fixture = generate_affected_chain(count);
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &fixture,
            |b, fixture| {
                b.iter(|| {
                    black_box(fixture.as_ref().map(|(graph, pipeline_tasks)| {
                        graph.compute_affected(pipeline_tasks, |task| task.affected, None)
                    }))
                });
            },
        );
    }

    group.finish();
}

fn benchmark_compute_affected_reverse(c: &mut Criterion) {
    let mut group = c.benchmark_group("compute_affected_reverse_deep_chain");

    for count in [100, 500, 1000, 2000] {
        let fixture = generate_reverse_affected_chain(count);
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &fixture,
            |b, fixture| {
                b.iter(|| {
                    black_box(fixture.as_ref().map(|(graph, pipeline_tasks)| {
                        graph.compute_affected(pipeline_tasks, |task| task.affected, None)
                    }))
                });
            },
        );
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
    benchmark_compute_affected,
    benchmark_compute_affected_reverse,
);

criterion_main!(benches);
