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
  or installer config uses FlakeHub. Namespace preparation transfers restored
  root-owned Nix state to the runner on Linux without systemd, matching the
  install action's single-user mode; daemon installations retain their ownership.
- When Nix is not already on `PATH`, source-install scripts and downloaded
  bootstrap closure setup source the daemon profile or the upstream single-user
  profile. Fresh Namespace stores must not depend on a legacy daemon profile.
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
- Test dependency derivations compile their required targets in one
  `cargo test --no-run` pass. Doctest preparation uses library targets, while
  BDD and infrastructure preparation select the same targets as their checks.
  Infrastructure's auto-discovered integration targets receive explicit dummy
  source files because Crane only stubs targets declared in Cargo.toml.
  Production package dependencies use one `cargo build` pass. Clippy retains
  its separate metadata-only preparation.
- Nix test checks and their dependency derivations share `ci` profile settings
  with debug information disabled, reducing linker output and artifact size.
- Cuetty's test derivation runs its test-profile compilation directly rather
  than first building a release application. Its package check still builds
  and validates the installed release executable.
- BDD scenario failures now fail the runner. The Nix BDD check also runs an
  intentional failing scenario and verifies a nonzero exit, so a fast green
  check establishes that the failure path works too.

Regression tests cover direct matrix execution, preserved environment,
dependencies, working directory and outputs, retained Cachix setup, skipped
cuenv setup and its dependent phases, and release bootstrap. Live speedup and
cache-publication results from the changed workflow are recorded below.

