//! Benchmarks for CUE evaluation performance

#![allow(missing_docs)] // Benchmarks don't need documentation
#![allow(clippy::unwrap_used, clippy::expect_used)] // Benchmarks can use unwrap

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use cuengine::evaluate_cue_package;
use std::fs;
use std::hint::black_box;
use tempfile::TempDir;

fn create_cue_file(size: usize) -> String {
    let mut content = String::from("package bench\n\nenv: {\n");
    for i in 0..size {
        use std::fmt::Write;
        writeln!(content, "    VAR_{i}: \"value_{i}\"").unwrap();
    }
    content.push('}');
    content
}

fn benchmark_evaluation(c: &mut Criterion) {
    let mut group = c.benchmark_group("evaluation");

    for size in &[10, 100, 1000] {
        let temp_dir = TempDir::new().unwrap();
        let content = create_cue_file(*size);
        fs::write(temp_dir.path().join("bench.cue"), content).unwrap();

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| evaluate_cue_package(black_box(temp_dir.path()), black_box("bench")));
        });
    }
    group.finish();
}

fn benchmark_memory_usage(c: &mut Criterion) {
    c.bench_function("large_config_evaluation", |b| {
        let temp_dir = TempDir::new().unwrap();
        let content = create_cue_file(10000);
        fs::write(temp_dir.path().join("large.cue"), content).unwrap();

        b.iter(|| evaluate_cue_package(black_box(temp_dir.path()), black_box("bench")));
    });
}

criterion_group!(benches, benchmark_evaluation, benchmark_memory_usage);
criterion_main!(benches);
