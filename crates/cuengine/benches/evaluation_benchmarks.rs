//! Benchmarks for CUE evaluation performance
//!
//! Tests both `evaluate_cue_package` (legacy) and `evaluate_module` with various options
//! to measure reference extraction overhead and detect performance regressions.

#![allow(missing_docs)] // Benchmarks don't need documentation
#![allow(clippy::unwrap_used, clippy::expect_used)] // Benchmarks can use unwrap

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use cuengine::{ModuleEvalOptions, evaluate_cue_package, evaluate_module};
use std::fmt::Write;
use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use tempfile::TempDir;

// ============================================================================
// CUE Fixture Generators
// ============================================================================

/// Create a flat CUE file with N environment variables
fn create_flat_cue_file(size: usize) -> String {
    let mut content = String::from("package bench\n\nenv: {\n");
    for i in 0..size {
        writeln!(content, "    VAR_{i}: \"value_{i}\"").unwrap();
    }
    content.push('}');
    content
}

/// Create a deeply nested CUE structure to test recursion performance
fn create_nested_cue_file(depth: usize) -> String {
    let mut content = String::from("package bench\n\nroot: {\n");
    let indent_base = "    ";

    for level in 0..depth {
        let indent = indent_base.repeat(level + 1);
        writeln!(content, "{indent}level{level}: {{").unwrap();
    }

    // Add leaf value
    let leaf_indent = indent_base.repeat(depth + 1);
    writeln!(content, "{leaf_indent}value: \"deep\"").unwrap();

    // Close all braces
    for level in (0..depth).rev() {
        let indent = indent_base.repeat(level + 1);
        writeln!(content, "{indent}}}").unwrap();
    }
    content.push('}');
    content
}

/// Create a wide CUE structure with many fields at the same level
fn create_wide_cue_file(width: usize) -> String {
    let mut content = String::from("package bench\n\nconfig: {\n");
    for i in 0..width {
        writeln!(content, "    field{i}: \"value{i}\"").unwrap();
    }
    content.push('}');
    content
}

/// Create a CUE file with many let-binding references to test reference extraction
fn create_reference_heavy_cue_file(count: usize) -> String {
    let mut content =
        String::from("package bench\n\nlet _base = { value: \"shared\" }\n\nitems: {\n");
    for i in 0..count {
        writeln!(content, "    item{i}: _base").unwrap();
    }
    content.push('}');
    content
}

/// Set up a temp directory as a CUE module (required for evaluate_module)
fn setup_cue_module(dir: &TempDir) {
    let cue_mod = dir.path().join("cue.mod");
    fs::create_dir_all(&cue_mod).unwrap();
    fs::write(cue_mod.join("module.cue"), "module: \"bench.test\"\n").unwrap();
}

// ============================================================================
// Legacy Benchmarks (evaluate_cue_package)
// ============================================================================

fn benchmark_evaluation(c: &mut Criterion) {
    let mut group = c.benchmark_group("evaluation");

    for size in &[10, 100, 1000] {
        let temp_dir = TempDir::new().unwrap();
        let content = create_flat_cue_file(*size);
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
        let content = create_flat_cue_file(10000);
        fs::write(temp_dir.path().join("large.cue"), content).unwrap();

        b.iter(|| evaluate_cue_package(black_box(temp_dir.path()), black_box("bench")));
    });
}

// ============================================================================
// Module Evaluation Benchmarks (evaluate_module)
// ============================================================================

fn benchmark_module_evaluation(c: &mut Criterion) {
    let mut group = c.benchmark_group("module_evaluation");

    for size in &[10, 100, 500] {
        let temp_dir = TempDir::new().unwrap();
        setup_cue_module(&temp_dir);
        let content = create_flat_cue_file(*size);
        fs::write(temp_dir.path().join("bench.cue"), content).unwrap();

        let options = ModuleEvalOptions::default();

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| {
                evaluate_module(
                    black_box(temp_dir.path()),
                    black_box("bench"),
                    Some(&options),
                )
            });
        });
    }
    group.finish();
}

// ============================================================================
// Reference Extraction Benchmarks
// ============================================================================

