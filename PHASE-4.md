# Phase 4 — Performance and memory

Status: Planned
Owners: Engine
Target window: 1 week

## Goals

- Establish benchmarks and budgets; identify hotspots.
- Validate no leaks or spikes across repeated FFI calls.

## Scope (must)

- Benchmarks with criterion
  - `crates/cuengine/benches/ffi_eval.rs`:
    - cold and warm runs for small/medium CUE packages
    - parallel invocations (no Send/Sync sharing of CStringPtr)
  - Record: p95 latency, bytes allocated/op (via dhat or heaptrack guidance).
- Profiling
  - Optional: flamegraph for Rust side; pprof for Go side (behind feature).
- Stress tests
  - 10k loop invoking `evaluate_cue_package` on valid input; assert steady memory.
- Performance targets (initial)
  - Cold eval (small package): ≤ 50ms p95
  - Warm eval with cache (when implemented): ≤ 10ms p95
  - Memory: no net growth over 10k iterations.

## Acceptance criteria

- Bench results checked into repo under `target/criterion/` artifact in CI.
- No leaks detected via sanitizer runs (see Phase 5) on Linux.

## Commands

- devenv shell -- cargo bench --workspace
- devenv shell -- cargo llvm-cov --workspace --all-features
