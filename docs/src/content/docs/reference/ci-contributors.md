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

## Using Built-in Contributors

cuenv provides pre-defined stage contributors in `contrib/stages/`. Import and use them in your `env.cue`:

```cue
import stages "github.com/cuenv/cuenv/contrib/stages"

// Use all default contributors (recommended)
ci: stageContributors: stages.#DefaultContributors

// Or select specific sets
ci: stageContributors: stages.#CoreContributors    // Nix, Cuenv, 1Password only
ci: stageContributors: stages.#GitHubContributors  // GitHub-specific only

// Or pick individual contributors
ci: stageContributors: [
    stages.#Nix,
    stages.#Cuenv,
    stages.#Cachix,
]
```

**Available Contributors:**

| Set                    | Contributors                                 |
| ---------------------- | -------------------------------------------- |
| `#CoreContributors`    | `#Nix`, `#Cuenv`, `#OnePassword`             |
| `#GitHubContributors`  | `#Cachix`, `#GhModels`, `#TrustedPublishing` |
| `#DefaultContributors` | All of the above                             |

## Activation Conditions

Stage contributors use activation conditions to determine when they should inject tasks. All specified conditions must be true (AND logic).

```cue
#ActivationCondition: {
    // Always active (overrides other conditions)
    always?: bool

    // Active if project uses any of these runtime types
    runtimeType?: [...("nix" | "devenv" | "container" | "dagger" | "oci" | "tools")]

    // Active if cuenv source mode matches
    cuenvSource?: [...("release" | "git" | "nix" | "homebrew")]

    // Active if environment uses any of these secret providers
    secretsProvider?: [...("onepassword" | "aws" | "vault")]

    // Active if these provider config paths are set
    providerConfig?: [...string]  // e.g., ["github.cachix", "github.trustedPublishing.cratesIo"]

    // Active if any pipeline task uses these commands
    taskCommand?: [...string]  // e.g., ["gh", "models"]

    // Active if any pipeline task has these labels
    taskLabels?: [...string]

    // Active only in these environments
    environment?: [...string]  // e.g., ["production", "staging"]
}
```

**Examples:**

```cue
// Always active
when: always: true

// Active for Nix-based runtimes
when: runtimeType: ["nix", "devenv"]

// Active when 1Password secrets are used
when: secretsProvider: ["onepassword"]

// Active when Cachix is configured
when: providerConfig: ["github.cachix"]

// Multiple conditions (AND logic)
when: {
    runtimeType: ["nix"]
    cuenvSource: ["git", "nix"]
}
```

## StageTask Schema

```cue
#StageTask: {
    // Unique task identifier (e.g., "install-nix")
    id: string

    // Target stage: bootstrap, setup, success, or failure
    stage: "bootstrap" | "setup" | "success" | "failure"

    // Human-readable display name
    label?: string

    // Shell command to execute (mutually exclusive with script)
    command?: string

    // Multi-line script (mutually exclusive with command)
    script?: string

    // Wrap command in shell (default: false)
    shell: bool | *false

    // Environment variables
    env: {[string]: string}

    // Secret references
    secrets: {[string]: string | #SecretRefConfig}

    // Dependencies on other stage tasks
    dependsOn: [...string]

    // Ordering within stage (lower = earlier, default: 10)
    priority: int | *10

    // Provider-specific overrides (e.g., GitHub Actions)
    provider?: {
        github?: {
            uses: string              // Action reference
            with?: {[string]: _}      // Action inputs
        }
    }
}
```

## Creating Custom Contributors

Define custom stage contributors in CUE using the `#StageContributor` schema:

```cue
import "github.com/cuenv/cuenv/schema"

// Define a custom contributor
#MyToolContributor: schema.#StageContributor & {
    id: "my-tool"

    // Activation condition - when should this contributor be active?
    when: {
        taskLabels: ["needs-my-tool"]
    }

    // Tasks to inject when active
    tasks: [{
        id:       "setup-my-tool"
        stage:    "setup"
        label:    "Setup My Tool"
        priority: 20
        shell:    true
        command:  "curl -sSL https://example.com/install.sh | sh"

        // Optional: use GitHub Action instead of shell command
        provider: github: {
            uses: "my-org/setup-my-tool@v1"
            with: version: "latest"
        }
    }]
}

// Use in your project
ci: stageContributors: [
    #MyToolContributor,
    // ... other contributors
]
```

**Example: Contributor with secrets:**

```cue
#MySecretContributor: schema.#StageContributor & {
    id: "my-secret-setup"
    when: secretsProvider: ["onepassword"]
    tasks: [{
        id:        "setup-my-secret"
        stage:     "setup"
        label:     "Configure Secrets"
        priority:  25
        dependsOn: ["setup-1password"]
        command:   "my-secret-tool configure"
        env: MY_TOKEN: "${MY_TOKEN}"
        secrets: MY_TOKEN: "MY_TOKEN_SECRET"
    }]
}
```

## Testing Contributors

The CI compiler evaluates stage contributors and injects their tasks into the Intermediate Representation (IR). Integration tests verify the compiled output:

```rust
#[test]
fn test_onepassword_contributor_active_with_op_refs() {
    let project = load_example_manifest("ci-onepassword");
    let ir = compile_with_pipeline(project, "deploy");

    assert!(
        ir.stages.setup.iter().any(|t| t.id == "setup-1password"),
        "1Password contributor should inject task when op:// refs exist"
    );
}
```

To test your custom CUE contributors, create an example project and verify the generated IR or workflow files contain the expected tasks.

See `crates/ci/tests/ir_contributor_tests.rs` for comprehensive test examples.

## See Also

- [Configuration Schema](/reference/cue-schema/) - CUE schema definitions
- [API Reference](/reference/rust-api/) - Rust API documentation
- [Examples](/reference/examples/) - Example configurations