fn benchmark_reference_extraction(c: &mut Criterion) {
    let mut group = c.benchmark_group("reference_extraction");

    // Test with reference-heavy config
    let temp_dir = TempDir::new().unwrap();
    setup_cue_module(&temp_dir);
    let content = create_reference_heavy_cue_file(100);
    fs::write(temp_dir.path().join("bench.cue"), content).unwrap();

    // Without reference extraction
    let options_no_refs = ModuleEvalOptions {
        with_references: false,
        ..Default::default()
    };

    group.bench_function("without_references", |b| {
        b.iter(|| {
            evaluate_module(
                black_box(temp_dir.path()),
                black_box("bench"),
                Some(&options_no_refs),
            )
        });
    });

    // With reference extraction (exercises extractReferencesFromValue)
    let options_with_refs = ModuleEvalOptions {
        with_references: true,
        ..Default::default()
    };

    group.bench_function("with_references", |b| {
        b.iter(|| {
            evaluate_module(
                black_box(temp_dir.path()),
                black_box("bench"),
                Some(&options_with_refs),
            )
        });
    });

    group.finish();
}

// ============================================================================
// Nested Structure Benchmarks (recursion performance)
// ============================================================================

fn benchmark_nested_structures(c: &mut Criterion) {
    let mut group = c.benchmark_group("nested_structures");

    for depth in &[10, 25, 50] {
        let temp_dir = TempDir::new().unwrap();
        setup_cue_module(&temp_dir);
        let content = create_nested_cue_file(*depth);
        fs::write(temp_dir.path().join("bench.cue"), content).unwrap();

        // Without reference extraction
        let options_no_refs = ModuleEvalOptions {
            with_references: false,
            ..Default::default()
        };

        group.bench_with_input(BenchmarkId::new("no_refs", depth), depth, |b, _| {
            b.iter(|| {
                evaluate_module(
                    black_box(temp_dir.path()),
                    black_box("bench"),
                    Some(&options_no_refs),
                )
            });
        });

        // With reference extraction
        let options_with_refs = ModuleEvalOptions {
            with_references: true,
            ..Default::default()
        };

        group.bench_with_input(BenchmarkId::new("with_refs", depth), depth, |b, _| {
            b.iter(|| {
                evaluate_module(
                    black_box(temp_dir.path()),
                    black_box("bench"),
                    Some(&options_with_refs),
                )
            });
        });
    }
    group.finish();
}

// ============================================================================
// Wide Structure Benchmarks (iteration overhead)
// ============================================================================

fn benchmark_wide_structures(c: &mut Criterion) {
    let mut group = c.benchmark_group("wide_structures");

    for width in &[100, 500, 1000] {
        let temp_dir = TempDir::new().unwrap();
        setup_cue_module(&temp_dir);
        let content = create_wide_cue_file(*width);
        fs::write(temp_dir.path().join("bench.cue"), content).unwrap();

        let options_with_refs = ModuleEvalOptions {
            with_references: true,
            ..Default::default()
        };

        group.bench_with_input(BenchmarkId::from_parameter(width), width, |b, _| {
            b.iter(|| {
                evaluate_module(
                    black_box(temp_dir.path()),
                    black_box("bench"),
                    Some(&options_with_refs),
                )
            });
        });
    }
    group.finish();
}

// ============================================================================
// Real-World Benchmark (project's env.cue)
// ============================================================================

fn benchmark_real_world(c: &mut Criterion) {
    let mut group = c.benchmark_group("real_world");

    // Get path to the workspace root (where env.cue lives)
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();

    // Skip if env.cue doesn't exist (e.g., running in isolation)
    if !workspace_root.join("env.cue").exists() {
        return;
    }

    // Without reference extraction
    let options_no_refs = ModuleEvalOptions {
        with_references: false,
        ..Default::default()
    };

    group.bench_function("env_cue_no_refs", |b| {
        b.iter(|| {
            evaluate_module(
                black_box(workspace_root),
                black_box("cuenv"),
                Some(&options_no_refs),
            )
        });
    });

    // With reference extraction (the scenario that triggered the hang investigation)
    let options_with_refs = ModuleEvalOptions {
        with_references: true,
        ..Default::default()
    };

    group.bench_function("env_cue_with_refs", |b| {
        b.iter(|| {
            evaluate_module(
                black_box(workspace_root),
                black_box("cuenv"),
                Some(&options_with_refs),
            )
        });
    });

    group.finish();
}

// ============================================================================
// Criterion Configuration
// ============================================================================

criterion_group!(
    benches,
    // Legacy benchmarks
    benchmark_evaluation,
    benchmark_memory_usage,
    // Module evaluation benchmarks
    benchmark_module_evaluation,
    benchmark_reference_extraction,
    benchmark_nested_structures,
    benchmark_wide_structures,
    benchmark_real_world,
);

criterion_main!(benches);
