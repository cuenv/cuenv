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
| `#GitHubContributors` | `#Cachix`, `#Hestia`, `#NamespaceCache`, `#GhModels`, `#TrustedPublishing`, `#CueRegistryTrustedPublishing` |
| `#DefaultContributors` | all of the above |

Even when you include a contributor, it stays dormant until its `when` condition
matches, so adding `#DefaultContributors` is safe.

`#InfisicalOidc` and `#InfisicalSecretsAction` are opt-in and are not in any
of these sets; add them next to the set you use.

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
set it to `"latest"` or a concrete version to override that. Generated release
downloads use `curl -f` so missing assets fail during download, and the shell
script falls back to `"latest"` only if an older cuenv binary leaves the version
placeholder unresolved. Sibling variants select other install strategies via the
`cuenvSource` condition:
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
that artifact into the runner temp directory before running later setup tasks
such as 1Password. The temp-directory download keeps the checkout clean for
VCS-sensitive release commands such as `cue mod publish`. Release,
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

### `#Hestia` — GitHub Actions-backed Nix binary cache

- **CUE id:** `hestia` (task `hestia.setup`)
- **Stage:** Bootstrap (priority **4**)
- **Activates when:** `ci.provider.github.hestia` is configured
- **Dependencies:** `nix.install`
- **GitHub Action:** `Mic92/hestia@fb239a2f72d4b6e26eec5425f289dea23b27a527`
- **Hestia binary:** `v2.0.0`

Configure both the contributor and provider:

```cue
import c "github.com/cuenv/cuenv/contrib/contributors"

ci: {
	providers: ["github"]
	contributors: [c.#Nix, c.#Hestia]
	provider: github: hestia: {}
	pipelines: build: {
		tasks: [_t.build]
		when: branch: "main"
	}
}
```

cuenv pins the action implementation by commit and tells it to download Hestia
v2.0.0. The generated setup enables `upstream-cache-filter`, so paths signed by
the configured upstream cache, `cache.nixos.org` by default, do not consume the
repository's GitHub Actions cache quota. It also raises `drain-timeout` to 900
seconds so the post-job step has up to 15 minutes to finish chunking, uploading
packs, and committing the manifest.

Ordinary build workflows do not need `actions: write`. Hestia uploads through
the runner-injected Actions cache token, which is independent of the workflow
`permissions` block. Concurrent jobs from the same workflow run merge their
closures into one root by union, regardless of finish order.

When any generated GitHub pipeline enables Hestia, `cuenv sync ci` also emits
`.github/workflows/cuenv-hestia-cache-gc.yml`. That repository-wide workflow
runs daily, can be dispatched manually with a `dry-run` input, serializes GC
runs, and operates only on the default branch. The maintenance workflow needs
`actions: write` to delete and repack GitHub cache entries; it keeps ordinary
build workflow permissions unchanged. When no generated GitHub pipeline enables
Hestia, normal sync removes the GC workflow and check mode reports it as stale.

