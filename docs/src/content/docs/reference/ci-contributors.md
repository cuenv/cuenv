---
title: CI Contributors
description: How cuenv injects Nix, cuenv, Cachix, 1Password, and workspace setup tasks into your CI pipelines — the real schema, task IDs, priorities, and tests.
---

import { Aside, Card, CardGrid, Tabs, TabItem } from "@astrojs/starlight/components";

You wrote one task: `build`. Your generated GitHub Actions workflow installs
Determinate Nix, sets up cuenv, configures Cachix, restores the cache, and only
then runs your build — in the right order, every time. You never wrote a single
line of that scaffolding.

That is what **contributors** do. They are CUE-defined task injectors that watch
your project (its runtime, its secrets, the commands your tasks call) and splice
the matching setup steps into the task graph before it runs. The same engine
powers `cuenv task` locally and `cuenv ci` in your provider. One typed config
replaces the install-Nix / setup-cuenv / restore-cache boilerplate you would
otherwise copy between every workflow.

<Aside type="note" title="Status">
CI contributors are **stable** on GitHub Actions, which is the strongest sync
path. Buildkite sync/export is **partial**. GitLab export/sync is
**schema-only** and `cuenv sync ci` rejects it until a GitLab emitter exists.
See [Schema status](/reference/schema/status/) before relying on a provider.
</Aside>

## How a contributor fits together

A contributor is a [`#Contributor`](https://github.com/cuenv/cuenv/blob/main/schema/ci.cue)
value. It declares:

1. An `id` (the contributor name, e.g. `cachix`).
2. A `when` activation condition (when should it inject?).
3. One or more `tasks` it contributes when active.
4. Optional `autoAssociate` rules that wire your own tasks to its setup task.

The compiler applies contributors in a fixed-point loop: it evaluates every
`when`, injects matching tasks, auto-associates user tasks by command, and
repeats until the DAG stops changing. Only then is the stable graph handed to
the executor.

```
Project + Pipeline -> Compiler -> fixed-point loop -> IR -> executor
                                       ^
                                       |
                         Contributors (Nix, Cuenv, Cachix,
                         1Password, GhModels, workspaces, ...)
```

### The three layers of a task ID

This is the single most common source of confusion, so name it up front. A
contributor task has **three** identities depending on where you are looking:

| Layer | Looks like | Where you see it |
| --- | --- | --- |
| CUE `id` | `cachix.setup` | The `id` field in `contrib/contributors/*.cue` |
| Runtime task ID | `cuenv:contributor:cachix.setup` | The DAG / `cuenv task` output (CUE id prefixed with `cuenv:contributor:`) |
| Compiled IR id | `setup-cachix` | The built-in CI Intermediate Representation and the Rust contributor tests |

The CUE `id` is what you write and reference in `dependsOn`. The
`cuenv:contributor:` prefix is what the runtime DAG uses (this is also the form
`autoAssociate.injectDependency` targets). The compiled IR ids (`install-nix`,
`setup-cuenv`, `setup-cachix`, `setup-1password`, `setup-gh-models`) are the
canonical names the built-in CI compiler emits and that the integration tests
assert against. Keep all three in mind when reading test output or debugging a
workflow.

## Priority and stages

Contributor tasks carry a `priority` (lower runs first, default `10`). Priority
also decides which CI stage a task lands in:

| Priority | Stage | Purpose | Example |
| --- | --- | --- | --- |
| 0–9 | Bootstrap | Environment provisioning, runs first | Install Nix (`nix.install`, priority 2) |
| 10–49 | Setup | Tool/provider configuration | Setup cuenv (`cuenv.setup`, priority 10) |
| 50+ | Success | Post-build actions | Coverage upload, notifications |

Tasks with `condition: "on_failure"` are placed in the Failure stage regardless
of priority.

## Configuring CI before contributors can run

Contributors only inject when you have told cuenv which providers to emit
workflows for. **No workflow is generated without explicit provider
configuration.** Pipelines are a CUE **map**, and tasks are CUE **references**,
not strings. Bind the local `tasks` to a helper with `let _t = tasks` and point
the pipeline at `_t.<task>`:

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "my-project"

ci: {
	// Required: which providers to emit workflows for.
	providers: ["github"]

	pipelines: {
		build: {
			tasks: [_t.build]
			when: branch: "main"
		}
	}
}

tasks: {
	build: schema.#Task & {
		command: "echo"
		args: ["building"]
		inputs: ["env.cue"]
	}
}
```

Per-pipeline `providers` completely replaces the global list for that pipeline
(no merge). Schema-recognized providers are `"github"`, `"buildkite"`, and
`"gitlab"` — see the status note above for which are wired up today.

<Aside type="tip">
The array form `pipelines: [{name: "build", tasks: ["build"]}]` you may have seen
elsewhere is not valid against `schema/ci.cue`. Use the map form with task
references shown above.
</Aside>

## Built-in contributors

cuenv ships ready-made contributors in
[`contrib/contributors/`](https://github.com/cuenv/cuenv/tree/main/contrib/contributors).
Import them and either use a named set or pick individuals:

```cue
import "github.com/cuenv/cuenv/contrib/contributors"

