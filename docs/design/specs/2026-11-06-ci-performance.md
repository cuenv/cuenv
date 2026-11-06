# CI performance investigation

Measured on 2026-11-06 against the workflows and flake at
`9452a642b47dae8dc124843fdc0726cb7733e853` (0.68.0). The measurements describe
the baseline before the optimizations below.

## Implemented changes

- Expanded non-release Nix matrices execute their original Nix commands
  directly. They retain task metadata and declared outputs, preserve job
  dependencies and runner selection, and skip cuenv bootstrap and artifact
  downloads. Release matrices retain orchestration and bootstrap.
- Direct Nix execution inherits the runner's Cachix configuration, including
  its publication hook. Global task hermeticity remains unchanged.
- CI and generated Hestia maintenance use upstream Nix 2.31.2 through
  `cachix/install-nix-action@v31`, without installer OIDC. Buildkite's installer
  and profile sourcing use upstream Nix too. The unused hosted `flake-schemas`
  input has been removed from both the flake and lockfile. No active flake input
  or installer config uses FlakeHub.
- The repository's Cachix contributor installs its client with
  `nix profile install --option flake-registry '' --inputs-from . nixpkgs#cachix`, using the exact nixpkgs
  revision already in `flake.lock` instead of a separate mutable install expression.
- Native build/check toolchains use Rust's minimal profile with Clippy and
  rustfmt. The default development shell retains docs, analyzer, sources, and
  cross-platform targets. Cuetty's build toolchain is minimal too.
- Clippy's dependency derivation builds all-target metadata without linking
  release/test binaries. The CLI package's dependency derivation skips Rust
  tests; the installed CLI still has its smoke checks, and separate Rust test
  checks remain in the gate.
- BDD uses the `ci` profile with package-scoped matching dependency artifacts.
  Test-only dependency artifacts remain separate where scopes or profiles
  differ. Terminal Clippy, nextest, doctest, and BDD checks no longer install
  their Cargo target directories into the cache.

Regression tests cover direct matrix execution, preserved environment,
dependencies, working directory and outputs, retained Cachix setup, skipped
cuenv setup and its dependent phases, and release bootstrap. Live speedup and
cache-publication results must be recorded from subsequent CI runs; the
historical timings below are not measurements of the changed workflow.

On x86_64 Linux, `nix path-info --json --closure-size` measured the original
development toolchain closure at **2,633.8 MiB** and the new native build
toolchain at **1,131.6 MiB**, a reduction of **1,502.2 MiB (57%)**. These are
uncompressed store closure sizes, not network download sizes or CI elapsed-time
measurements. The full development toolchain remains available in the default
development shell.

## Local validation

The native CLI package builds and passes its installed-binary smoke checks.
`cuenv fmt --fix`, `cuenv sync ci --check`, Buildkite sync check, and
`cuenv task ci.schema-docs-check` have been exercised. Generated workflow
inspection confirms both Darwin checks have no bootstrap job, artifact
setup, or unresolved job dependencies. Active build/workflow configuration
contains no FlakeHub integration, and unrelated lockfile input revisions are
unchanged. Workspace all-target Clippy passes with `-D warnings`.

The locked Cachix client resolves to 1.9.1 and provides the daemon run, push,
and stop interface used by `cachix/cachix-action@v17`. Global registry lookup
is disabled during its installation. The full root flake gate is run for the
Nix expressions and generated workflow changes; its final result belongs in
the PR validation record. Local cache publication cannot be exercised because
this execution environment blocks the project's Cachix host, so a warm-cache
CI comparison remains necessary to measure elapsed-time improvement.

## Measured bottleneck

macOS nextest determines the completion time in all four successful CI runs
sampled. GitHub's job/step timestamps and job logs provide the timings below.
Workflow elapsed time includes queuing and gaps between dependent jobs.

