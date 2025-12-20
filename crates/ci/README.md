# cuenv CI - Pipeline Compiler

CI pipeline compiler that transforms cuenv task definitions into orchestrator-native CI configurations (GitLab, Buildkite, Tekton). Implements PRD v1.3 architecture with task graph semantics, environment materialization, and cache correctness.

## Overview

cuenv CI owns:

- **Task graph semantics**: Dependency resolution, cycle detection, deployment constraints
- **Environment materialization**: Nix runtime digest computation with purity enforcement
- **Cache correctness**: Content-addressable caching with secret rotation support

It delegates to orchestrators:

- **Scheduling**: Job concurrency and resource allocation
- **Approvals**: Manual intervention steps
- **Secret storage**: Credential management systems

## Architecture

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│  cuenv tasks    │────▶│   IR Compiler   │────▶│    Emitters     │
│  (CUE files)    │     │   (JSON v1.3)   │     │ GitLab/Buildkite│
└─────────────────┘     └─────────────────┘     └─────────────────┘
                               │
                               ▼
                        ┌─────────────────┐
                        │  Runtime Digest │
                        │  (Cache Keys)   │
                        └─────────────────┘
```

## Implementation Status

### ✅ Phase 1.1: IR Schema & Compiler

**Completed:**

- IR v1.3 JSON schema with full type definitions (`ir/schema.rs`)
- Task graph validation (cycles, deployment dependencies) (`ir/validation.rs`)
- Compiler from cuenv tasks to IR (`compiler/mod.rs`)
- Shell execution mode support (`shell: true|false`)
- Deployment task semantics enforcement

**Files:**

- `src/ir/schema.rs` - IR types (IntermediateRepresentation, Task, Runtime, etc.)
- `src/ir/validation.rs` - Graph validation with cycle detection
- `src/compiler/mod.rs` - Task-to-IR compiler with group handling

### ✅ Phase 1.2: Runtime Digest Computation

**Completed:**

- Content-addressable digest builder (`compiler/digest.rs`)
- SHA-256 based cache key computation
- Deterministic hashing (env vars sorted, reproducible output)
- Input file glob tracking
- Runtime configuration fingerprinting

**Features:**

- `DigestBuilder` API for incremental digest construction
- Separate inputs: command, env, inputs, runtime, secrets
- Hex-encoded SHA-256 output (`sha256:abc123...`)

### 🚧 Phase 1.3: Secret Fingerprinting (Partial)

**Completed:**

- HMAC-SHA256 implementation for secret fingerprints
- Salt-based keying (reads `CUENV_SYSTEM_SALT`)
- Deterministic secret ordering
- Secret rotation support (digest changes when secrets change)

**Remaining:**

- Integration with cuenv secret resolvers
- `CUENV_SYSTEM_SALT_PREV` for graceful rotation
- Compile-time validation when `cache_key: true` but no salt

### ⏳ Phase 1.4: Impure Flake Handling

**Completed:**

- UUID injection for unlocked flakes
- `PurityMode` enum (strict, warning, override)

**Remaining:**

- Flake.lock detection and parsing
- Warning emission for impure flakes
- UUID propagation to downstream tasks

### ⏳ Phase 1.5: Local Execution

**Remaining:**

- Task executor with digest computation
- Local file-based cache
- Secret injection from environment

### ⏳ Phase 2: Caching Infrastructure

**Remaining:**

- Bazel Remote Execution v2 integration
- Action Cache + CAS connections
- Cache policies (normal, readonly, writeonly, disabled)
- Retry logic and failure handling

### ⏳ Phase 3: Emitters

**Remaining:**

- GitLab CI emitter (`.gitlab-ci.yml`)
- Buildkite emitter (pipeline YAML)
- Tekton emitter (PipelineRun/TaskRun)

### ⏳ Phase 4: Observability & Hardening

**Remaining:**

- Log redaction (secret value replacement)
- `cuenv ci diff` command
- Metrics export (OpenTelemetry)
- Garbage collection for Nix store

### ⏳ Phase 5: Deployment Safety

**Remaining:**

- Concurrency control enforcement
- Lock timeout handling

## Usage

### Compiling Tasks to IR

```rust
use cuenv_ci::compiler::Compiler;
use cuenv_core::manifest::Project;