// Recommended: everything, gated by its own activation conditions.
ci: contributors: contributors.#DefaultContributors

// Or a curated subset:
ci: contributors: [
	contributors.#Nix,
	contributors.#Cuenv,
	contributors.#Cachix,
]
```

The default set is the concatenation of three groups:

```text
#DefaultContributors = #WorkspaceContributors + #CoreContributors + #GitHubContributors
```

| Set | Contributors |
| --- | --- |
| `#WorkspaceContributors` | `#BunWorkspace`, `#NpmWorkspace` |
| `#CoreContributors` | `#Nix`, `#Cuenv`, `#OnePassword`, `#Infisical` |
| `#GitHubContributors` | `#Cachix`, `#NamespaceCache`, `#GhModels`, `#TrustedPublishing` |
| `#DefaultContributors` | all of the above |

Even when you include a contributor, it stays dormant until its `when` condition
matches, so adding `#DefaultContributors` is safe.

### `#Nix` — install Determinate Nix

- **CUE id:** `nix` (task `nix.install`) · **Compiled IR id:** `install-nix`
- **Stage:** Bootstrap (priority **2**)
- **Activates when:** the project uses a Nix runtime (`runtimeType: ["nix"]`)
- **GitHub Action:** `DeterminateSystems/determinate-nix-action@v3`

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "my-project"

runtime: schema.#NixRuntime & {
	flake:  "."
	output: "devShells.x86_64-linux.default"
}
```

Example: [`examples/ci-cachix`](https://github.com/cuenv/cuenv/tree/main/examples/ci-cachix)
(its Nix runtime is what makes this contributor active).

### `#Cuenv` — install or build cuenv

- **CUE id:** `cuenv` (task `cuenv.setup`) · **Compiled IR id:** `setup-cuenv`
- **Stage:** Setup (priority **10**)
- **Activates when:** always (cuenv is needed to run your tasks)
- **Dependencies:** when built from a Nix-backed source, `cuenv.setup` depends on `nix.install` (`install-nix` in the IR)

The default `#Cuenv` downloads a release binary and runs `cuenv sync ci`. In
release mode, `config.ci.cuenv.version` defaults to `"self"`, which pins the
download URL to the version of the `cuenv` binary that generated the workflow;
set it to `"latest"` or a concrete version to override that. Sibling variants
select other install strategies via the `cuenvSource` condition:
`#CuenvRelease`, `#CuenvGit`, `#CuenvNix`, `#CuenvHomebrew`, `#CuenvNative`, and
`#CuenvFromArtifact`. Pick one to override the default behaviour:

```cue
import "github.com/cuenv/cuenv/contrib/contributors"

// Build cuenv from the repository flake instead of downloading a release.
ci: contributors: [contributors.#CuenvNix]
```

In generated GitHub workflows, expanded jobs that run through `cuenv task` can
share a bootstrap artifact only when `config.ci.cuenv.source: "nix"` selects
`#CuenvNix`. The workflow emits one `build.cuenv` job per runner, uploads the
built `result/bin/cuenv` binary, and has downstream orchestrated jobs download
that artifact before running later setup tasks such as 1Password. Release,
Homebrew, native, git, and artifact sources render their normal setup task
inside each job instead. Direct Nix jobs, such as `nix build .#checks...`, do
not consume that bootstrap and start as soon as their normal Nix setup is ready.