| Run | Event | Workflow elapsed | macOS bootstrap job | macOS nextest job | macOS nextest task step |
| --- | --- | --- | --- | --- | --- |
| [42259511820](https://github.com/cuenv/cuenv/actions/runs/42259511820) | PR, infrastructure write-only arguments | 21m 10s | 5m 37s | 14m 49s | 9m 01s |
| [42207554564](https://github.com/cuenv/cuenv/actions/runs/42207554564) | PR, release 0.68.0 | 16m 20s | 7m 04s | 8m 42s | 7m 29s |
| [42203864570](https://github.com/cuenv/cuenv/actions/runs/42203864570) | PR, infrastructure secret arguments | 13m 35s | 4m 29s | 8m 30s | 7m 35s |
| [42209698594](https://github.com/cuenv/cuenv/actions/runs/42209698594) | Push, 0.68.0 | 12m 26s | 1m 06s | 9m 22s | 8m 15s |

For the latest PR, Linux jobs finished by 17:19:15, 6m 40s after workflow
creation. The macOS nextest job finished at 17:33:44. Its actual test execution
took **45.554 seconds** for 4,223 passing tests; Linux nextest ran 4,229 tests
in 8.361 seconds. Test counts differ by platform and source revision.

## 1. Execute non-release Nix matrix tasks directly

The baseline generated macOS jobs followed this chain:

1. Build the checkout's release cuenv package on macOS and upload its binary.
2. Start the macOS check jobs, install Nix and Cachix, and download that binary.
3. Realize the cuenv package again on each fresh runner for its runtime closure.
4. Run `cuenv task darwin.nextest --skip-dependencies`, which ultimately runs
   `nix build .#checks.aarch64-darwin.cuenv-nextest`.

The Linux Nix checks already execute directly and skip cuenv bootstrap. The
selection is in
[`simple_job_execution`](../../../crates/cuenv/src/commands/sync/functions/github.rs).
Matrix emission previously always used orchestrated execution in
[`build_matrix_jobs`](../../../crates/github/src/workflow/jobs.rs).
[`create_synthetic_matrix_task`](../../../crates/cuenv/src/commands/sync/functions/github/matrix.rs)
also previously created a task with an empty command rather than retaining the original
Nix command.

The implementation applies the existing non-release direct-Nix rule to matrix jobs, retaining the
original task's command, environment, dependencies, and outputs during matrix
expansion. Only orchestrated jobs consume a cuenv bootstrap artifact.
Runner selection and release orchestration remain intact. Regenerate
workflows with `cuenv sync ci`; do not edit the generated YAML by hand.

On the latest PR this would let macOS checks begin approximately six minutes
earlier. That is an overlap opportunity, not a measured speedup: independent
runner availability, setup, and cache behavior still determine the result.

## 2. Restore cache publication from macOS checks

The macOS nextest logs show the **same dependency derivation** being rebuilt
in the main and latest PR runs:

```text
/nix/store/0l0rrkjds80fnf9cz63l0lzai8drsbcn-cuenv-deps-0.68.0.drv
```

In [job 127866103038](https://github.com/cuenv/cuenv/actions/runs/42209698594/job/127866103038),
the dependency build ran from 11:26:31 to about 11:32:43. In
[job 128038636784](https://github.com/cuenv/cuenv/actions/runs/42259511820/job/128038636784),
it ran from 17:26:41 to about 17:32:04. Neither macOS check's Cachix daemon
reported any paths pushed. Direct Linux checks and the macOS bootstrap job did
report successful pushes.

Cachix exports `NIX_USER_CONF_FILES` pointing at its temporary Nix configuration.
The baseline Darwin task definitions had no explicit passthrough for this variable.
Hermetic execution calls `env_clear()` in
[`executor.rs`](../../../crates/task-exec/src/executor.rs); the execution
environment in [`environment.rs`](../../../crates/core/src/environment.rs)
only includes declared variables and passthrough. The host configuration that
registers the Cachix post-build hook therefore does not reach the nested Nix
invocation. A configured cache action does not establish that this child build
can publish its outputs.

Direct execution addresses this environment boundary as well as bootstrap
serialization. Verify publication on the first changed run, then use a fresh
runner to verify substitution of the dependency output on the next run. Check
the realized output path as well as its derivation path. The repeated build is
observed; the full time saving needs those CI runs to establish it.

Do not pass the host `HOME` into hermetic tasks or weaken task hermeticity
globally to repair this CI-specific boundary.

## 3. Remove cache-service failures from tool installation

The latest macOS nextest job spent **4m 41s** in Setup Cachix. Its installation
command used `nix-env ... -f https://cachix.org/api/v1/install` and retried HTTP
500 responses from `edge.cache.flakehub.com`. In the corresponding docs-preview
job, Cachix setup took **5m 11s** with the same error pattern. Other sampled
macOS jobs completed Cachix setup in a few seconds.

Investigate the failing substituter separately from project compilation.
Preinstall a pinned Cachix client in the runner image, or install it using a
known working substitute path. Preserve the cache publication hook. Warm
macOS tool/store state where supported; the current Namespace `/nix` volume
action deliberately runs on Linux only.

The multi-minute setup delay is a service failure in this sample, not a
constant cost or a guaranteed saving on every run. Adding another cache layer
without validating hits and publication would not address the evidence.

## 4. Reduce compilation and transfer costs after cache repair

- `bdd-check` previously used the release profile. The latest Linux BDD build
  spent 3m 32s compiling with that profile; the job took 6m 29s overall. Give
  test-only checks a lightweight profile with matching dependency artifacts,
  while keeping release packaging settings. Benchmark BDD execution to ensure
  lower optimization does not materially increase runtime.
- Audit dependency-only derivations by scope, features, profile, and bridge
  inputs. Several distinct artifact sets exist intentionally; share them only
  where those settings match. First confirm stable dependency derivations can
  actually be substituted before changing the cache keys.
- The baseline Linux nextest job spent 72s in the Cachix post step. Its check
  retained Cargo artifacts, unlike infrastructure E2E. The change applies
  `doInstallCargoArtifacts = false` for terminal checks whose artifacts have no
  consumers. Keep reusable dependency artifacts and the published CLI package.
- Every check task declares `_flakeInputs`, including Cuetty files. Keep
  accurate flake source declarations, but avoid dispatching every check for an
  app-only change when narrower provider/job selection can prove the required
  checks still run. This needs dependency-aware filtering and coverage tests.

## Validation and success criteria

Implement direct matrix execution and cache publication first, then measure
before changing test coverage or adding test shards. Preserve macOS tests,
Clippy, the Linux suites, and release packaging.

Regression tests should cover direct Nix matrix commands, preserved task
metadata and dependencies, runner selection, absence of unnecessary bootstrap
dependencies, and continued bootstrap/artifact handling for orchestrated and
release jobs. Run formatting, workflow sync validation, the schema docs check,
and the full root flake gate when making that CI behavior change.

Use a changed-source run and a repeat on a fresh runner. Record queue time,
setup, dependency substitutions/builds, workspace compilation, tests, and
cache publication separately. Confirm Cachix reports pushes and the second
runner substitutes the dependency output. Track PR median and p95 across
multiple runs; do not claim savings by adding overlapping job durations.

An initial target is to bring ordinary PR CI closer to the observed Linux
completion range of roughly 6–9 minutes. This is a target, not a benchmark of
an implemented optimization. Cache-service failures and cold dependency
changes need separate measurements.