let project = Project::new("my-project");
// ... configure project.tasks ...

let compiler = Compiler::new(project);
let ir = compiler.compile()?;

// Serialize to JSON
let json = serde_json::to_string_pretty(&ir)?;
```

### Computing Task Digests

```rust
use cuenv_ci::compiler::digest::compute_task_digest;
use std::collections::HashMap;

let command = vec!["cargo".to_string(), "build".to_string()];
let env = HashMap::from([("RUST_LOG".to_string(), "debug".to_string())]);
let inputs = vec!["src/**/*.rs".to_string()];

let digest = compute_task_digest(&command, &env, &inputs, None, None, None);
// Output: "sha256:abc123..."
```

### Validating IR

```rust
use cuenv_ci::ir::{IrValidator, IntermediateRepresentation};

let ir: IntermediateRepresentation = /* ... */;
let validator = IrValidator::new(&ir);

match validator.validate() {
    Ok(()) => println!("IR is valid"),
    Err(errors) => {
        for error in errors {
            eprintln!("Validation error: {}", error);
        }
    }
}
```

## IR v1.3 Schema

See [schema.rs](src/ir/schema.rs) for full type definitions.

### Example IR Document

```json
{
  "version": "1.3",
  "pipeline": {
    "name": "my-pipeline",
    "trigger": {
      "branch": "main"
    }
  },
  "runtimes": [
    {
      "id": "nix-rust",
      "flake": "github:NixOS/nixpkgs/nixos-unstable",
      "output": "devShells.x86_64-linux.default",
      "system": "x86_64-linux",
      "digest": "sha256:abc123...",
      "purity": "strict"
    }
  ],
  "tasks": [
    {
      "id": "build",
      "runtime": "nix-rust",
      "command": ["cargo", "build", "--release"],
      "shell": false,
      "env": {
        "CARGO_INCREMENTAL": "0"
      },
      "inputs": ["src/**/*.rs", "Cargo.toml", "Cargo.lock"],
      "outputs": [
        {
          "path": "target/release/binary",
          "type": "cas"
        }
      ],
      "cache_policy": "normal"
    }
  ]
}
```

## Design Decisions

### Task Graph Validation

- **Cycle detection**: DFS-based algorithm with recursion stack tracking
- **Deployment constraints**: Non-deployment tasks cannot depend on deployment tasks
- **Cache policy enforcement**: Deployment tasks must have `cache_policy: disabled`

### Digest Computation

- **Determinism**: Environment variables and secrets sorted alphabetically
- **Separation of concerns**: Secrets hashed separately via HMAC (never in plaintext)
- **Impure flake handling**: UUID injection forces cache miss for unlocked flakes

### Shell Execution Modes

- `shell: false` + array command → Direct `execve()` (recommended)
- `shell: true` → Wraps in `/bin/sh -c`
- Script tasks automatically use shell mode

## Testing

```bash
# Run all tests
cuenv exec -- cargo test -p cuenv-ci

# Run specific module tests
cuenv exec -- cargo test -p cuenv-ci --lib ir::schema
cuenv exec -- cargo test -p cuenv-ci --lib ir::validation
cuenv exec -- cargo test -p cuenv-ci --lib compiler
```

## Next Steps

1. **Flake digest computation**: Parse flake.lock, compute runtime digests
2. **Secret resolver integration**: Connect to 1Password, AWS Secrets Manager, etc.
3. **Local executor**: Implement task execution with caching
4. **GitLab emitter**: Generate `.gitlab-ci.yml` from IR
5. **Buildkite emitter**: Generate pipeline YAML
6. **Bazel RE integration**: Connect to remote cache

## References

- PRD v1.3: See issue #211
- Bazel Remote Execution v2: https://github.com/bazelbuild/remote-apis
- GitLab CI YAML: https://docs.gitlab.com/ee/ci/yaml/
- Buildkite Pipeline YAML: https://buildkite.com/docs/pipelines/defining-steps
