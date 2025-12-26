---
title: CI Contributors
description: Reference documentation for cuenv CI stage contributors
---

This page documents the stage contributor system used by cuenv to inject setup tasks into CI pipelines. Contributors automatically add necessary steps (like installing Nix or configuring 1Password) based on project configuration.

## Overview

The cuenv CI compiler uses a **contributor system** to inject platform-specific setup tasks into workflows. Each contributor:

1. **Self-detects** whether it should be active based on IR and project state
2. **Contributes** stage tasks (bootstrap, setup, success, failure) when active
3. **Reports modifications** to enable fixed-point iteration

### Compilation Process

```
Project + Pipeline -> Compiler -> Fixed-point iteration -> IR
                                        ^
                                        |
                          Contributors (Nix, Cuenv, 1Password, Cachix, GH Models)
```

The compiler applies contributors in a loop until no contributor reports modifications (stable state).

## Stage Types

Contributors can inject tasks into four stages:

| Stage       | Purpose                                 | Example              |
| ----------- | --------------------------------------- | -------------------- |
| `Bootstrap` | Environment setup, runs first           | Install Nix          |
| `Setup`     | Provider configuration, after bootstrap | Configure 1Password  |
| `Success`   | Post-success actions                    | Notify on completion |
| `Failure`   | Post-failure actions                    | Alert on failure     |

## Built-in Contributors

### NixContributor

Installs Nix using the Determinate Systems installer.

**Activation:** Project has a Nix-based runtime (`runtime.nix` or `runtime.devenv`)

**Stage:** Bootstrap (priority 0)

**Task ID:** `install-nix`

**Configuration Example:**

```cue
import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "my-project"

runtime: schema.#NixFlake & {
    flake:  "."
    output: "devShells.x86_64-linux.default"
}
```

**ActionSpec:** Uses `DeterminateSystems/nix-installer-action@v16` on GitHub Actions.

---

### CuenvContributor

Installs or builds cuenv for use in CI pipelines.

**Activation:** Always active (cuenv is needed to run tasks)

**Stage:** Setup (priority 10)

**Task ID:** `setup-cuenv`

**Dependencies:** Depends on cuenv source mode:

- `git` / `nix`: Depends on `install-nix`
- `release` / `homebrew`: No dependencies (Nix not required)

**Configuration:**

The source and version are configured via `config.ci.cuenv`:

| Source              | Version              | Behavior                                                          | Nix Required |
| ------------------- | -------------------- | ----------------------------------------------------------------- | ------------ |
| `release` (default) | `latest` or `0.17.0` | Download pre-built binary from GitHub Releases                    | No           |
| `git`               | `self` (default)     | Build from current checkout via `nix build .#cuenv`               | Yes          |
| `git`               | `0.17.0`             | Clone specific tag and build via nix                              | Yes          |
| `nix`               | `self`               | Build from current checkout + Cachix                              | Yes          |
| `nix`               | `0.17.0`             | Install via `nix profile install github:cuenv/cuenv/0.17.0#cuenv` | Yes          |
| `homebrew`          | (ignored)            | Install via `brew install cuenv/cuenv/cuenv`                      | **No**       |

**Configuration Examples:**

```cue
import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "my-project"

// Option 1: Release mode (default) - fastest, no Nix required
config: ci: cuenv: {
    source: "release"
    version: "latest"
}

// Option 2: Homebrew mode - no Nix required
config: ci: cuenv: {
    source: "homebrew"
}

// Option 3: Git mode - build from current checkout
config: ci: cuenv: {
    source: "git"
    version: "self"
}

// Option 4: Nix mode - install specific version with Cachix
config: ci: cuenv: {
    source: "nix"
    version: "0.19.0"
}
```

---

### OnePasswordContributor

Configures 1Password secret resolution for environments with `op://` references.

**Activation:** Pipeline environment contains 1Password secret references (`op://...` URIs or `resolver: "onepassword"`)

**Stage:** Setup (priority 15)

**Task ID:** `setup-1password`

**Environment Variables:**

- `OP_SERVICE_ACCOUNT_TOKEN`: Injected from GitHub secrets

**Configuration Example:**

```cue
import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "my-project"

env: {
    production: {
        API_TOKEN: schema.#OnePasswordRef & {ref: "op://vault/api/token"}
        DEPLOY_KEY: schema.#OnePasswordRef & {ref: "op://vault/deploy/key"}
    }
}

ci: pipelines: [
    {
        name:        "deploy"
        environment: "production"  // Must match env key
        tasks: ["deploy"]
    },
]
```

---

### CachixContributor

Configures Cachix for Nix binary caching.

**Activation:** `ci.provider.github.cachix` is configured

**Stage:** Setup (priority 5)

**Task ID:** `setup-cachix`

**Dependencies:** `install-nix`

**Environment Variables:**

- `CACHIX_AUTH_TOKEN`: Injected from GitHub secrets

**Configuration Example:**

