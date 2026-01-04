---
title: CI Contributors
description: Reference documentation for cuenv CI contributors
---

This page documents the contributor system used by cuenv to inject setup tasks into CI pipelines. Contributors automatically add necessary steps (like installing Nix or configuring 1Password) based on project configuration.

## Overview

The cuenv CI compiler uses a **contributor system** to inject platform-specific setup tasks into workflows. Each contributor:

1. **Self-detects** whether it should be active based on IR and project state
2. **Contributes** phase tasks (bootstrap, setup, success, failure) when active
3. **Reports modifications** to enable fixed-point iteration

### Compilation Process

```
Project + Pipeline -> Compiler -> Fixed-point iteration -> IR
                                        ^
                                        |
                          Contributors (Nix, Cuenv, 1Password, Cachix, GH Models)
```

The compiler applies contributors in a loop until no contributor reports modifications (stable state).

## Task Priority and Ordering

Contributors use **priority** values to determine task ordering. Lower values run first:

| Priority Range | Stage       | Purpose                                 | Example              |
| -------------- | ----------- | --------------------------------------- | -------------------- |
| 0-9            | Bootstrap   | Environment setup, runs first           | Install Nix          |
| 10-49          | Setup       | Provider configuration, after bootstrap | Configure 1Password  |
| 50+            | Success     | Post-build actions                      | Notify on completion |

Tasks with `condition: "on_failure"` are placed in the Failure stage regardless of priority.

## Built-in Contributors

### NixContributor

Installs Nix using the Determinate Systems installer.

**Activation:** Project has a Nix-based runtime (`runtime.nix` or `runtime.devenv`)

**Phase:** Bootstrap (priority 0)

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

**Phase:** Setup (priority 10)

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

**Phase:** Setup (priority 15)

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

**Phase:** Setup (priority 5)

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

**Phase:** Setup (priority 25)

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

cuenv provides pre-defined contributors in `contrib/contributors/`. Import and use them in your `env.cue`:

```cue
import "github.com/cuenv/cuenv/contrib/contributors"

// Use all default contributors (recommended)
ci: contributors: contributors.#DefaultContributors

// Or select specific sets
ci: contributors: contributors.#CoreContributors    // Nix, Cuenv, 1Password only
ci: contributors: contributors.#GitHubContributors  // GitHub-specific only

// Or pick individual contributors
ci: contributors: [
    contributors.#Nix,
    contributors.#Cuenv,
    contributors.#Cachix,
]
```

**Available Contributors:**

| Set                    | Contributors                                 |
| ---------------------- | -------------------------------------------- |
| `#CoreContributors`    | `#Nix`, `#Cuenv`, `#OnePassword`             |
| `#GitHubContributors`  | `#Cachix`, `#GhModels`, `#TrustedPublishing` |
| `#DefaultContributors` | All of the above                             |

## Activation Conditions

Contributors use activation conditions to determine when they should inject tasks. All specified conditions must be true (AND logic).

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

## ContributorTask Schema

```cue
#ContributorTask: {
    // Task identifier (will be prefixed with cuenv:contributor:)
    id: string

    // Human-readable display name
    label?: string

    // Human-readable description
    description?: string

    // Shell command to execute
    command?: string

    // Command arguments
    args?: [...string]

    // Multi-line script (alternative to command)
    script?: string

    // Wrap command in shell (default: false)
    shell: bool | *false

    // Environment variables
    env: {[string]: string}

    // Secret references
    secrets: {[string]: string | #SecretRefConfig}

    // Input files/patterns for caching
    inputs?: [...string]

    // Output files/patterns for caching
    outputs?: [...string]

    // Whether task requires hermetic execution
    hermetic: bool | *false

    // Dependencies on other tasks
    dependsOn: [...string]

    // Ordering priority (lower = earlier, default: 10)
    // 0-9: Bootstrap, 10-49: Setup, 50+: Success
    priority: int | *10

    // Execution condition (on_success, on_failure, always)
    condition?: "on_success" | "on_failure" | "always"

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

Define custom contributors in CUE using the `#Contributor` schema:

```cue
import "github.com/cuenv/cuenv/schema"

// Define a custom contributor
#MyToolContributor: schema.#Contributor & {
    id: "my-tool"

    // Activation condition - when should this contributor be active?
    when: {
        taskLabels: ["needs-my-tool"]
    }

    // Tasks to inject when active
    tasks: [{
        id:       "my-tool.setup"
        label:    "Setup My Tool"
        priority: 20  // 10-49 = Setup stage
        command:  "sh"
        args:     ["-c", "curl -sSL https://example.com/install.sh | sh"]

        // Optional: use GitHub Action instead of shell command
        provider: github: {
            uses: "my-org/setup-my-tool@v1"
            with: version: "latest"
        }
    }]
}

// Use in your project
ci: contributors: [
    #MyToolContributor,
    // ... other contributors
]
```

**Example: Contributor with secrets:**

```cue
#MySecretContributor: schema.#Contributor & {
    id: "my-secret-setup"
    when: secretsProvider: ["onepassword"]
    tasks: [{
        id:        "my-secret.setup"
        label:     "Configure Secrets"
        priority:  25  // 10-49 = Setup stage
        dependsOn: ["onepassword.setup"]
        command:   "my-secret-tool"
        args:      ["configure"]
        env: MY_TOKEN: "${MY_TOKEN}"
        secrets: MY_TOKEN: "MY_TOKEN_SECRET"
    }]
}
```

## Testing Contributors

The CI compiler evaluates contributors and injects their tasks into the Intermediate Representation (IR). Integration tests verify the compiled output:

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