Examples: [`examples/ci-cuenv-nix`](https://github.com/cuenv/cuenv/tree/main/examples/ci-cuenv-nix)
and [`examples/ci-cuenv-homebrew`](https://github.com/cuenv/cuenv/tree/main/examples/ci-cuenv-homebrew).

### `#Cachix` — Nix binary cache (GitHub)

- **CUE id:** `cachix` (task `cachix.setup`) · **Compiled IR id:** `setup-cachix`
- **Stage:** Setup (priority **9**)
- **Activates when:** `ci.provider.github.cachix` is configured (`providerConfig: ["github.cachix"]`)
- **Dependencies:** `nix.install`
- **GitHub Action:** `cachix/cachix-action@v17`

Configure the cache under `ci.provider.github.cachix` exactly as in the example:

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-cachix"

runtime: schema.#NixRuntime & {
	flake:  "."
	output: "devShells.x86_64-linux.default"
}

ci: {
	provider: github: cachix: {
		name: "my-project-cache"
	}
	pipelines: {
		build: {
			tasks: [_t.build]
			when: branch: "main"
		}
	}
}

tasks: {
	build: schema.#Task & {
		command: "echo"
		args: ["Building with Nix and Cachix caching"]
		inputs: ["env.cue"]
	}
}
```

The action receives `name` and `authToken` inputs; `authToken` defaults to the
`${CACHIX_AUTH_TOKEN}` secret. Example:
[`examples/ci-cachix`](https://github.com/cuenv/cuenv/tree/main/examples/ci-cachix).

### `#NamespaceCache` — Namespace nscloud Nix cache (GitHub)

- **CUE id:** `namespaceCache` (tasks `namespaceCache.setup`, `namespaceCache.prepareDeterminateReceipt`, `namespaceCache.cleanupDeterminateReceipt`)
- **Stage:** Bootstrap
- **Activates when:** `ci.provider.github.namespaceCache` is configured
- **GitHub Action:** `namespacelabs/nscloud-cache-action@v1` (Linux only)

Use this on Namespace Linux runner profiles with cache volumes instead of
`#Cachix`. It does not install Nix; it manages the Determinate Nix receipt
around the install step and skips the `/nix` cache action on macOS runners.
Example: [`examples/ci-namespace-cache`](https://github.com/cuenv/cuenv/tree/main/examples/ci-namespace-cache).

### `#OnePassword` — 1Password secret resolution

- **CUE id:** `1password` (task `1password.setup`) · **Compiled IR id:** `setup-1password`
- **Stage:** Setup (priority **20**)
- **Activates when:** the pipeline environment contains 1Password references (`secretsProvider: ["onepassword"]`)
- **Dependencies:** `cuenv.setup` (the 1Password setup runs `cuenv secrets setup onepassword`, so cuenv must already be installed)
- **Command:** `cuenv secrets setup onepassword`, with `OP_SERVICE_ACCOUNT_TOKEN` injected

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-onepassword"

env: {
	environment: production: {
		API_TOKEN:  schema.#OnePasswordRef & {ref: "op://vault/api/token"}
		DEPLOY_KEY: schema.#OnePasswordRef & {ref: "op://vault/deploy/key"}
	}
}

tasks: {
	deploy: schema.#Task & {
		command: "echo"
		args: ["Deploying with secrets from 1Password"]
		inputs: ["env.cue"]
	}
}

ci: pipelines: {
	deploy: {
		environment: "production"
		tasks: [_t.deploy]
		when: branch: "main"
	}
}
```

Example: [`examples/ci-onepassword`](https://github.com/cuenv/cuenv/tree/main/examples/ci-onepassword).
See also [How to manage secrets](/how-to/secrets/).

### `#Infisical` — Infisical secret resolution

- **CUE id:** `infisical`
- **Stage:** Setup
- **Activates when:** the environment uses Infisical references (`secretsProvider: ["infisical"]`)

