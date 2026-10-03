# cuenv end-to-end testing plan with tester-army/e2e

Status: the bounded macOS pilot/Phase 0 evidence passes locally; Linux coverage,
CI wiring, and the broader feature lanes remain pending.

Date: 2026-10-03

## Decision summary

Use [tester-army/e2e](https://github.com/tester-army/e2e) as a deterministic,
black-box acceptance runner for the built `cuenv` binary. Start with an
engine-free target and TypeScript fixtures that create disposable projects,
spawn real cuenv processes, and assert on exit status, structured output,
files, processes, and environment state.

Keep Rust, CUE, and Go tests as the primary owners of schema constraints, FFI
contracts, graph algorithms, schedulers, cache protocols, provider protocols,
and exhaustive error behavior. The e2e suite should prove that those pieces
compose correctly for a user of the installed binary; it should not reimplement
the unit and property-test suites in TypeScript.

Do not build a browser wrapper around the CLI. A custom terminal engine is an
optional later lane for real shell, TUI, and desktop journeys, and must first
pass the feasibility gate below. Browser e2e is relevant to the VS Code
webviews, not to the core CLI. Cuetty needs a separate desktop-driver decision.

The [schema coverage matrix](schema-coverage-matrix.md) remains the source of
truth. Every exported schema definition and every CLI-only feature must map to
an acceptance scenario, an existing native test, or an explicit unsupported /
partial limitation test.

## Pilot implementation

The first vertical slice is represented by a root TypeScript suite:

- `e2e.config.ts` uses the published `e2e` 0.16.0 runner with a tools-only CLI
  target, two workers, zero retries, and the e2e replay cache disabled.
- `tests/e2e/support/fixtures.ts` requires an absolute `CUENV_TEST_BIN`, copies
  the checked-out `schema/` into each temporary CUE module, and gives every
  attempt private HOME, XDG, state, cache, runtime, and temporary directories.
- `tests/e2e/cli-and-environment.e2e.ts` covers help, exact env/JSON output,
  named-environment overrides, invalid CUE, and unknown-task errors.
- `tests/e2e/task-execution.e2e.ts` covers dependency order, failed dependency
  side-effect suppression, task groups, JSON task listing, and timeout cleanup
  for a task process group plus an explicit descendant.

This pilot deliberately does not replace the Cucumber suite. It complements
the existing readable BDD journeys with black-box process and fixture
evidence. Parallel barriers, hermetic sentinels, cache invalidation, real
shell/PTY behavior, and provider boundaries remain subsequent slices.

Sol's adversarial review drove the timeout and failure-path safeguards in this
slice: the fixture sends SIGINT to cuenv first so cuenv can clean up its
separately registered task process groups, tracks configured groups and
descendants before timeout assertions, and uses a bounded process-group
fallback. The initial tasks disable task-result caching through
`CUENV_CACHE=off` and use file side effects only for deterministic ordering
and absence checks. The latest local evidence uses a fresh Nix-built
`cuenv 0.57.0` on macOS ARM64: 10/10 scenarios passed, and three repeats
passed 30/30. Its SHA-256 is
`153354957fdf2b4decbf4397b35effe38c4c871097a5870e9ea0ac01df2f3006`.
Linux/Nix CI evidence and workflow wiring remain pending.

## Why this boundary fits `e2e`

The framework's documented engines are web and mobile, but it also supports a
target without a UI engine for API and deterministic tests. Its fixtures,
assertions, retries, workers, tags, sharding, and JUnit/JSON reporting are
useful for a CLI acceptance harness. Its custom-engine contract can later drive
a terminal or desktop surface, but that would be new cuenv test infrastructure,
not a capability supplied by the framework.

Important constraints to carry into the design:

- Pin a released `e2e` package and lockfile. The upstream project is still
  pre-1.0 and its API may change between minor releases.
- The runner is not a security sandbox. Test code, fixtures, reporters,
  engines, and app commands run with the host's permissions.
- The runner's command log is not a substitute for secret redaction. The
  fixture must keep plaintext secrets out of child-process logs and must scan
  raw captured output before publishing sanitized artifacts.
- Model-backed steps are unnecessary for the initial CLI lane. If agent steps
  are added later, deterministic assertions remain the final oracle and the
  agent must never receive authority to construct arbitrary shell commands.
- A timed-out test body can outlive the runner's immediate control flow. The
  subprocess fixture must own cancellation, process-group termination, bounded
  waits, and idempotent cleanup.
- `e2e`'s replay cache is unrelated to cuenv's content-addressed task cache.
  Tests of cuenv caching must disable or isolate e2e action replay so a replay
  cannot masquerade as a product cache hit.

Primary references:

- [tester-army/e2e repository](https://github.com/tester-army/e2e)
- [e2e API testing and engine-free targets](https://e2e.tester.army/docs/api-testing)
- [e2e continuous integration](https://e2e.tester.army/docs/ci)
- [e2e security model](https://e2e.tester.army/docs/security)
- [e2e custom executors](https://e2e.tester.army/docs/executors)
- [e2e custom engine contract](https://e2e.tester.army/docs/writing-an-engine)

## Test-layer ownership

| Layer | Owns | Does not claim |
| --- | --- | --- |
| CUE and Go bridge tests | Schema unification, constraints, package selection, FFI conversion, timeout and concurrency contracts | Successful use of the installed CLI |
| Rust unit/property tests | Manifest types, graph algorithms, scheduling, cache keys, redaction, provider protocol, service probes, event contracts | Full multi-process journeys |
| Rust integration and BDD tests | Existing CLI, hooks, task, sync, VCS, infrastructure, and example coverage | Real interactive-shell behavior unless a real shell is used |
| `e2e` engine-free CLI suite | Built-binary journeys across disposable projects and real child processes | Exhaustive internal branches or external provider correctness |
| Real shell / PTY lane | Bash, Zsh, Fish, prompt hooks, cancellation, TUI input/output | Browser or mobile UI behavior |
| Provider and network canaries | Disposable real provider, registry, REAPI, Docker/Nix, and hosted-workflow boundaries | Required PR checks on untrusted code |
| Native UI / custom engine lane | VS Code webviews, and possibly Cuetty after a driver exists | Core CLI coverage |

## Feature coverage portfolio

The following portfolio covers the current product surface. `P0` means an
offline, deterministic merge-gate candidate; `P1` means a local integration or
trusted CI candidate; `P2` means an expensive, credentialed, UI, or exploratory
candidate. Existing evidence means that source/tests were inspected, not that
they passed during this planning pass.

| Area | Current evidence to preserve | Acceptance scenarios to add | Owner / tier |
| --- | --- | --- | --- |
| Schema, evaluation, and discovery | `schema/**`, `crates/cuengine/**`, `crates/core/src/manifest/**`, `crates/cuenv/tests/schema_project_base_tests.rs` | Valid project evaluates; invalid syntax and unknown fields fail with useful paths; selected package and nested module are isolated; unrelated CUE packages are ignored; evaluation timeout is explicit | Native exhaustive; representative `EVAL-*` P0 |
| CLI surface, help, output, and errors | `crates/cuenv/src/cli/commands.rs`, `crates/cuenv/src/cli/subcommands.rs`, `crates/cuenv/tests/bdd/features/{help,errors}.feature` | Version, help, completions, defaults, path/package selection, text/JSON output, malformed arguments, missing task, exit-code propagation, piped output, cancellation; assert the current fail-fast contract of `cuenv web` | `CLI-*` P0; native parser tests remain primary |
| Environments and policies | `crates/cuenv/tests/bdd/features/env.feature`, `crates/cuenv/src/commands/export_tests.rs`, `schema/env.cue`, `schema/policy.cue` | Primitive, interpolated, multiline, Unicode, empty, and named-overlay values; `env print/load/status/inspect/check/list`; passthrough and `allowTasks` / `allowExec` / `allowInfrastructure` filtering | `ENV-*` / `POLICY-*` P0 |
| Secrets and redaction | `crates/secrets/**`, provider crates, `crates/cuenv/src/commands/export_tests.rs`, `schema/secrets.cue` and provider schemas | Fake resolver success/failure; missing credentials; denied consumer does not resolve; interpolation; raw stdout/stderr and generated files never contain a sentinel; provider errors are classified and redacted | `SECRET-*` P0 with fake providers; live providers P2 |
| Shell hooks and approval | `crates/cuenv/tests/hook_integration.rs`, `crates/cuenv/tests/exec_hooks_wait.rs`, BDD hook features, `crates/hooks/**` | Approve → enter directory → background hook → environment load → leave → re-enter; deny and changed config; failed/source-malformed/timeout/cancelled hooks; `exec` and `task` do not unexpectedly run shell hooks | `HOOK-*` P1 real shell/PTY; simulated BDD remains useful |
| Task discovery and parameters | `crates/cuenv/tests/task_exec_integration.rs`, task discovery and task-graph crates, BDD task feature | List/tree, nested names, labels, reusable/imported tasks, positional/named/bool parameters, invalid parameters, ambiguous/missing references, working-directory behavior | `TASK-DISCOVERY-*` P0 |
| Task graph execution | `crates/task-graph/**`, `crates/task-exec/**`, `crates/cuenv/tests/task_exec_support/**` | Dependencies, groups, ordered sequences, contributor injection, failure propagation, `continue-on-error`, concurrency caps, retries, timeout, SIGINT cleanup; use barriers and event order rather than elapsed-time guesses | Native graph/property primary; `TASK-GRAPH-*` P0/P1 |
| Outputs, captures, and cross-project references | `crates/cuenv/tests/cross_project_deps.rs`, `crates/cuenv/tests/task_exec_support/examples.rs`, `schema/tasks.cue` | Declared output projection; captures and output references; failed output transaction; imported project working directory; missing producer, path escape, duplicate destination, and mapped-output errors | `TASK-OUTPUT-*` P0; native extraction remains primary |
| Hermetic execution and local cache | `crates/task-exec/tests/sandbox_dir.rs`, `crates/task-exec/tests/cache_roundtrip.rs`, ADR-0008 | Cold run → warm hit → output deletion/restoration; input, command, environment, platform, tool, and backend invalidation; managed HOME/XDG; undeclared files absent; failed tasks do not publish bad outputs | `HERMETIC-*` / `CACHE-*` P0 |
| Remote cache and CAS | `crates/cas/**`, `crates/cas-remote/tests/reapi_roundtrip.rs`, `external_reapi.rs` | Local REAPI fixture read-through; missing/corrupt blobs; unavailable endpoint fallback; auth scoping/redaction; verify current read-only upload behavior rather than assuming writes | Native protocol primary; `REMOTE-CACHE-*` P1; external REAPI P2 |
| Tools, lockfiles, and runtimes | `crates/tool-*`, `crates/cuenv/tests/node_tool_integration.rs`, Nix runtime tests, `schema/tools.cue`, `schema/runtime.cue` | Lock → download → list → activate → exec; platform overrides; local archive/OCI fixtures; checksum/corrupt download; offline reuse; Nix/devenv; explicit limitation tests for partial/container/Dagger/OCI behavior | `TOOL-*` / `RUNTIME-*` P1; registries P2 |
| Services and readiness | `crates/services/**`, `crates/cuenv/src/commands/{up,down,ps,restart}.rs`, `schema/services.cue` | `up → readiness → ps → logs → restart → named down → full down`; port/HTTP/log/command/delay probes; task/service dependencies; crashes, watch/SIGHUP, stale sessions, cleanup, and process leaks | `SERVICE-*` P1; native probes remain primary |
| Images and build | `crates/cuenv/src/commands/build.rs`, image/runtime schemas, container examples | Dockerfile/Nix build canary with disposable inputs; output reference and multi-arch behavior are explicit; failures are actionable; do not treat command-vector tests as proof of delivered images | `IMAGE-*` P1/P2 |
| Sync, codegen, rules, owners, and formatting | `crates/codegen/**`, `crates/ignore/**`, `crates/codeowners/**`, `crates/editorconfig/**`, sync providers, `sync_scope_rules.rs` | Scoped/all-project generation; supported file types; `.gitignore`, `.dockerignore`, `.editorconfig`, CODEOWNERS; repeat sync no-op; drift detection with `--check`; malformed input and formatter propagation | `SYNC-*` / `CODEGEN-*` P0/P1; native emitters remain primary |
| VCS dependencies | `crates/cuenv/tests/vcs_subdir_e2e.rs`, VCS sync provider, `schema/vcs.cue`, VCS docs | Local Git remote/revision pin; sparse subtree; overlay preserves siblings; `vendor: false`; repeat sync/check; missing ref, traversal, and unsafe materialization rejection | `VCS-*` P0/P1 |
| CI compiler and contributors | `crates/ci/**`, CI emitters, `crates/cuenv/tests/contributor_integration.rs`, generated workflow and `sync ci` | Explicit provider opt-in; per-pipeline override; thin/expanded generation; matrix, artifacts, triggers, affected tasks, contributor stabilization, idempotent generation, and drift check; assert unsupported GitLab/Buildkite behavior | Native compiler primary; `CI-*` P0/P1; hosted workflow P2 |
| Changesets and release | `crates/cuenv/src/commands/changeset_picker.rs`, `release*.rs`, release schema/docs | Temporary Git repository → add/status/from-commits → dry-run version/prepare/changelog; tag-prefix rule; backend selection/order; dirty/unrelated files preserved; fake publisher failure; publication only in trusted canary | `RELEASE-*` P1; live publication P2 |
| Infrastructure | `crates/infrastructure/tests/provider_end_to_end.rs`, `crates/cuenv/tests/infrastructure_lifecycle.rs`, `flake.nix` infrastructure check, infrastructure docs | Preserve provider + sqld coverage; add CLI plan/apply/no-op/destroy, two-process lock contention, fencing, interrupt/recovery, environment identity collision, state list/locks/remove/recover/adopt, and non-TTY confirmation | Existing native suite primary; `INFRA-*` P1; real resources P2 |
| Events, JSON, TUI, and distribution | `crates/events/**`, trace acceptance, TUI sources, `crates/cuenv/tests/stress_tests.rs`, release artifacts | Event ordering/redaction; JSON schema; interactive task cancellation; released binary smoke on Linux x64/ARM64 and macOS ARM64; deep/wide graph and cold/warm evaluation behavior | Native/PTY primary; `EVENT-*` P1; `DIST-*` P2 |
| VS Code and Cuetty | `integrations/vscode/**`, VS Code docs, `apps/cuetty/**` | VS Code webview rendering and extension-host task/env/graph flows; Cuetty typing, resize, tabs, settings, and task launch only after a supported desktop/terminal driver is chosen | Native UI first; `UI-*` P2 |
| Unsupported and partial contracts | Matrix, status page, CLI/docs | Assert current rejection or limitation for Vault, GitLab, container runtime, `cuenv web`, unsupported CI matrix filtering, CUE publication, remote-cache write behavior, and other partial features. Never add a green happy path for schema presence alone | `LIMIT-*` P0 |

## Scenario catalog

These IDs are the first implementation slice. Each later schema-matrix row gets
one or more IDs in the same ledger; the IDs below are representative journeys,
not a claim that one test covers an entire domain.

### P0 offline and deterministic

1. `CLI-001` — version, top-level help, nested help, completions, and stable
   exit status.
2. `CLI-002` — JSON/text output, piped output, missing task/argument, invalid
   CUE, and exit-code mapping.
3. `EVAL-001` — valid project, selected package, nested package, unrelated
   package isolation, unknown field, and evaluation timeout.
4. `ENV-001` — primitive, interpolated, multiline, Unicode, empty, and named
   environment values through `print`, `list`, `status`, and `inspect`.
5. `POLICY-001` — consumer-specific filtering for task, exec, shell, and
   infrastructure access.
6. `SECRET-001` — synthetic fake secret resolution, denial without resolution,
   failure classification, and raw-output/file redaction scan.
7. `TASK-DISCOVERY-001` — task list/tree, labels, nested names, parameters,
   aliases, and missing/ambiguous references.
8. `TASK-GRAPH-001` — dependency order, parallel siblings, sequence, failure
   propagation, continue-on-error, and event order.
9. `TASK-OUTPUT-001` — captures, output refs, working directories, output
   projection, failed-output transaction, and cross-project mapping.
10. `HERMETIC-001` — host sentinels, explicit environment, managed HOME/XDG,
    declared inputs, undeclared files, and process cleanup.
11. `CACHE-001` — cold execution, proven warm hit, deleted-output restoration,
    invalidation by each key input, cache mode, and failed-result behavior.
12. `SYNC-001` — codegen/rules/VCS sync, `--check`, drift repair, scope, and
    repeat idempotence in a temporary Git repository.
13. `CI-001` — provider opt-in, pipeline override, generation, artifact/matrix
    mapping, contributor stabilization, and generated-file drift.
14. `LIMIT-001` — current explicit rejection for schema-only/unsupported
    features, including `cuenv web`, Vault, GitLab, and container runtime.

### P1 local integration and trusted CI

1. `HOOK-001` — real Bash/Zsh/Fish shell integration, approval, background
   completion, changed configuration, failure, timeout, cancellation, and
   re-entry.
2. `REMOTE-CACHE-001` — read-through and fault injection against a local REAPI
   fixture.
3. `TOOL-001` — local artifact lock/download/activate/exec, extraction and
   checksum failures, platform selection, and offline reuse.
4. `SERVICE-001` — service lifecycle, all readiness probes, logs, restart,
   watcher behavior, dependencies, and cleanup.
5. `VCS-001` — local Git sparse subtree, overlay, vendor behavior, revisions,
   and unsafe path rejection.
6. `RELEASE-001` — changesets, dry-run versioning, changelog, tag policy,
   backend ordering, and fake publisher failure.
7. `INFRA-001` — disposable provider/sqld plan/apply/state/destroy journey,
   lock contention, interruption, and recovery.
8. `EVENT-001` — machine-readable event stream, redaction, cancellation, and
   process outcome under a real child-process run.

### P2 canaries and UI exploration

1. `PROVIDER-001` — one disposable canary for each supported external secret
   provider and remote service; never a required untrusted-PR check.
2. `IMAGE-001` — Docker/Nix image build and output-reference delivery.
3. `DIST-001` — released artifacts on Linux x64/ARM64 and macOS ARM64.
4. `UI-001` — VS Code webview and extension-host user journey.
5. `UI-002` — Cuetty terminal journey only after an accessibility/desktop engine
   exists and its lifecycle contract is tested.
6. `AGENT-001` — bounded agent-driven exploration of docs/onboarding or
   terminal error recovery; deterministic assertions decide pass/fail.

## Proposed harness

The first implementation should be a self-contained package under
`tests/e2e/`, unless the implementation phase finds a stronger existing
workspace boundary. It should not silently change the root JavaScript
workspace.

```text
tests/e2e/
  package.json
  bun.lock                 # or the package manager lock chosen by the repo
  e2e.config.ts
  support/
    fixtures.ts
    cuenv-process.ts
    workspace.ts
    process-registry.ts
    polling.ts
    reports.ts
  fixtures/
    projects/
    git-remotes/
    artifacts/
  specs/
    cli/
    evaluation/
    environment/
    secrets/
    tasks/
    cache/
    tools/
    hooks/
    services/
    sync/
    ci/
    release/
    infrastructure/
```

The exact `e2e.config.ts` shape must be checked against the pinned package
before implementation. The intended configuration is:

- one engine-free `cli` target;
- zero model credentials for P0/P1 deterministic tests;
- explicit workers and zero retries for required checks;
- tags for feature (`tasks`, `services`), tier (`smoke`, `acceptance`,
  `stress`), dependency (`offline`, `nix`, `docker`, `provider-live`, `pty`),
  and behavior (`negative`, `security`, `parallel`);
- list, JUnit, and JSON/Markdown report output; and
- per-shard artifact names because upstream CI documentation says reports are
  not merged automatically.

### Fixture contract

`cuenv-process.ts` should accept an executable path and argument vector, never a
shell-interpreted command string, and return:

- exit code or terminating signal;
- stdout and stderr with byte/line limits;
- start/end timestamps and deadline state;
- the explicit environment names used by the child;
- owned process-group identifiers; and
- cleanup status and leaked-resource diagnostics.

The fixture must require an absolute `CUENV_TEST_BIN`. A missing binary is a
fixture failure, never a skipped passing test. The fixture should record the
selected binary version and source revision in each report.

Each attempt gets a new disposable workspace and explicit:

- `HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME`, and temp
  directories;
- Git identity/config where a repository is needed;
- ports allocated by binding rather than guessed static values;
- fake provider state and local artifact servers; and
- an environment allowlist, with host sentinels passed only by tests that
  exercise inheritance or passthrough.

On timeout or cancellation, the fixture must stop accepting operations,
terminate owned process groups, wait with a bounded escalation policy, close
sockets, and remove the workspace. Teardown must be idempotent and must fail if
an owned process remains. All polling must observe files, ports, events, or
structured state with a deadline; avoid fixed sleeps.

For secret scenarios, use long synthetic values and scan the raw child output
before framework reporting. Publish only sanitized artifacts. Never pass a real
provider credential to untrusted pull requests.

## CI and rollout

### Phase 0: feasibility gate

Implement only three tests:

1. `CLI-001` against the selected built binary;
2. one temporary schema-backed environment plus task;
3. a forced timeout with a grandchild process.

The gate passes only when collection, zero-model execution, output capture,
report generation, parallel isolation, missing-binary failure, process-group
cleanup, and workspace cleanup are all demonstrated. If the timeout test leaks
or the runner cannot reliably host an engine-free target, stop and reconsider
the framework before expanding the suite.

### Phase 1: offline PR smoke

Add the P0 catalog with deterministic assertions and zero retries. Run on the
existing Linux CI lane first, then add the macOS lane once the same fixtures
are proven portable. Required checks must fail on missing prerequisites and
unexpected skips.

### Phase 2: local integration

Add PTY shells, services, local tools, local Git remotes, REAPI fixtures,
release dry-runs, and infrastructure fixtures. Reuse the existing native
infrastructure and provider fixtures rather than duplicating their protocol
assertions in TypeScript.

### Phase 3: trusted canaries

Run external secret providers, registries, Docker/Nix image delivery, external
REAPI, hosted workflow checks, and release publication only in scheduled or
manual jobs with disposable accounts/resources. Keep credentials and model
keys out of untrusted pull requests.

### Phase 4: UI and agent lane

Use browser e2e for VS Code webviews where a browser surface exists. Establish a
desktop/terminal accessibility contract before adding a Cuetty custom engine.
Keep agent-driven exploration advisory until its cost, replay behavior, driver
fidelity, and flake rate are measured.

### Pilot commands

The pilot uses the root Bun workspace and requires an explicit absolute binary:

```bash
bun run typecheck:e2e
CUENV_TEST_BIN=/absolute/path/to/cuenv E2E_TELEMETRY_DISABLED=1 bun run test:e2e:list
CUENV_TEST_BIN=/absolute/path/to/cuenv E2E_TELEMETRY_DISABLED=1 bun run test:e2e
```

Use `E2E_TELEMETRY_DISABLED=1` in CI unless a deliberate telemetry decision is
made. Upload `.e2e/report.json`, JUnit, and sanitized artifacts on every
non-cancelled run. Keep separate artifact names for shards.

If implementation adds a Nix check, generated workflow, or CI task, define it
in `env.cue`, regenerate with `cuenv sync ci`, and apply the repository's full
flake gate before review/merge. Do not hand-edit generated workflows.

## Flake control and failure triage

- Keep required tests at zero retries. A diagnostic retry may be run manually,
  but a pass only on retry is tracked as flaky rather than converted to green.
- Prove cache hits with execution counters, nonces, or restored outputs; never
  infer them from elapsed time.
- Use barriers and markers to prove task overlap and concurrency limits.
- Parse JSON/events and assert named fields/codes. Use whole-output snapshots
  only for stable generated contracts.
- Record scenario ID, binary/version, platform, arguments, sanitized
  environment names, stdout/stderr, file differences, process outcome, and
  cleanup result.
- Classify failures as product assertion, fixture/precondition, cleanup leak,
  external provider, or agent/replay. Attach a native reproduction command for
  the first three classes.
- Keep external-network tests separate from offline contract tests so a
  registry or provider outage cannot obscure a product regression.

## Coverage ledger and definition of done

Maintain a ledger with one row for every schema-matrix definition and CLI-only
surface:

| Field | Requirement |
| --- | --- |
| Feature / definition | Exact matrix definition or CLI behavior |
| Scenario ID | Stable `DOMAIN-NNN` identifier |
| Support status | Implemented, partial, schema-only, legacy, or unsupported |
| Test layer | Native, e2e engine-free, PTY, provider canary, or UI |
| Oracle | Exact output/event/file/process/environment assertion |
| Fixture | Workspace, provider, service, cache, or external dependency |
| Tier/platform | P0/P1/P2 and OS/runtime lane |
| Gate | Required, scheduled, manual, or informational |
| Evidence | Last run, report, and reproduction command |
| Owner | Maintainer responsible for drift |

The plan is implemented when:

1. the Phase 0 gate passes without model credentials;
2. every matrix row and CLI-only feature has a test, existing native owner, or
   explicit limitation assertion;
3. P0 tests are deterministic, isolated, and pass with zero retries on each
   supported required OS lane;
4. raw secret-leak scans and process/workspace cleanup are enforced;
5. existing Rust/CUE/Go, BDD, and infrastructure gates remain intact;
6. CI reports and artifacts are retained on failure and unexpected skips fail;
7. docs and the schema coverage matrix are updated when implementation changes
   support status, schema, CLI, examples, or CI wiring; and
8. `cuenv task ci.schema-docs-check` passes, with the full root flake check
   used when the implementation triggers the repository's broad-risk or
   review/merge gate.

## Open decisions

- Should the root Bun workspace remain the long-term home, or should the suite
  move to a dedicated package once CI wiring begins?
- Should the pilot's pinned `e2e` 0.16.0 dependency be upgraded only with a
  deliberate API/reporter review?
- Which Linux and macOS runners can build and expose the exact `CUENV_TEST_BIN`?
- Which features are required in the first P0 set versus merely mapped to
  existing native tests?
- Which disposable provider, registry, Docker/Nix, and REAPI canaries are
  funded and maintained?
- Does "all features" include Cuetty desktop interaction, or only cuenv core
  and the VS Code integration?