```cue
import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "my-project"

runtime: schema.#NixFlake & {
    flake:  "."
    output: "devShells.x86_64-linux.default"
}

ci: {
    provider: github: cachix: {
        name: "my-project-cache"
        // Optional: custom secret name (defaults to CACHIX_AUTH_TOKEN)
        // authToken: "MY_CACHIX_SECRET"
    }
    pipelines: [
        {
            name:  "build"
            tasks: ["build"]
        },
    ]
}
```

---

### GhModelsContributor

Installs the GitHub Models CLI extension for LLM evaluation tasks.

**Activation:** Any pipeline task uses `gh models` command

**Stage:** Setup (priority 25)

**Task ID:** `setup-gh-models`

**Configuration Example:**

```cue
import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "my-project"

ci: pipelines: [
    {
        name:  "eval"
        tasks: ["eval.prompts"]
    },
]

tasks: {
    "eval.prompts": {
        command: "gh"
        args: ["models", "eval", "prompts/test.yml"]
    }
}
```

## StageRenderer Trait

Emitters implement `StageRenderer` to convert platform-agnostic `StageTask` into native CI steps.

```rust
pub trait StageRenderer {
    type Step;
    type Error;

    fn render_task(&self, task: &StageTask) -> Result<Self::Step, Self::Error>;
    fn render_bootstrap(&self, stages: &StageConfiguration) -> Result<Vec<Self::Step>, Self::Error>;
    fn render_setup(&self, stages: &StageConfiguration) -> Result<Vec<Self::Step>, Self::Error>;
    fn render_success(&self, stages: &StageConfiguration) -> Result<Vec<Self::Step>, Self::Error>;
    fn render_failure(&self, stages: &StageConfiguration) -> Result<Vec<Self::Step>, Self::Error>;
}
```

### ActionSpec

Contributors can provide `ActionSpec` for GitHub Actions-specific rendering:

```rust
pub struct ActionSpec {
    pub uses: String,                              // e.g., "DeterminateSystems/nix-installer-action@v16"
    pub inputs: HashMap<String, serde_yaml::Value>, // Action inputs
}
```

When `ActionSpec` is present, `GitHubStageRenderer` uses the GitHub Action instead of a shell command. This allows contributors to specify both:

- A shell command (for non-GitHub platforms)
- A GitHub Action (for optimal GitHub integration)

## StageTask Schema

```rust
pub struct StageTask {
    pub id: String,                       // Unique identifier (e.g., "install-nix")
    pub provider: String,                 // Contributor ID (e.g., "nix")
    pub label: Option<String>,            // Human-readable label
    pub command: Vec<String>,             // Shell command
    pub shell: bool,                      // Whether to use shell execution
    pub env: HashMap<String, String>,     // Environment variables
    pub depends_on: Vec<String>,          // Task dependencies
    pub priority: i32,                    // Sort order (lower = earlier)
    pub action: Option<ActionSpec>,       // GitHub Action alternative
}
```

## Creating Custom Contributors

To create a custom contributor, implement the `StageContributor` trait:

```rust
use cuenv_ci::stages::StageContributor;
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation, StageTask};
use cuenv_core::manifest::Project;

pub struct MyContributor;

impl StageContributor for MyContributor {
    fn id(&self) -> &'static str {
        "my-contributor"
    }

    fn is_active(&self, ir: &IntermediateRepresentation, project: &Project) -> bool {
        // Return true if this contributor should inject tasks
        project.some_config.is_some()
    }

    fn contribute(
        &self,
        ir: &IntermediateRepresentation,
        project: &Project,
    ) -> (Vec<(BuildStage, StageTask)>, bool) {
        // Idempotency check
        if ir.stages.setup.iter().any(|t| t.id == "setup-my-thing") {
            return (vec![], false);
        }

        (
            vec![(
                BuildStage::Setup,
                StageTask {
                    id: "setup-my-thing".to_string(),
                    provider: "my-contributor".to_string(),
                    label: Some("Setup My Thing".to_string()),
                    command: vec!["my-setup-command".to_string()],
                    priority: 20,
                    ..Default::default()
                },
            )],
            true,  // Report modification
        )
    }
}
```

## Testing Contributors

The crate includes IR-level regression tests to ensure contributors work correctly:

```rust
#[test]
fn test_onepassword_contributor_active_with_op_refs() {
    let project = load_example_manifest("ci-onepassword");
    let ir = compile_with_pipeline(project, "deploy");

    assert!(
        ir.stages.setup.iter().any(|t| t.id == "setup-1password"),
        "OnePasswordContributor should inject task when op:// refs exist"
    );
}
```

See `crates/ci/tests/ir_contributor_tests.rs` for comprehensive test examples.

## See Also

- [Configuration Schema](/reference/cue-schema/) - CUE schema definitions
- [API Reference](/reference/rust-api/) - Rust API documentation
- [Examples](/reference/examples/) - Example configurations