Injects credential validation for the Infisical REST API. Example:
[`examples/ci-infisical`](https://github.com/cuenv/cuenv/tree/main/examples/ci-infisical).

### `#GhModels` — GitHub Models CLI (GitHub)

- **CUE id:** `gh-models` (task `gh-models.setup`) · **Compiled IR id:** `setup-gh-models`
- **Stage:** Setup (priority **25**)
- **Activates when:** any pipeline task uses the `gh` / `models` commands (`taskCommand: ["gh", "models"]`)
- **Command:** `gh extension install github/gh-models`

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-gh-models"

ci: pipelines: {
	eval: {
		tasks: [_t.evalPrompts]
		when: branch: "main"
	}
}

tasks: {
	evalPrompts: schema.#Task & {
		command: "gh"
		args: ["models", "eval", "prompts/test.yml"]
		inputs: ["prompts/**/*.yml"]
	}
}
```

Example: [`examples/ci-gh-models`](https://github.com/cuenv/cuenv/tree/main/examples/ci-gh-models).

### `#BunWorkspace` / `#NpmWorkspace` — package install (workspaces)

- **CUE ids:** `bun.workspace` / `npm.workspace`
- **Tasks:** `<pm>.workspace.install` (priority 20) and `<pm>.workspace.setup` (anchor)
- **Activates when:** the project is a member of the matching workspace (`workspaceMember: ["bun"]` / `["npm"]`)
- **Auto-association:** any task whose command is `bun`/`bunx` (or `npm`/`npx`) automatically gains a dependency on `cuenv:contributor:<pm>.workspace.setup`

