---
title: CUE Engine
description: Core CUE evaluation engine with Rust FFI interface
---

The CUE Engine (`cuengine`) is the foundational component of cuenv, providing fast and reliable CUE evaluation through a Rust FFI interface to the Go CUE library.

## Overview

The CUE Engine bridges the gap between Rust's performance and safety with CUE's powerful constraint-based configuration language. It provides a high-level API for evaluating CUE expressions, validating configurations, and extracting structured data.

## Architecture

```text
┌─────────────────┐    ┌──────────────┐    ┌─────────────┐
│   Rust Client   │◄──►│   cuengine   │◄──►│  Go CUE     │
│   (cuenv-core)  │    │   (FFI Layer) │    │  Evaluator  │
└─────────────────┘    └──────────────┘    └─────────────┘
```

### Key Components

**FFI Wrapper**
Provides a safe Rust interface to the Go CUE evaluation engine using CGO.

**Memory Management**
Implements RAII patterns for automatic cleanup of Go-allocated memory.

**Error Handling**
Translates Go errors into structured Rust error types with proper error codes.

**JSON Bridge**
Handles serialization/deserialization between Rust and Go data structures.

## API Reference

### CueEvaluator

`CueEvaluator` is the high-level interface for evaluating CUE packages. Build it via the fluent builder and call `evaluate` or `evaluate_typed`:

```rust
use cuengine::CueEvaluator;
use cuenv_core::manifest::Cuenv;
use std::path::Path;

let evaluator = CueEvaluator::builder()
    .max_output_size(10 * 1024 * 1024)
    .no_retry()
    .build()?;

// Evaluate and get raw JSON
let json = evaluator.evaluate(Path::new("./project"), "cuenv")?;

// Or evaluate and deserialize to a typed struct
let manifest: Cuenv = evaluator.evaluate_typed(Path::new("./project"), "cuenv")?;
```

**Common methods**

| Method                                                | Description                                                       |
| ----------------------------------------------------- | ----------------------------------------------------------------- |
| `builder()`                                           | Returns a `CueEvaluatorBuilder` with sane defaults                |
| `evaluate(&self, dir: &Path, package: &str)`          | Returns the raw JSON string emitted by the Go bridge              |
| `evaluate_typed<T>(&self, dir: &Path, package: &str)` | Deserializes the JSON into any `serde::de::DeserializeOwned` type |
| `clear_cache()`                                       | Flushes the in-memory evaluation cache                            |

**Builder options**

| Method                         | Description                                                 |
| ------------------------------ | ----------------------------------------------------------- |
| `max_path_length(len)`         | Clamp accepted directory path length                        |
| `max_package_name_length(len)` | Restrict package name length                                |
| `max_output_size(bytes)`       | Reject bridge responses larger than `bytes`                 |
| `retry_config(RetryConfig)`    | Customize retry/backoff behavior                            |
| `no_retry()`                   | Disable retries                                             |
| `cache_capacity(entries)`      | Number of cached evaluations to keep (`0` disables caching) |
| `cache_ttl(duration)`          | Expiration for cached evaluations                           |
| `build()`                      | Produce a `CueEvaluator`                                    |

### Free functions

The crate also exposes thin wrappers when you do not need a reusable evaluator:

- `evaluate_cue_package(path, package)` → `Result<String>`
- `evaluate_cue_package_typed::<T>(path, package)` → `Result<T>`
- `get_bridge_version()` → `Result<String>`

### RetryConfig

```rust
use cuengine::RetryConfig;
use std::time::Duration;

let retry = RetryConfig {
    max_attempts: 4,
    initial_delay: Duration::from_millis(50),
    max_delay: Duration::from_secs(5),
    exponential_base: 2.0,
};
```

| Field              | Description                                       |
| ------------------ | ------------------------------------------------- |
| `max_attempts`     | Maximum retry attempts before surfacing the error |
| `initial_delay`    | Delay before the first retry                      |
| `max_delay`        | Ceiling applied to the exponential backoff        |
| `exponential_base` | Multiplier for each successive delay              |

## Performance Characteristics

The CUE Engine is optimized for:

**Fast Evaluation**

- Minimal FFI overhead through efficient serialization
- Reusable evaluation contexts for batch operations
- Lazy evaluation where possible

**Memory Efficiency**

- Automatic cleanup of Go-allocated memory
- Streaming support for large configurations
- Configurable memory limits

**Concurrent Safety**

- Thread-safe evaluation contexts
- Parallel validation support
- Lock-free read operations where possible

## Integration Patterns

### Basic Usage

```rust
use cuengine::CueEvaluator;
use std::path::Path;

fn main() -> cuenv_core::Result<()> {
    let evaluator = CueEvaluator::builder().build()?;
    let json = evaluator.evaluate(Path::new("./config"), "cuenv")?;
    println!("Manifest JSON: {json}");
    Ok(())
}
```

### Configuration Validation

```rust
use cuengine::CueEvaluator;
use cuenv_core::{manifest::Cuenv, Error, Result};
use std::path::Path;

fn validate_app_config(dir: &Path) -> Result<()> {
    let evaluator = CueEvaluator::builder().build()?;
    let manifest: Cuenv = evaluator.evaluate_typed(dir, "cuenv")?;

    if manifest.env.is_none() {
        return Err(Error::configuration("env block is required"));
    }

    Ok(())
}
```

### Batch Processing

```rust
use cuengine::CueEvaluator;
use cuenv_core::Result;
use std::path::Path;

fn process_config_directory(path: &Path) -> Result<Vec<String>> {
    let evaluator = CueEvaluator::builder().no_retry().build()?;
    let mut results = Vec::new();

    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|ext| ext == "cue") {
            if let Some(dir) = entry.path().parent() {
                results.push(evaluator.evaluate(dir, "cuenv")?);
            }
        }
    }

    Ok(results)
}
```

## Testing

The engine includes comprehensive test coverage:

```bash
# Run all engine tests
cargo test -p cuengine

# Run specific test categories
cargo test -p cuengine evaluation
cargo test -p cuengine validation
cargo test -p cuengine error_handling

# Run with debugging output
RUST_LOG=debug cargo test -p cuengine -- --nocapture
```

## Troubleshooting

### Common Issues

**FFI Initialization Errors**
Ensure the Go bridge library is properly built and accessible.

**Memory Leaks**
Ensure every `CueEvaluator` (and any cached evaluation result) is dropped when no longer needed.

**Evaluation Timeouts**
Increase timeout settings for complex CUE expressions.

**Version Mismatches**
Use `get_bridge_version()` to verify Rust/Go component compatibility.

### Debug Mode

Enable debug logging for detailed operation tracing:

```rust
env_logger::init();
log::debug!("CUE evaluation trace enabled");
```

## See Also

- [cuenv-core](/explanation/cuenv-core/) - Higher-level configuration management
- [API Reference](/reference/rust-api/) - Complete API documentation
- [Examples](/reference/examples/) - Usage examples and patterns