Example:
[`examples/ci-hestia`](https://github.com/cuenv/cuenv/tree/main/examples/ci-hestia).

### `#NamespaceCache` — Namespace nscloud Nix cache (GitHub)

- **CUE id:** `namespaceCache` (tasks `namespaceCache.setup`, `namespaceCache.prepareDeterminateReceipt`, `namespaceCache.cleanupDeterminateReceipt`)
- **Stage:** Bootstrap
- **Activates when:** `ci.provider.github.namespaceCache` is configured
- **GitHub Action:** `namespacelabs/nscloud-cache-action@v1` (Linux only)

Use this on Namespace Linux runner profiles with cache volumes instead of
`#Cachix`. It does not install Nix; it manages the Determinate Nix receipt
around the install step and skips the `/nix` cache action on macOS runners.
Namespace gives concurrent jobs private forks of the latest committed volume
and publishes successful updates with last-write-wins semantics. It may also
serve a stale committed generation. Parallel jobs sharing one cache volume
therefore do not merge their Nix closures, so a mounted volume is not proof that
every path built by a sibling job is present. Prefer Hestia when the cache must
union outputs from a parallel GitHub Actions run.
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

Injects credential validation for the Infisical REST API
(`cuenv secrets setup infisical`, priority **20**) and passes the
`INFISICAL_CLIENT_ID`, `INFISICAL_CLIENT_SECRET`, `INFISICAL_TOKEN` and
`INFISICAL_ORGANIZATION_SLUG` CI secrets to it and to the cuenv step. Example:
[`examples/ci-infisical`](https://github.com/cuenv/cuenv/tree/main/examples/ci-infisical).

### `#InfisicalOidc` — Infisical login with GitHub OIDC (GitHub)

- **CUE id:** `infisical-oidc` (task `infisical.oidc`)
- **Stage:** Setup (priority **15**: after `cuenv.setup`, before `infisical.setup`)
- **Activates when:** the environment uses Infisical references (`secretsProvider: ["infisical"]`)
- **Permissions:** `id-token: write`
- **Parameters:** `#identityId` (default `"${INFISICAL_MACHINE_IDENTITY_ID}"`, the
  repository secret; a literal may not contain a line break), `#audience`
  (default `""`, GitHub's default audience)

Opt-in companion to `#Infisical` for Pattern A, where cuenv resolves
`#InfisicalSecret` references itself. The step requests the job's GitHub
Actions ID token (with `#audience` when set), masks it, and writes
`INFISICAL_AUTH_JWT` and `INFISICAL_MACHINE_IDENTITY_ID` to `$GITHUB_ENV` in
heredoc form with a random delimiter, so `cuenv secrets setup infisical` and
the cuenv step log in with Infisical OIDC Auth. The step fails, without
printing the value, if either value contains a line break. The JWT stays
readable by later steps of the job until it expires. `cuenv ci` logs in before
its first task, so the identity's access token TTL must cover the run. It
runs as a plain shell step (`curl` and `jq`, both on GitHub-hosted runners),
never through `cuenv exec`. The order in `ci.contributors` does not
matter. On other providers the step fails because there is no Actions ID token
endpoint.

```cue
import (
	"list"
	xContributors "github.com/cuenv/cuenv/contrib/contributors"
)

ci: contributors: list.Concat([
	xContributors.#DefaultContributors,
	[xContributors.#InfisicalOidc & {#audience: "https://github.com/my-org"}],
])
```

Example: [`examples/ci-infisical-oidc`](https://github.com/cuenv/cuenv/tree/main/examples/ci-infisical-oidc).

### `#InfisicalSecretsAction` — Infisical's official action (GitHub)

- **CUE id:** `infisical-secrets-action` (task `infisical.secrets-action`)
- **Stage:** Setup (priority **15**)
- **Activates when:** always (add it only where you want it)
- **Action:** [`Infisical/secrets-action@v1.0.18`](https://github.com/Infisical/secrets-action) with `method: oidc`
- **Permissions:** `id-token: write`
- **Parameters:** `#project` (required, exactly one of `{slug: "..."}` or
  `{id: "..."}`; setting both fails `cuenv sync ci`),
  `#envSlug` (required), `#identityId` (default the
  `INFISICAL_MACHINE_IDENTITY_ID` secret), `#secretPath` (default `"/"`),
  `#recursive` (default `false`), `#domain` (default the action's
  `https://app.infisical.com`), `#audience` (default GitHub's), `#tasks`
  (restrict the step to these task IDs' jobs)

Pattern B: the action exports every secret under the path as a job
environment variable, masked in the GitHub log; tasks read them with
`schema.#EnvPassthrough`. cuenv does not resolve, scope, apply policies to, or
redact these values. The action has no moving `v1` tag, so the contributor pins
the release. On other providers the fallback command fails explicitly.

```cue
import xContributors "github.com/cuenv/cuenv/contrib/contributors"

ci: contributors: [
	xContributors.#Cuenv,
	xContributors.#InfisicalSecretsAction & {
		#project: slug: "my-project"
		#envSlug: "prod"
	},
]
```

Example: [`examples/ci-infisical-action`](https://github.com/cuenv/cuenv/tree/main/examples/ci-infisical-action).
See [How to manage secrets](/how-to/secrets/#infisical-in-github-actions-with-oidc)
for the trade-offs between the two patterns.

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

### `#CueRegistryTrustedPublishing` — CUE Registry OIDC publishing (GitHub)

- **CUE id:** `trusted-publishing.cue-registry`
- **Activates when:** `ci.provider.github.trustedPublishing.cueRegistry` is true and the pipeline environment is `production`
- **Action:** [`cue-labs/registry-login-action`](https://github.com/cue-labs/registry-login-action)

This adds the CUE Registry login action before the conventional `publish.cue`
job's task step. The action uses GitHub OIDC and writes the CUE CLI login
configuration; the task then runs `cue mod publish` without a long-lived
`CUE_REGISTRY_TOKEN`. The workflow must grant `id-token: write`.
If a provider without GitHub Actions support emits this contributor, its
fallback command fails explicitly instead of attempting an empty action task.

## GitHub job permissions

Every generated job starts with `contents: read`. cuenv does not grant checks or
pull-request write access to expanded jobs automatically. Thin mode's `cuenv ci`
step reports a check run (`checks: write`) and posts a PR comment when a PR trigger
is enabled (`pull-requests: write`); those grants are still capped by pipeline
ceilings. The repository's thin `sync-check` pipeline is an example of this
reporter requirement. Declare a task's requirements on the task:

```cue
publishContainerImage: schema.#Task & {
    command: "docker"
    args: ["push", "ghcr.io/example/image:latest"]
    ci: github: permissions: packages: "write"
}
```

Contributor tasks use the same `ci.github.permissions` map. Their requirements
are unioned only into jobs that emit their steps, including conditional steps
that might execute. Optional `ci.github.tasks: ["publish.cue"]` restricts a
contributor step to jobs running those task IDs. An omitted or empty list means
all applicable jobs. The dedicated binary-release builder uses the synthetic
task IDs `build` and `publish`. In thin mode, a filtered step is included when any
task in the single job matches. Built-in trusted-publishing steps target `publish.crates`
and `publish.cue`; custom task names must override this list in the contributor.

Expanded and matrix jobs combine their own task's declarations and inference
with the bootstrap/setup steps they actually emit. Success/failure contributors
are emitted only in thin mode. Deployment tasks infer `contents: write` and
Orchestrator outputs infer `packages: write`; these inferences never spread from
an unrelated task. Artifact uploads use the Actions runtime token and do not
require `actions: write` on `GITHUB_TOKEN`.

**Breaking change:** global and per-pipeline `provider.github.permissions` maps
are ceilings, not grants. Each named scope caps computed access at `none`, `read`,
or `write`; omitted scopes remain unrestricted. An empty map imposes no caps.
A per-pipeline map replaces the global map. For example, `packages: "write"`
allows a publisher's declared write grant but gives a lint job no package access;
`packages: "none"` forbids package access even when requested. A ceiling below a
step's requirement intentionally prevents that operation and can make CI fail.
Move grants previously supplied only by the pipeline map onto the relevant task
or contributor before regenerating workflows.

The cuenv bootstrap job ignores pipeline maps and uses only its own injected
steps' requirements. Checkout and artifact upload alone need `contents: read`;
Determinate/FlakeHub cache authentication adds `id-token: write` when its
contributor step is present. Namespace cache volumes use the runner's existing
credentials; their cache action does not request OIDC. Hestia's ordinary cache
action uses the Actions runtime cache token, not OIDC. Its separate GC job needs `actions: write`
for cache maintenance. Release asset publishing needs `contents: write`; OIDC is
added only by authentication steps present in the job. No workflow-level
`permissions` block is emitted by any path.

After upgrading, run `cuenv sync ci` and commit the generated workflows, then
verify with `cuenv sync ci --check`.

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
