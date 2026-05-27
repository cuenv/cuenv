//! Benchmarks for CUE evaluation performance
//!
//! Tests both `evaluate_cue_package` (legacy) and `evaluate_module` with various options
//! to measure reference extraction overhead and detect performance regressions.

#![allow(missing_docs)] // Benchmarks don't need documentation

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use cuengine::{ModuleEvalOptions, ModuleResult, evaluate_cue_package, evaluate_module};
use std::fmt::Write;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

type BenchResult<T> = Result<T, String>;
type FixtureResult = BenchResult<TempDir>;

// ============================================================================
// CUE Fixture Generators
// ============================================================================

/// Create a flat CUE file with N environment variables
fn create_flat_cue_file(size: usize) -> String {
    let mut content = String::from("package bench\n\nenv: {\n");
    for i in 0..size {
        let _ = writeln!(content, "    VAR_{i}: \"value_{i}\"");
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
        let _ = writeln!(content, "{indent}level{level}: {{");
    }

    // Add leaf value
    let leaf_indent = indent_base.repeat(depth + 1);
    let _ = writeln!(content, "{leaf_indent}value: \"deep\"");

    // Close all braces
    for level in (0..depth).rev() {
        let indent = indent_base.repeat(level + 1);
        let _ = writeln!(content, "{indent}}}");
    }
    content.push('}');
    content
}

/// Create a wide CUE structure with many fields at the same level
fn create_wide_cue_file(width: usize) -> String {
    let mut content = String::from("package bench\n\nconfig: {\n");
    for i in 0..width {
        let _ = writeln!(content, "    field{i}: \"value{i}\"");
    }
    content.push('}');
    content
}

/// Create a CUE file with many let-binding references to test reference extraction
fn create_reference_heavy_cue_file(count: usize) -> String {
    let mut content =
        String::from("package bench\n\nlet _base = { value: \"shared\" }\n\nitems: {\n");
    for i in 0..count {
        let _ = writeln!(content, "    item{i}: _base");
    }
    content.push('}');
    content
}

/// Set up a temp directory as a CUE module (required for evaluate_module)
fn setup_cue_module(dir: &TempDir) -> std::io::Result<()> {
    let cue_mod = dir.path().join("cue.mod");
    fs::create_dir_all(&cue_mod)?;
    fs::write(cue_mod.join("module.cue"), "module: \"bench.test\"\n")
}

fn package_fixture(file_name: &str, content: String) -> FixtureResult {
    let temp_dir = TempDir::new().map_err(|err| err.to_string())?;
    fs::write(temp_dir.path().join(file_name), content).map_err(|err| err.to_string())?;
    Ok(temp_dir)
}

fn module_fixture(content: String) -> FixtureResult {
    let temp_dir = TempDir::new().map_err(|err| err.to_string())?;
    setup_cue_module(&temp_dir).map_err(|err| err.to_string())?;
    fs::write(temp_dir.path().join("bench.cue"), content).map_err(|err| err.to_string())?;
    Ok(temp_dir)
}

fn fixture_path(fixture: &FixtureResult) -> BenchResult<&Path> {
    fixture.as_ref().map(TempDir::path).map_err(Clone::clone)
}

fn evaluate_package_fixture(fixture: &FixtureResult) -> BenchResult<String> {
    let path = fixture_path(fixture)?;
    evaluate_cue_package(black_box(path), black_box("bench")).map_err(|err| err.to_string())
}

fn evaluate_module_fixture(
    fixture: &FixtureResult,
    options: &ModuleEvalOptions,
) -> BenchResult<ModuleResult> {
    let path = fixture_path(fixture)?;
    evaluate_module(black_box(path), black_box("bench"), Some(options))
        .map_err(|err| err.to_string())
}

// ============================================================================
// Legacy Benchmarks (evaluate_cue_package)
// ============================================================================

fn benchmark_evaluation(c: &mut Criterion) {
    let mut group = c.benchmark_group("evaluation");

    for size in &[10, 100, 1000] {
        let content = create_flat_cue_file(*size);
        let fixture = package_fixture("bench.cue", content);

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| black_box(evaluate_package_fixture(&fixture)));
        });
    }
    group.finish();
}

fn benchmark_memory_usage(c: &mut Criterion) {
    let content = create_flat_cue_file(10000);
    let fixture = package_fixture("large.cue", content);

    c.bench_function("large_config_evaluation", |b| {
        b.iter(|| black_box(evaluate_package_fixture(&fixture)));
    });
}

// ============================================================================
// Module Evaluation Benchmarks (evaluate_module)
// ============================================================================

fn benchmark_module_evaluation(c: &mut Criterion) {
    let mut group = c.benchmark_group("module_evaluation");

    for size in &[10, 100, 500] {
        let content = create_flat_cue_file(*size);
        let fixture = module_fixture(content);

        let options = ModuleEvalOptions::default();

        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, _| {
            b.iter(|| black_box(evaluate_module_fixture(&fixture, &options)));
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
    let content = create_reference_heavy_cue_file(100);
    let fixture = module_fixture(content);

    // Without reference extraction
    let options_no_refs = ModuleEvalOptions {
        with_references: false,
        ..Default::default()
    };

    group.bench_function("without_references", |b| {
        b.iter(|| black_box(evaluate_module_fixture(&fixture, &options_no_refs)));
    });

    // With reference extraction (exercises extractReferencesFromValue)
    let options_with_refs = ModuleEvalOptions {
        with_references: true,
        ..Default::default()
    };

    group.bench_function("with_references", |b| {
        b.iter(|| black_box(evaluate_module_fixture(&fixture, &options_with_refs)));
    });

    group.finish();
}

// ============================================================================
// Nested Structure Benchmarks (recursion performance)
// ============================================================================

fn benchmark_nested_structures(c: &mut Criterion) {
    let mut group = c.benchmark_group("nested_structures");

    for depth in &[10, 25, 50] {
        let content = create_nested_cue_file(*depth);
        let fixture = module_fixture(content);

        // Without reference extraction
        let options_no_refs = ModuleEvalOptions {
            with_references: false,
            ..Default::default()
        };

        group.bench_with_input(BenchmarkId::new("no_refs", depth), depth, |b, _| {
            b.iter(|| black_box(evaluate_module_fixture(&fixture, &options_no_refs)));
        });

        // With reference extraction
        let options_with_refs = ModuleEvalOptions {
            with_references: true,
            ..Default::default()
        };

        group.bench_with_input(BenchmarkId::new("with_refs", depth), depth, |b, _| {
            b.iter(|| black_box(evaluate_module_fixture(&fixture, &options_with_refs)));
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
        let content = create_wide_cue_file(*width);
        let fixture = module_fixture(content);

        let options_with_refs = ModuleEvalOptions {
            with_references: true,
            ..Default::default()
        };

        group.bench_with_input(BenchmarkId::from_parameter(width), width, |b, _| {
            b.iter(|| black_box(evaluate_module_fixture(&fixture, &options_with_refs)));
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
    let Some(workspace_root) = manifest_dir.parent().and_then(Path::parent) else {
        return;
    };

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
            black_box(evaluate_module(
                black_box(workspace_root),
                black_box("cuenv"),
                Some(&options_no_refs),
            ))
        });
    });

    // With reference extraction (the scenario that triggered the hang investigation)
    let options_with_refs = ModuleEvalOptions {
        with_references: true,
        ..Default::default()
    };

    group.bench_function("env_cue_with_refs", |b| {
        b.iter(|| {
            black_box(evaluate_module(
                black_box(workspace_root),
                black_box("cuenv"),
                Some(&options_with_refs),
            ))
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