The first changed-source run, [42277205456](https://github.com/cuenv/cuenv/actions/runs/42277205456),
passed both macOS checks. macOS nextest took **9m 50s** after starting, including
**31s** for Nix installation, **24s** for Cachix setup, and **53.803s** for 4,226
passing tests. Its Cachix daemon published the minimal Rust toolchain,
`cuenv-deps-0.69.0`, and `cuenv-nextest-0.69.0`, confirming publication from the
direct check. The runner queued for **8m 26s** before starting. Linux jobs
exposed the restored root-owned store's incompatibility with single-user Nix;
the Namespace preparation above corrects that failure. This partial run is
not a successful end-to-end timing comparison.

The corrected run, [42281253736](https://github.com/cuenv/cuenv/actions/runs/42281253736),
passed all **11 main CI jobs in 9m 27s**, compared with the baseline's 21m 10s.
macOS nextest completed in **3m 57s**, including 33s for Nix installation,
39s for Cachix setup, 29.82s of workspace compilation, and 43.778s for 4,226
passing tests. Its fresh runner substituted the exact dependency output
published by the first run:

```text
/nix/store/11hj7qqwgiq9mzmimjv7r4ws7b9997bn-cuenv-deps-0.69.0
```

macOS Clippy also substituted its dependencies and compiled in 14.44s.
Linux BDD passed all 19 implemented scenarios (16 skipped) and 152 implemented
steps. The docs preview passed; the separate sync workflow found an outdated
runtime digest, corrected with `cuenv sync lock`. These observations compare
different source revisions and cache/queue states; they do not establish a
median or p95 improvement.

## Remaining cost found in the changed-source run

Linux nextest became the critical job in run 42281253736. Its dependency
derivation used Crane's default three-pass preparation:

| Command | Elapsed |
| --- | --- |
| `cargo check --profile ci --locked --workspace --all-features --all-targets` | 2m 27s |
| `cargo build --profile ci --locked --workspace --all-features` | 2m 34s |
| `cargo test --profile ci --locked --workspace --all-features --no-run` | 2m 04s |

The actual workspace test compilation then took 48.18s and all 4,232 Linux
tests ran in 14.863s. The follow-up removes separate metadata/build passes
from test preparation and disables Nix CI debug information. The 9m 27s
measurement predates this change; the follow-up measurements are below.

For the same local 0.68.0 dependency set and compiler, the compressed BDD
dependency archive fell from **1,141,528,573 bytes to 480,432,416 bytes**
(**57.9% smaller**) after narrowing its targets and disabling Nix CI debug
information. This is an artifact-size measurement, not a CI elapsed-time claim.
The nextest dependency archive fell from **1,283,172,601 bytes to 504,605,830
bytes** (**60.7% smaller**) with the same local version and compiler.

In [run 42291426496](https://github.com/cuenv/cuenv/actions/runs/42291426496),
Linux nextest's dependency preparation fell from **7m 06s to 3m 34s**.
The workspace then compiled in 28.04s and all 4,232 tests passed in 8.716s.
That partial run exposed the fresh single-user bootstrap and dummy-target
issues corrected above; it is not a successful end-to-end workflow benchmark.

The corrected second pass, [run 42297118480](https://github.com/cuenv/cuenv/actions/runs/42297118480),
passed all **11 main CI jobs in 5m 57s**, compared with the original **21m 10s**.
The separate sync and docs-preview workflows also passed, giving **13 green
checks** on commit `43a4e179359a33ead87e88f29ab269adabc7fd47`. macOS nextest
substituted the new dependency archive, compiled the workspace in 36.28s, and
passed 4,226 tests in 56.333s. Its 5m 31s job duration included 37s for Nix
installation, 34s for Cachix setup, and toolchain/dependency downloads.
These individual runs have different source revisions and cache/queue states;
they establish working publication and substitution, not a median or p95.

The local full gate also showed Cuetty compiling a release application for
5m 30s in its package check and repeating that release build before its test
check invoked `cargo test` in the dev profile. The test derivation now skips
that unused release build. The release package and all-target test command
remain separate checks.

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
unchanged. Workspace all-target Clippy passes with `-D warnings`. Regenerate the
runtime digest with `cuenv sync lock` when changing the flake, then run
`cuenv sync --check` as well as the provider-specific workflow checks.

The locked Cachix client resolves to 1.9.1 and provides the daemon run, push,
and stop interface used by `cachix/cachix-action@v17`. Global registry lookup
is disabled during its installation. The full root flake gate is run for the
Nix expressions and generated workflow changes; its final result belongs in
the PR validation record. Local cache publication cannot be exercised because
this execution environment blocks the project's Cachix host. The two CI runs
above verify publication and subsequent substitution on fresh macOS runners.

The initial local full gate exhausted its 32 GiB filesystem while two source
snapshots were building; its failures are not passing gate evidence. The gate
is rerun serially after reclaiming obsolete artifacts.

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
expansion. Only orchestrated jobs depend on the cuenv bootstrap job.
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
Clippy, the Linux suites, and release packaging. macOS checks have since moved
off pull requests; see the follow-up below.

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

## Follow-up: release bootstrap and pull request scope (2026-11-07)

### Removing the bootstrap artifact

[Release run 42322678159](https://github.com/cuenv/cuenv/actions/runs/42322678159)
failed at "Add cuenv to PATH" because `cmp` found two different binaries for one
store path. `cuenv-ci`'s `checks.cuenv` and the release's `build.cuenv` built
`/nix/store/mslxzdrd14bqqjjl1ssviy9jjipgkkj9-cuenv-0.70.1` concurrently. Cachix
kept the first push, so consumers realized that copy and compared it with the
artifact from the second build. The `.#cuenv` derivation is not bit-reproducible.

The artifact dates from #436, when the flake package was a portable binary that
consumers could run without Nix. `bf730d14` made the package Nix-native, so every
consumer already had to run `nix build .#cuenv` for the runtime closure. From then
on the artifact only supplied the bytes for the comparison. #575 changed how that
step finds Nix and did not depend on the artifact.

Consumers now put `$cuenv_package/bin` on `PATH`. No compilation returns:
consumers ran the same `nix build` before this change, and `build.cuenv` remains
so that they substitute the package rather than compile it. Without a working
binary cache each consumer compiles, as it did before. The artifact had no other
reader: `cargo.build`, `docs.build`, `docs.deploy`, `publish.cue`, `publish.github`,
and `publish.homebrew` only found `cuenv` through `PATH`, and published binaries
come from `cargo.build`'s own outputs.

Removing the transfer is a correctness change, not a measurable speedup: in the
failed run "Download cuenv" took about a second and "Upload cuenv" two to four.

### Why two builds of `.#cuenv` differ

`cranelift-codegen` includes generated code from `OUT_DIR`, so the published
binary holds seven absolute paths such as
`/nix/var/nix/builds/nix-551-62768335/source/target/release/build/cranelift-codegen-<hash>/out/settings.rs`.
CI builds run in a randomized directory, so each separate build of `cuenv-deps`
embeds a different path. In the failed release, `checks.cuenv` built it in
`nix-551-62768335` and `build.cuenv` in `nix-581-2191724202`. The lengths differ,
which would also move later data and explain why the first difference is in
`.rela.dyn`.

Rebuilding `cuenv` locally on Cachix's `cuenv-deps` reproduced Cachix's binary bit
for bit, even though the rebuild ran in `/build`, so `cuenv` does not embed its
own build directory. Rebuilding `cuenv-deps` gave different output. Attempt 2
replaced the failing artifact, so the path inside that binary could not be read
directly. Randomized directories appear in CI logs from before #575, so #575 did
not introduce them; the `cmp` from `bf730d14` is what turned them into a failure.

Reproducibility is not changed here. Applying `--remap-path-prefix` for the build
directory to the Rust builds is the likely fix, and would change store paths.

### Pull request scope

`darwin.clippy` and `darwin.nextest` moved from the `ci` pipeline to a `darwin`
pipeline that runs on pushes to `main` and on `workflow_dispatch`. Those two jobs
were the only macOS or ARM jobs on pull requests, so pull requests now run only
`x86_64-linux` jobs.

The cost is that a macOS regression is first reported after merge. `cuenv-darwin`
is part of a green `main`, and `gh workflow run cuenv-darwin.yml --ref <branch>`
checks a branch before merge. Pull requests also no longer publish macOS
dependencies to Cachix, so the first `main` push after a dependency change pays
for that build.

In [PR run 42322944747](https://github.com/cuenv/cuenv/actions/runs/42322944747),
the two macOS jobs ran for 3m 19s and 6m 38s, but the run finished 5m 47s after
them, on `checks.doctest`. That run's elapsed time was set by when Linux jobs
started, not by macOS. Expect saved macOS runner time and no dependence on macOS
capacity; a change in elapsed time needs measuring across runs, as above.

### Cross-compiling linux-arm64

Linux ARM was the slowest release path. In attempt 2 of the same release, with
`cuenv` already cached, `cargo.build` compared as follows:

| Phase | Linux ARM (8x16) | Linux x86 (8x16) | macOS ARM (6x14) |
| --- | --- | --- | --- |
| Setup and Go bridge | 4m 14s | 1m 28s | 2m 36s |
| Dependencies | 2m 06s | 48s | 1m 01s |
| Final crates and LTO link | 7m 32s | 2m 36s | 2m 06s |
| Whole job | 14m 03s | 4m 58s | 5m 51s |

In attempt 1, with nothing cached, `build.cuenv` also took 14m 07s on ARM before
`cargo.build` could start, and the release binaries were ready after 29m 30s.
The logs do not separate the ARM hardware from the toolchain as the cause. macOS
is the fastest compile, so ARM itself is not the reason.

`cargo.build` already targets glibc 2.17 through `cargo-zigbuild`, so `linux-arm64`
now maps to the x86 runner in `runners.arch`. That also removes the ARM
`build.cuenv` job, because no release job uses the ARM runner.

The smoke test runs under `qemu-aarch64` instead of on ARM hardware, so a problem
that only the cross-built binary hits on real hardware could go unnoticed. Two
things limit that: the `arm` pipeline builds and smoke-tests the flake package on
real ARM after every merge, and it also keeps publishing that package to Cachix for
flake users, which the ARM `build.cuenv` job used to do. The manual `release-dry-run`
pipeline runs the three binary builds on any branch, publishing nothing.

### Running `cargo.build` directly

macOS's `build.cuenv` still cost 5m 37s on a cold release, only to run `cargo.build`,
which needs nothing from cuenv: it runs a Nix command, and its script copies its
sources to a clean workspace and unsets inherited build settings.

Tasks can now set `ci: github: execution: "direct"`, and `cargo.build` does. Its
three jobs start immediately, so the only `build.cuenv` left is the x86 one the
publishing jobs need. They also no longer receive `OP_SERVICE_ACCOUNT_TOKEN`, so the
jobs producing release binaries hold no 1Password token. `release-dry-run` now runs
the same command as a release, rather than a direct version of it.

This is an explicit opt-in rather than a rule for every release Nix task. A release
task can depend on secrets from its pipeline's `environment`, and running it directly
would silently drop them. Compilation rejects the setting when a task's own `env`
needs cuenv, but it can't see which pipeline environment variables a script reads,
so the opt-in stays the author's decision.

### Measured releases

Time from the first job until all three release binaries were built:

| Release | Binaries built | Slowest build job | Longest wait for a runner |
| --- | --- | --- | --- |
| 0.70.1, attempt 1 (nothing cached) | 29m 38s | linux-arm64, 14m 29s on ARM | not measured |
| 0.70.1, attempt 2 (cached) | 14m 36s | linux-arm64, 14m 03s on ARM | not measured |
| [0.70.2](https://github.com/cuenv/cuenv/actions/runs/42363040694), cross-compiled ARM | 22m 18s | linux-arm64, 14m 16s on x86 | 3m 43s |
| [0.71.0](https://github.com/cuenv/cuenv/actions/runs/42369534144), direct `cargo.build` | 11m 56s | darwin-arm64, 6m 15s | 7m 04s |

In 0.70.2 the ARM job's Cargo build took 3m 42s, but `cuenv task` spent 8m 55s
before starting `nix develop`, with nothing logged. The same step took 11 to 80
seconds for every target in 0.70.1, and 11s for x64 in 0.70.2. The cause was not
found; 0.71.0 no longer runs `cargo.build` through `cuenv task`.

In 0.71.0 the build jobs ran for 4m 52s (x64), 5m 23s (ARM on x86) and 6m 15s
(macOS), so with free runners the binaries would have been ready after about six
and a half minutes. Over half of the 11m 56s was spent waiting for x86 runners.

### Runner contention at release time

Cross-compiling ARM moved a release build onto the x86 pool that pull request CI
uses. Each release commit also starts `cuenv-ci` on `main`, and it changed every
workspace version in `Cargo.lock`, which conflicted with the open Dependabot Cargo
updates. Dependabot rebases a pull request whenever it conflicts, so #541 and #543
were force-pushed within three minutes of each release commit for 0.70.0 through
0.71.0, and their CI took x86 runners while the release builds waited.

`.github/dependabot.yml` now sets `rebase-strategy: "disabled"` for every
ecosystem. Dependabot pull requests are updated with `@dependabot rebase` when they
are about to be merged. A separate runner profile for releases would also remove
the wait, but it has to be created in Namespace first.

### Covering every flake check

`cuenv-ci` named nine of the fifteen `x86_64-linux` flake checks. The release commit
for 0.70.2 left `apps/cuetty/Cargo.lock` pinning `cuengine` 0.70.1, which only the
local `nix flake check` caught, because no CI job built the cuetty checks.

`checks.cuetty` now builds `cuetty`, `cuetty-test`, `cuetty-clippy` and `cuetty-fmt`
in one job, so a pull request takes one more runner rather than four.
`cuenv-fake-terraform-provider` and `cuenv-numeric-interop` were already built,
because `cuenv-infrastructure-e2e` is built from both; that job now names them, so
they stay covered if the suite stops using them. macOS still runs only
`cuenv-clippy` and `cuenv-nextest`.

The `cuenv` check has no job of its own. `checks.x86_64-linux.cuenv` and
`packages.x86_64-linux.cuenv` are the same derivation, and its smoke test
(`cuenv --version`, `env print` on `examples/env-basic`) is an install check
inside it. The `sync-check` pipeline bootstraps this repository's own cuenv
(`config.ci.cuenv: {source: "nix", version: "self"}`) with `nix build .#cuenv`,
so that step builds and checks it, and `cuenv-sync-check` triggers on every path
`cuenv-ci` does (its path list differs only by the workflow's own file, which
cannot change the package). See the 2026-11-12 follow-up for the cost this
removes.

## Follow-up: development shell and shared test artifacts (2026-11-12)

Measured at 0.73.1 on one 32-core x86_64 Linux machine. Nix builds ran with only
`cache.nixos.org` as a substituter, so every cuenv derivation was built locally.
Each figure is a single run.

### Development shell

`cuenv exec -- true` took 2.5 to 2.8 seconds. The shell hook of the default
development shell runs on every shell entry and every `cuenv exec`, and it ran
`chmod -R +w target` to make the copied CUE bridge writable. On a `target/` of
604,368 files that command alone took about 2 seconds. The hook now copies the
bridge only when the built bridge changed, a copy is missing, or a Go source is
newer than the copy, and makes only the copied files writable. The same command
now takes 0.57 to 0.62 seconds: 0.35 seconds of CUE evaluation and about 0.2
seconds for the hook.

sccache served 654 of 654 cacheable compilations from its cache in a
`cargo test -p cuenv --no-run`, but its cache sat at the 10 GiB default limit.
The shell now defaults `SCCACHE_CACHE_SIZE` to 40G.

The dev profile no longer emits debug information for dependencies. After
touching `crates/cuenv/src/lib.rs`, `cargo test -p cuenv --no-run` fell from
5.70s to 4.59s, and the CLI's test binaries from about 506 MB to about 300 MB.
Backtraces keep line numbers for workspace code only. The Nix checks already
build without debug information.

### Shared test dependency artifacts

| Check | Before | After | Change |
| --- | --- | --- | --- |
| `cuenv-doctest` | 327s | 33s | Uses the `ci` profile and the dependency artifacts of `cuenv-nextest` instead of its own in Crane's default release profile |
| `cuenv-nextest` | 282s | 39s | Takes `CUENV_EXECUTABLE` from the `ci` binary it builds rather than from the release package |
| `cuenv-bdd` | 112s | 29s | Workspace scope, so it reuses the same dependency artifacts |

"Before" includes the dependency derivation only that check used, and for
`cuenv-nextest` the release package (47s of dependencies and 3m 09s for the
workspace with fat link-time optimization, since replaced by thin LTO below).
"After" excludes the shared dependency derivation, which took 82s and is built
once for all three. The same 37 doctest targets, 4,510 tests and 36 scenarios
pass before and after. The root flake now has four dependency derivations rather
than six.

Two experiments were measured and not adopted:

- Building `ci` dependencies without optimization (`[profile.ci.package."*"]
  opt-level = 0`) cut the dependency build from 82s to 44s but raised the
  nextest check from 39s to 52s, because the tests ran in 16.6s instead of 7.9s.
  The dependency build is cached and the check runs on every change.
- Giving `cuenv-infrastructure-e2e` the shared artifacts cut its cold build from
  119s to 47s, but its package scope resolves features differently, so 13
  third-party crates recompiled on every run and the check's own compilation
  rose from 23.5s to 34.7s. It keeps its own dependency derivation.

### Version-independent dependencies and a decoupled Go bridge

A release commit changed the workspace version, and the version reached every
dependency derivation twice: through the derivation name, and through Crane's dummy
source, which keeps each workspace member's version in `Cargo.toml` and
`Cargo.lock`. The release commit for 0.73.1 therefore rebuilt every dependency on
every platform, and its CI took 14m03s against 1m26s for the commit before it.
The `libcue-bridge` derivation took the version too.

The four `buildDepsOnly` derivations now build a stand-in workspace that carries
the constant version `0.0.0`:

- The source passed to them leaves out `Cargo.lock`, because Crane copies the file
  it finds there into the dummy source, which would bring the release number back.
  The lockfile is passed as `cargoLock` and `cargoLockContents` instead: the same
  text with the `version` of every package that has no `source` set to `0.0.0`.
- Crane's `cleanCargoToml` is overridden in a derived `craneLib` to set the package
  version, `workspace.package.version` and the version of every dependency that
  has a `path` to the same constant, so the manifests, the lockfile and the
  workspace dependencies stay consistent and `cargo --locked` accepts the result.
- Their `version` is the constant too, so the derivation names no longer change.

Third-party crates keep their fingerprints because those do not include the
versions of the crates that depend on them; only the workspace's own crates are
compiled again by the consuming check, as before.

Crane replaces every build script in the dummy source with an empty one (the
generated `Cargo.toml` points `package.build` at it), so the dependency
derivations no longer receive `CUE_BRIDGE_PATH`, `ONEPASSWORD_WASM_PATH`, Go, the
CUE tool, git or Zig. Only `pkg-config` stays, for third-party build scripts.

The bridge had two further problems. Its source was the whole `crates/cuengine`
directory, so an edit to any Rust file, benchmark or the changelog rebuilt it, and
its check phase ran `go vet` and the full Go suite, so every Rust derivation waited
for the Go tests. Now:

- `cue-bridge` has the constant version `0.0.0`, builds from the Go files,
  `go.mod`, `go.sum`, the C and assembly sources and headers in the module
  directory, and `testdata/` (the Go tests read only `large_project.json`), and
  has no check phase. `vendorHash` is unchanged.
- `go vet ./...` and `CUENV_FROZEN_SCALING=1 GOMAXPROCS=4 go test -timeout 30m
  ./...` run in the new flake check `cuenv-cue-bridge-test`, which reuses the
  bridge's vendored modules. Nothing depends on it. It runs on every platform that
  used to run the suite inside the bridge: the `checks.clippy` job builds it for
  `x86_64-linux`, `darwin.clippy` for `aarch64-darwin` and `arm.cuenv` for
  `aarch64-linux`, so every `x86_64-linux` flake check is still covered by a job,
  the Go suite still runs on macOS and ARM, and no runner was added.

Measured on the same 32-core x86_64 Linux machine as above, single runs, with
Cachix disabled: after a simulated release bump from 0.73.1 to 0.73.2 the
dependency and bridge derivation paths were unchanged, and `cuenv-nextest`
rebuilt alone in 36.7s with 4,510 tests passing. Before the change the same bump
also rebuilt the shared test dependency derivation (82s) and the bridge together
with its Go suite (the suite alone ran in 46s).

### Shared dependency artifacts for the cuetty checks

`apps/cuetty` is a separate Cargo workspace of about 790 crates. Its package, test
and clippy checks were three independent `buildRustPackage` builds that shared
nothing, so every change under `crates/` (their source held all of it, plus
`examples`, `contrib` and the agent skills) recompiled every third-party crate
three times. The Rio git dependency was also cloned while evaluating the flake, on
every fresh runner. `apps/cuetty/nix.nix` now builds with Crane, as the root
workspace does:

- Three `buildDepsOnly` derivations compile third-party crates once per profile and
  command: `cuetty-deps` (release, `--bin cuetty`), `cuetty-test-deps` (dev profile,
  `cargo test --no-run` plus the doctest pass, with the checks' own flags) and
  `cuetty-clippy-deps` (the same `cargo clippy` command as the check). The checks
  reuse them, so a source change recompiles cuetty, the `cuengine` crate and the
  stacksafe adapter, and nothing else.
- The dependency derivations are version independent in the way described above,
  through the same `cleanCargoToml` override and lockfile pinning, now shared with
  the root flake as `pinDepsVersions` and `pinLockVersions`. Only `cuengine`'s
  version is pinned: cuetty's own and the stacksafe adapter's stay, because the
  adapter's `0.1.x` version is what lets the `[patch.crates-io]` entry satisfy
  GPUI. A simulated release bump left all three dependency derivation paths
  unchanged.
- The stacksafe adapter (`apps/cuetty/compat/stacksafe`) is a workspace member that
  GPUI depends on, so the dependency derivations carry its real manifest and
  sources instead of Crane's stub. Cargo fingerprints workspace members under the
  compiler wrapper in use, so the clippy dependency derivation runs `cargo clippy`
  rather than `cargo check`; with a plain check, the consumer rechecked the
  adapter, GPUI and `gpui-component`.
- Cargo runs from the repository root against `apps/cuetty/Cargo.toml` with
  `CARGO_TARGET_DIR=target`, which is where Crane unpacks the artifacts. The
  release dependency derivation and the package both run `cargo auditable build
  --release --target <host triple>`, as nixpkgs' `buildRustPackage` does, so the
  binary still carries its `.dep-v0` audit section (9,079 bytes, as before) and the
  two derivations share artifacts; the command must stay identical in both. The
  package installs `target/<triple>/release/cuetty` itself, because Crane's
  build-log install runs `cargo metadata` at the root, whose workspace lists crates
  this source omits. The desktop entry, icon, `patchelf` RPATH, install check and
  `meta` are unchanged, and the installed file list, RPATH and runtime references
  match the previous build. The only difference found is the binary's size, 51.57 MB
  against 51.14 MB, from the Crane vendor paths it embeds.
- The source of the Rust checks is the app (without `nix.nix`), the root
  `Cargo.toml` and `clippy.toml`, `crates/cuengine` (manifest, build script, `src`,
  the declared benchmark file and the non-test Go files the build script
  fingerprints) and the CUE the repository-catalog test evaluates: `env.cue` and
  the `nix/source-files.json` it embeds, `cue.mod/module.cue`, `schema/`, and the
  `codecov`, `contributors` and `nix` packages of `contrib/` that `env.cue`
  imports. An edit to any other crate, to `examples/`, to the other `contrib/`
  packages or to the agent skills no longer changes the three derivations
  (checked with `nix eval`). `cuetty-fmt` keeps every crate's Rust because
  `cargo fmt --all` follows path dependencies into the root workspace.
  `nix/source-files.json` and `env.cue` needed no change: every file above is
  already listed there, and `flake-source-check` passes.
- Rio is vendored through Crane with an `outputHashes` entry keyed by the lockfile's
  `source` string, a fixed-output fetch that substitutes from a binary cache,
  instead of `allowBuiltinFetchGit`. It is the same checkout. Crane itself only
  warns about a git source with no hash and then clones it while evaluating, so
  `nix.nix` asserts that every `git+` source in `apps/cuetty/Cargo.lock` has an
  entry: a new or re-pinned git dependency fails evaluation with a message saying
  how to obtain the hash. The key is built from the revision in
  `apps/cuetty/Cargo.toml`, and a stale hash for a new revision is Nix's ordinary
  hash-mismatch error.

Measured on one 32-core x86_64 Linux machine, single runs, with the Cachix
substituter disabled. Other projects were building on it throughout (load average
8 to 23 during the new runs, 3 to 17 during the baseline), so differences of a few
seconds are noise. "Before" rebuilt the existing derivations with
`nix build --rebuild`; "cold" built the new ones with the dependency derivations
absent; "source edit" is a comment in `apps/cuetty/src/main.rs` with the
dependency derivations present.

| Check | Before (cargo) | Cold, after | Source edit, after |
| --- | --- | --- | --- |
| `cuetty` (release package) | 58.6s (54.2s) | 67.3s (deps 51.8s, app 5.7s) | 16.1s (6.3s) |
| `cuetty-test` | 49.5s (42.0s + 3.3s doctests) | 64.8s (deps 41.5s + 3.5s, tests 7.0s) | 15.9s (6.9s) |
| `cuetty-clippy` | 31.0s (27.5s) | 37.7s (deps 26.6s, clippy 2.9s) | 8.4s (3.1s) |

Cold builds are slower and source-edit rebuilds are faster. The cold overhead is
the dependency build followed by compressing the dependency artifacts with zstd and
decompressing them in the consumer, which the old single build did not do. The
archives are large (466M + 764M + 368M, about 1.6 GB), and a fresh runner pulls
them from Cachix, which is a cost of this design. The test check ran 617 tests
(610 + 5 + 2 in the unit and integration binaries, no doctests) before and after.

Frequency, counted from git history: of 293 commits since 2026-09-01, 281 touched
the old cuetty source and 242 still touch the narrowed one, while about 23
non-release commits would change the cuetty dependency derivations. Most commits
therefore take the faster source-edit path and a minority pay the cold cost. The
effect on CI is not yet measured: before this change the `checks.cuetty` job, which
builds all four cuetty checks, took 5m26s on pull request run 43256828942.

### Hook types moved into the manifest crate

`cuenv-manifest` no longer depends on `cuenv-hooks`. The `Hook` and `Hooks`
definition types moved to `crates/manifest/src/hook.rs`, and `cuenv-hooks` now
depends on the manifest crate and re-exports them at their old paths, so no
dependent changed. For an edit to hook execution code, `cuenv-manifest` and
`cuenv-infrastructure` (lib, lib test and its 2 integration tests) leave the
rebuild cascade: `cargo test --workspace --no-run` after touching
`crates/hooks/src/executor/hook_executor.rs` rebuilds 81 units instead of 87.
`cuenv-core` still rebuilds because it depends on `cuenv-hooks`.

Wall times are indicative only, because the machine was under unrelated load
(load average 53 to 70 across these runs). The earlier before/after runs took
15.45s and 8.86s to 9.37s; a later warm re-run of the hook edit took about 10.7s
(timing-report total, load average 70). The trade-off: an edit to
`cuenv-manifest` now also rebuilds `cuenv-hooks` (87 units, 12.81s at load
average 53).

### Release profile: thin LTO

`[profile.release]` changed from fat LTO with one codegen unit
(`lto = true`, `codegen-units = 1`) to thin LTO with 16 codegen units
(`lto = "thin"`, `codegen-units = 16`). `opt-level = "z"`, `panic = "abort"` and
`strip = true` are unchanged, and there is no separate fat-LTO profile: release
artifacts built by `.github/scripts/release-build.sh` (`cargo build --release`
and `cargo zigbuild --release`) and the Nix `cuenv` package all use it. Nothing
in `.github/` or `env.cue` passes LTO flags or `CARGO_PROFILE_RELEASE_*`
overrides, and `apps/cuetty` is a separate workspace with its own profiles.
`contrib/contributors/cuenv.cue` also builds with `cargo build --release -p
cuenv` and benefits.

Measured on one loaded 32-core x86_64 Linux machine, single runs, sccache
disabled, `cargo build --release --locked -p cuenv`. The one-minute load average
ranged from 3 to 119 during these runs and the fat-LTO rebuilds ran under the
heaviest load, so the rebuild gap is probably overstated.

| | Fat LTO, 1 codegen unit (before) | Thin LTO, 16 codegen units (after) |
| --- | --- | --- |
| Cold build | 196s | 86s |
| Rebuild after touching `crates/cuenv/src/lib.rs` (two runs) | 274s / 252s | 55s / 76s |
| Binary | 50,252,032 bytes | 65,836,736 bytes |
| gzip -9 | 20,487,268 bytes | 24,423,591 bytes |
| `cuenv version`, mean of 200 | 3.7 ms | 4.8 ms |
| `cuenv env print --output json` on `examples/env-basic`, mean of 50 | about 22 ms | about 23 ms |

The Nix `cuenv` package (`nix build .#checks.x86_64-linux.cuenv`, Cachix
substituter disabled, load average 43 at the start) took 1m 39s end to end after
the change, which rebuilt the release dependency derivation because the profile
changed: cargo reported 44.6s for the dependencies and 43.3s for the workspace,
against 47s and 3m 09s with fat LTO. The installed binary is 65,989,728 bytes
against 50,442,336 for 0.73.1, and gzip -9 gives 23,935,643 bytes. After adding
a comment line to `crates/cuenv/src/lib.rs` the check took 52s (load average 23),
with the cargo build finishing in 44.4s; no third-party crate recompiled, only
the 36 workspace crates. The package install check passed.

The cost is a larger binary and download (about 31% larger on disk, about 19%
larger gzipped) and roughly 1 ms slower startup; commands that evaluate CUE are
within noise. Two variants were measured and not chosen: thin LTO at
`opt-level = 3` (87.7 MB, no measurable runtime gain) and no LTO (69.1 MB, with
rebuilds of 16 to 18s).

The macOS and cross-compiled linux-arm64 release builds have not yet been built
with thin LTO, so run the `release-dry-run` pipeline before the next release.

### One build of the `cuenv` package per pull request

On the thin-LTO commit `c0e00ce2` the package was compiled twice at the same
moment on separate runners: `checks.cuenv` in `cuenv-ci` took 175s in its step,
and the "Build cuenv (nix)" bootstrap of `cuenv-sync-check` took 143s (runs
43282606369 and 43282606387). Neither could substitute the other's result
because they started together. The `checks.cuenv` job is removed from the `ci`
pipeline; the bootstrap builds the package, including its install check, on
every trigger of the `ci` pipeline except a change to `cuenv-ci.yml` alone,
which cannot change the package. Every `x86_64-linux` flake check is still built
by some CI job.

Trade-off: a failure to build the package, or to pass its install check, now
shows as a failed `cuenv-sync-check` rather than a failed `checks.cuenv`. No
branch protection rule requires `checks.cuenv`. `docs-preview` still builds the
package separately on pull requests that touch the docs (188s in run
43282606339); that is not addressed here.

Publishing the `x86_64-linux` package to Cachix now depends on the `sync-check`
job. On `main` run 43105997270 the `checks.cuenv` job pushed the `cuenv-0.73.1`
package path, while `cuenv-sync-check` run 43105997318 logged it as skipped. The
`sync-check` job has the same Cachix setup step, so it is expected to push the
path from now on, but that has not been observed yet.

Coupling to keep: if the `sync-check` pipeline is narrowed or removed, or
`ci.cuenv.source` stops being `nix`, the package check must be listed in the
`ci` pipeline again.