These detect a `bun.lock` / `package-lock.json` and inject the dependency
install (`bun install --frozen-lockfile` / `npm ci`) ahead of your own tasks, so
you never declare the install step yourself. Example:
[`examples/ci-bun-workspace`](https://github.com/cuenv/cuenv/tree/main/examples/ci-bun-workspace).
Source: [`bun.cue`](https://github.com/cuenv/cuenv/blob/main/contrib/contributors/bun.cue),
[`npm.cue`](https://github.com/cuenv/cuenv/blob/main/contrib/contributors/npm.cue).

### `#TrustedPublishing` — OIDC publishing (GitHub)

- **CUE id:** `trustedPublishing`
- **Activates when:** `ci.provider.github.trustedPublishing.cratesIo` is set (`providerConfig: ["github.trustedPublishing.cratesIo"]`)

Enables OIDC-based crates.io authentication with no long-lived secret.

## Activation conditions

A contributor's `when` is a [`#ActivationCondition`](https://github.com/cuenv/cuenv/blob/main/schema/ci.cue).
All specified fields must be true (AND logic):

```cue
// Always active.
when: always: true

// Active for Nix-based runtimes.
when: runtimeType: ["nix", "devenv"]

// Active when any task uses these commands.
when: taskCommand: ["gh", "models"]

// Active when 1Password secrets are present.
when: secretsProvider: ["onepassword"]

// Active when a provider config path is set.
when: providerConfig: ["github.cachix"]

// Multiple fields combine with AND.
when: {
	runtimeType: ["nix"]
	cuenvSource: ["git", "nix"]
}
```

The full field set is `always`, `workspaceMember`, `runtimeType`,
`cuenvSource`, `secretsProvider`, `providerConfig`, `taskCommand`, `taskLabels`,
`environment`, `serviceCommand`, and `hasService`.

## Writing a custom contributor

Compose your own with `schema.#Contributor`. Mirror how the built-ins are
written: a `when` condition, one or more tasks with explicit `priority`, and
`dependsOn` referencing the **CUE id** of any prerequisite.

```cue
import "github.com/cuenv/cuenv/schema"

#MyToolContributor: schema.#Contributor & {
	id: "my-tool"

	// When should this inject?
	when: taskLabels: ["needs-my-tool"]

	tasks: [{
		id:       "my-tool.setup"
		label:    "Setup My Tool"
		priority: 20 // 10-49 = Setup stage

		// Either a shell command...
		command: "sh"
		args: ["-c", "curl -sSL https://example.com/install.sh | sh"]

		// ...or a provider-native step on GitHub Actions:
		provider: github: {
			uses: "my-org/setup-my-tool@v1"
			with: version: "latest"
		}
	}]
}

ci: contributors: [#MyToolContributor]
```

A contributor whose task needs cuenv (for example, to call a `cuenv secrets`
subcommand) should depend on the cuenv setup task — the **CUE id** `cuenv.setup`,
exactly as the real `#OnePassword` contributor does:

```cue
#MySecretContributor: schema.#Contributor & {
	id: "my-secret-setup"
	when: secretsProvider: ["onepassword"]
	tasks: [{
		id:        "my-secret.setup"
		label:     "Configure Secrets"
		priority:  25
		shell:     false
		dependsOn: ["cuenv.setup"] // depend on cuenv install, not onepassword.setup
		command:   "my-secret-tool configure"
		env: MY_TOKEN: "${MY_TOKEN}"
	}]
}
```

<Aside type="caution">
Earlier docs showed `dependsOn: ["onepassword.setup"]` here. That is wrong: the
1Password setup task itself depends on `cuenv.setup`, and your secret-tool setup
should usually do the same. Depend on what actually has to run first.
</Aside>

The full task shape is `#ContributorTask` in
[`schema/ci.cue`](https://github.com/cuenv/cuenv/blob/main/schema/ci.cue) —
fields include `id`, `command`/`args`/`script`, `shell`, `env`, `secrets`,
`inputs`, `outputs`, `hermetic`, `dependsOn`, `priority`, `condition`, and the
provider-specific `provider.github` override.

## Testing contributors

The CI compiler evaluates contributors and injects their tasks into the
Intermediate Representation (IR). The integration tests load a real example,
compile a named pipeline, and assert the expected compiled IR task is present.
This is the faithful shape of a test from
[`crates/ci/tests/ir_contributor_tests.rs`](https://github.com/cuenv/cuenv/blob/main/crates/ci/tests/ir_contributor_tests.rs):

```rust
#[test]
fn test_onepassword_contributor_active_with_op_refs() -> Result<(), String> {
    skip_if_ffi_unavailable!();

    // ci-onepassword has op:// refs in the production environment.
    let ir = compile_example("ci-onepassword", "deploy")?;

    // The compiled IR id is `setup-1password` (CUE id `1password.setup`).
    let setup_tasks = ir.sorted_phase_tasks(BuildStage::Setup);
    assert!(
        setup_tasks.iter().any(|t| t.id == "setup-1password"),
        "OnePasswordContributor should inject 'setup-1password' task when op:// refs exist"
    );

    // Verify the command.
    let setup_1password = phase_task(&setup_tasks, "setup-1password")?;
    assert!(
        setup_1password.command[0].contains("cuenv secrets setup onepassword"),
        "setup-1password should run 'cuenv secrets setup onepassword'"
    );
    Ok(())
}
```

Note how the test asserts against the **compiled IR id** `setup-1password`, even
though the CUE you wrote used `id: "1password.setup"`. The `phase_task` helper
looks up a task by its compiled IR id within a stage. To test your own custom
contributors, add an example project and assert that the compiled IR (or the
generated workflow) contains your expected task.

## See also

<CardGrid>
	<Card title="CI configuration schema" icon="setting">
		The `#CI`, `#Pipeline`, and `#Contributor` types in
		[`schema/ci.cue`](https://github.com/cuenv/cuenv/blob/main/schema/ci.cue),
		documented in the [CUE schema reference](/reference/cue-schema/).
	</Card>
	<Card title="Schema status" icon="approve-check">
		Which providers are stable, partial, or schema-only:
		[Schema status](/reference/schema/status/).
	</Card>
	<Card title="Secrets" icon="seti:lock">
		Pair 1Password and Infisical contributors with runtime secrets:
		[How to manage secrets](/how-to/secrets/).
	</Card>
	<Card title="Examples" icon="open-book">
		Every contributor here maps to a runnable `ci-*` project in
		[examples](/reference/examples/).
	</Card>
</CardGrid>
