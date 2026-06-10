---
title: Manage releases
description: Cut releases with cuenv — declare a release block, record changesets, bump versions, write changelogs, and publish crates and binaries.
---

A release usually means juggling three hand-maintained things at once: version
numbers scattered across manifests, a CHANGELOG nobody enjoys writing, and a
pile of publish scripts. cuenv folds all of that into one typed `release:` block
in `env.cue` plus a changeset log. You declare *what* a release looks like once,
record changes as you make them, and then a small family of `cuenv changeset`
and `cuenv release` commands turns those records into version bumps, changelogs,
and published artifacts.

:::caution[Status: Partial]
The whole release family is **Partial**. It is genuinely useful — changesets,
version aggregation, changelog generation, the unified `prepare` flow, and
topological crates.io publishing all work — but it is **not** a turnkey
end-to-end pipeline yet. CUE registry publishing fails fast as unimplemented on
a real run, binary backend coverage is limited, and some commands carry explicit
"not fully implemented" notes. Read the caveats in [Know the
limits](#know-the-limits) and the authoritative
[schema status](/reference/schema/status/) before relying on any step.
:::

## Before you start

You need a Cargo workspace whose root `Cargo.toml` carries
`[workspace.package]` with a `version`, and crates that inherit it via
`version.workspace = true`. The release commands read and rewrite those
manifests. A `gh`-authenticated checkout is required only for `release prepare`
when it opens a pull request, and registry tokens are required only at publish
time.

## 1. Declare a `release:` block

Add a `release:` block to your root `env.cue`. Every field below is real
schema from [`schema/release.cue`](/reference/cue-schema/#release-configuration); the
configuration is loaded by the CLI release commands today, though not every
backend is fully wired (see [Know the limits](#know-the-limits)).

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "myapp"

	release: schema.#Release & {
		// Binary name for distribution (defaults to the project name).
		binary: "myapp"

		// Targets used by `cuenv release binaries`.
		targets: ["linux-x64", "linux-arm64", "darwin-arm64"]

		git: {
			defaultBranch: "main"
			// Project rule: no `v` prefix. The default is already empty,
			// which yields bare versions like 0.50.0.
			tagPrefix: ""
			tagType:   "semver"
		}

		packages: {
			// "independent" (default), "linked", or "fixed".
			strategy: "independent"
		}

		changelog: {
			path:       "CHANGELOG.md"
			workspace:  true
			perPackage: true
		}

		backends: {
			crates: {
				tokenEnv: "CARGO_REGISTRY_TOKEN"
				ordered:  true
			}
			github: {
				assets: true
				draft:  false
			}
		}
	}

	tasks: {}
}
```

A few field notes worth internalising:

- **`git.tagPrefix` defaults to empty**, which is exactly the cuenv project rule:
  tags and release titles are bare versions (`0.50.0`, never `v0.50.0`).
- **`packages.strategy`** controls monorepo versioning: `independent` (each crate
  versioned on its own), `linked` (bumped together, versions may differ), or
  `fixed` (lockstep, all share one version). `fixed` and `linked` groups are
  honoured by version calculation.
- **`backends`** is the opt-in switch. With no `backends`, a default crates.io
  backend is assumed. With `backends` present, only the backends you list are
  active.

If you omit `release:` entirely, the commands fall back to sensible defaults
(crates backend, semver, empty tag prefix), so you can try changesets before
committing to a config.

## 2. Record changes as changesets

A changeset is a small file describing which packages a change affects and how
hard to bump each one. Record one whenever you make a user-visible change —
think of it as a queued, machine-readable CHANGELOG entry.

On a TTY, run the interactive picker:

```bash
cuenv changeset add
```

Or specify everything non-interactively. The `-P` flag takes
`package:bump` pairs, where bump is `major`, `minor`, `patch`, or `none`:

```bash
cuenv changeset add \
  -s "Add AWS Secrets Manager resolver" \
  -P cuenv-core:minor \
  -P cuenv:patch
```

```text
Created changeset: .changeset/relaxed-otters-sing.md
  ID: relaxed-otters-sing
  Summary: Add AWS Secrets Manager resolver
```

Already have conventional commits? Generate a changeset straight from history.
This analyzes which packages each commit touched and bumps only those
(`feat:` → minor, `fix:` → patch); changes to root-level files do not bump any
package:

```bash
cuenv changeset from-commits --since 0.49.0
```

```text
Created changeset from 7 conventional commit(s)
  Path: .changeset/swift-pandas-wave.md
  ID: swift-pandas-wave

Packages affected:
  • cuenv (minor)
  • cuenv-core (minor)

Packages unchanged:
  • cuenv-cache
```

`--since` accepts a tag or ref; commit parsing honors your configured
`git.tagPrefix` and `git.tagType`, so it finds the right "last tag" for your
versioning scheme.

## 3. Inspect what is queued

Before bumping anything, review the pending changesets and the bumps they
aggregate to:

```bash
cuenv changeset status
```

```text
Found 2 pending changeset(s):

  relaxed-otters-sing - Add AWS Secrets Manager resolver
    • cuenv-core (minor)
    • cuenv (patch)

  swift-pandas-wave - Release from 7 commits
    • cuenv (minor)

Aggregated version bumps:

  cuenv: minor
  cuenv-core: minor
```

For CI, add `--json` to get structured output with `count`, `has_pending`, the
individual changesets, and the `aggregated_bumps` map — ideal for gating a
"do we have anything to release?" job:

```bash
cuenv changeset status --json
```

## 4. Apply version bumps

`cuenv release version` consumes the pending changesets, calculates the new
version per package, and writes the changes. Always dry-run first:

```bash
cuenv release version --dry-run
```

```text
Dry run - no changes will be made.

Version changes:

  cuenv: 0.50.0 -> 0.51.0
  cuenv-core: 0.50.0 -> 0.51.0
```

Drop `--dry-run` to apply. On a real run it updates the `[workspace.package]`
version, rewrites the matching `[workspace.dependencies]` version strings,
writes the workspace and per-package CHANGELOGs according to your `changelog`
config, and then **deletes the changesets it consumed**:

```text
Version changes:

  cuenv: 0.50.0 -> 0.51.0
  cuenv-core: 0.50.0 -> 0.51.0

Manifest files updated successfully.
Changelogs have been updated.
Changesets have been consumed.
```

If there are no changesets, the command stops with an error telling you to run
`cuenv changeset add` first.

:::note[Do not restamp `cue.mod/module.cue`]
`cuenv release version` rewrites the Cargo manifests, changelogs, and lockfile
inputs. It does **not** update `cue.mod/module.cue`, and release-only version
bumps should not edit that file. cuenv publishes the CUE module at the release
tag; consumer projects update their schema dependency with `cue mod get`.
:::

For a release-only version bump from an already-green `main`, do not rerun the
local test suite or full root flake check just to restamp versions. Before
tagging, verify `HEAD` matches the green `origin/main` commit, ensure
`Cargo.toml` and `Cargo.lock` agree on the target version, inspect that the
lockfile only changes workspace package versions, run
locked Cargo metadata, and run `git diff --check`. Releases that include code,
schema, workflow, dependency, feature, or behavior changes still need the normal
broader release gate.

The generated release workflow builds cuenv once per runner only when
`config.ci.cuenv.source: "nix"` is active. Each `build.cuenv` job uploads the
checked-out `result/bin/cuenv` as a runner-specific artifact, and downstream
orchestrated jobs download that binary into the runner temp directory instead
of rebuilding cuenv in every job. Keeping the downloaded binary outside the
checkout preserves a clean VCS state for release steps such as
`cue mod publish`.
Release, Homebrew, native, git, and artifact sources render their setup task
inside each job. Linux jobs still restore the Namespace `/nix` cache before Nix
setup, while macOS jobs skip that cache and use normal Nix installation.
:::

## 5. Prepare a release in one shot

`cuenv release prepare` is the unified path: it analyzes commits since the last
tag, maps them to packages, calculates bumps, updates manifests, regenerates
changelogs, then creates a `release/next` branch, commits, pushes, and opens a
PR via the `gh` CLI. It does **not** read your `.changeset/` queue — it derives
bumps from conventional commits directly.

Preview first:

```bash
cuenv release prepare --dry-run
```

```text
Release Prepare Summary
=======================

Commits analyzed: 7
Packages affected: 2

Changelog path: CHANGELOG.md

Version Bumps:
------------------------------------------------------------
Package                             Current          New
------------------------------------------------------------
cuenv                                0.50.0       0.51.0
cuenv-core                           0.50.0       0.51.0
------------------------------------------------------------

[DRY RUN] No changes applied.

To apply changes, run without --dry-run
```

Useful flags:

- `--since <ref>` — analyze commits from a specific tag or ref instead of the
  detected last tag.
- `--branch <name>` — branch to create (default `release/next`).
- `--no-pr` — do everything except open the PR (commit and push only).
- `--dry-run` — summarize without touching anything.

Opening the PR requires an authenticated `gh` CLI (`gh auth login`) and a remote
`origin`. If PR creation fails, prepare still leaves the branch pushed and tells
you to open the PR manually.

## 6. Publish to crates.io

`cuenv release publish` computes a topological publish order from your
workspace dependency graph so dependencies publish before dependents. Dry-run
shows the plan:

```bash
cuenv release publish --dry-run
```

```text
Dry run - no packages will be published.

Publish plan (topologically sorted):

  1. cuenv-core
  2. cuenv-cache
  3. cuenv (skipped: publish disabled)

Dry run complete.
```

Crates with `publish = false` (or no `crates-io` registry) in their
`Cargo.toml` are skipped. There is a guardrail: if a *publishable* crate depends
on a *skipped* one, the command errors rather than producing a broken release.

Dropping `--dry-run` runs `cargo publish -p <crate> --locked` for each
publishable crate in order. Authentication comes from the `tokenEnv` you
configured (default `CARGO_REGISTRY_TOKEN`); cuenv forwards it under the name
cargo expects:

```bash
export CARGO_REGISTRY_TOKEN=...   # value from crates.io
cuenv release publish
```

## 7. Build and publish binaries

`cuenv release binaries` drives the binary distribution backends (GitHub
Releases, Homebrew tap). It builds, packages, and publishes for the configured
`targets`:

```bash
cuenv release binaries --dry-run
```

Useful flags:

- `--build-only`, `--package-only`, `--publish-only` — run a single phase
  instead of the full build → package → publish pipeline.
- `--backend github,homebrew` — restrict to specific backends.
- `--target linux-x64,darwin-arm64` — override the configured targets.
- `--version <v>` — override the version (defaults to the workspace version).

Backends activate from configuration *and* environment: the GitHub backend
needs `GITHUB_TOKEN`, and the Homebrew backend needs the token named by its
`tokenEnv` (default `HOMEBREW_TAP_TOKEN`). Backends are also feature-gated at
build time (`github`, `homebrew`), so a binary built without those features
will not offer them.

## Know the limits

The release family is **Partial**. Mirror these limits in your expectations —
do not present it as a finished pipeline.

- **CUE registry publishing is not implemented.** If you configure a `cue`
  backend, `cuenv release publish` accepts it in `--dry-run` but **fails fast**
  with "CUE registry release publishing is not implemented yet" on a real run.
- **`release version` help carries a caveat.** Its clap help notes that manifest
  reading is "not yet implemented" in full; treat workspace-only manifests as
  the supported shape and verify the rewritten files.
- **Binary backend coverage is limited.** `release binaries` loads configured
  targets and backends from `env.cue`, but build-backend coverage is partial —
  validate artifacts before trusting a `--publish-only` run.
- **Publish skips and guards.** Crates with `publish = false` are skipped, and
  publishing a crate that depends on a skipped crate is rejected with a
  configuration error.
- **Auth is token-env driven.** crates.io uses `CARGO_REGISTRY_TOKEN` (or your
  `tokenEnv`), GitHub Releases use `GITHUB_TOKEN`, and Homebrew uses
  `HOMEBREW_TAP_TOKEN` (or your `tokenEnv`). Missing tokens silently skip the
  affected backend rather than erroring.
- **No `v` prefix, ever.** Tags and release titles are bare versions. The
  schema default for `git.tagPrefix` is empty, which already enforces this.
- **`release version` does not touch `cue.mod/module.cue`.** That is expected.
  Do not edit `cue.mod/module.cue` for a release-only version bump unless there
  is a separate CUE module metadata change.
- **CUE publish tasks need a valid temp directory.** `cuenv task publish.cue`
  runs hermetically and should not inherit stale Nix shell temp directories; if
  `cue mod publish` reports a missing `cue-publish-*` path, check that the
  task environment is not preserving a removed `TMPDIR`, `TMP`, or `TEMP`.

The authoritative per-definition status lives in the
[schema status page](/reference/schema/status/) and the schema coverage matrix.
When in doubt, that matrix wins over this guide.

## A realistic workflow

For day-to-day, changeset-driven releases:

```bash
# As you work, record changes:
cuenv changeset add -s "Fix readiness probe timeout" -P cuenv-core:patch

# When ready to cut a release, review and apply:
cuenv changeset status
cuenv release version --dry-run
cuenv release version

# Commit the version + changelog updates, tag (bare version!), then publish:
git commit -am "release: 0.51.0"
git tag -a 0.51.0 -m "0.51.0"
cuenv release publish --dry-run
cuenv release publish
```

For a fully automated, commit-driven release PR, use `cuenv release prepare`
instead of the changeset steps — but remember it reads conventional commits,
not your changeset queue.

## Where to go next

- [CLI reference](/reference/cli/) — every flag for `changeset` and `release`.
- [CUE schema reference](/reference/cue-schema/#release-configuration) — the full `#Release`
  shape and defaults.
- [CI](/how-to/ci/) — wire `release version`/`publish` into a generated
  pipeline.
- [Schema status](/reference/schema/status/) — the honest, authoritative
  support matrix.
