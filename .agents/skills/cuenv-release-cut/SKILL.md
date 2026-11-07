---
name: cuenv-release-cut
description: "Use when cutting a cuenv release manually: bumping the workspace Cargo version and internal crate dependency versions, refreshing Cargo.lock, creating annotated bare-version tags, creating GitHub releases, and monitoring the release workflow."
---

# Cuenv Release Cut

Use this for an actual cuenv release operation. For release schema/config changes, use `cuenv-ci-release` first; this skill is for the maintainer workflow that mutates versions, tags, and GitHub releases.

Read the current release rules before acting:

- `AGENTS.md` release section for project policy.
- `docs/src/content/docs/how-to/releases.mdx` for release-tooling limits.
- `docs/src/content/docs/how-to/develop-cuenv.md` for maintainer release notes.
- `Cargo.toml` and `Cargo.lock` for the current version state.
- `env.cue` and `.github/workflows/cuenv-release.yml` when GitHub release workflow behavior matters.

## Manual Release Workflow

1. Preflight:
   - Confirm the target version with the user if it was not explicit.
   - Work from `main` at `origin/main` with a clean tree.
   - Fetch tags and inspect the latest release tag: `git fetch --tags origin` and `git tag --list --sort=-v:refname | head`.
   - Verify GitHub auth before network writes: `gh auth status`.

2. Bump versions:
   - In root `Cargo.toml`, set `[workspace.package].version` to the new bare semver.
   - In root `Cargo.toml`, set every internal path dependency under `[workspace.dependencies]` (`cuenv-*` and `cuengine`) to the same version.
   - Do not edit `cue.mod/module.cue` for a release-only version bump unless there is a separate CUE module metadata change.

3. Refresh and verify the lockfiles:
   - Run `cuenv exec -- cargo update --workspace`.
   - Run `cuenv exec -- cargo metadata --locked --format-version 1`.
   - Inspect `git diff -- Cargo.lock`; expected release-only lockfile changes are workspace package version entries matching the new version.
   - `apps/cuetty` is a separate Cargo workspace with its own `Cargo.lock` that pins the path dependency `cuengine`. Set that entry's `version` to the new version, then run `cuenv exec -- cargo metadata --locked --format-version 1 --manifest-path apps/cuetty/Cargo.toml`. Its diff must be that one line. A stale entry fails the `checks.cuetty` CI job on the release commit, after the tag already exists.

4. Write the release notes:
   - Add `docs/releases/<version>.md`, headed `# cuenv <version>`, summarizing the user-visible changes since the previous release with links to their pull requests. Follow the previous notes in `docs/releases/`.

5. Validate before committing or tagging:
   - For a release-only version bump from an already-green `main`, do not rerun local tests or the full root flake gate. Verify `HEAD` matches `origin/main`, confirm `cuenv-ci`, `cuenv-darwin`, and `cuenv-arm` are all green for that commit (pull requests run neither macOS nor ARM jobs), run the lockfile and metadata checks above, inspect that `Cargo.lock` only updates workspace package versions, verify `Cargo.toml`, `Cargo.lock`, and the `cuengine` entry in `apps/cuetty/Cargo.lock` agree on the target version, and run `git diff --check`.
   - For releases that include code, schema, CI/release workflow, dependency, feature, or behavior changes beyond the version bump, run `cuenv fmt --fix`, `git diff --check`, and `nix flake check -L --accept-flake-config`.
   - If docs, schema, prompts, examples, or `.agents/skills/**` changed, also run `cuenv task ci.schema-docs-check`.
   - Do not tag, create a GitHub release, publish, request review, merge, or release if a required check failed or was skipped.

6. Commit and push:
   - Stage `Cargo.toml`, `Cargo.lock`, `apps/cuetty/Cargo.lock`, `docs/releases/<version>.md`, and any other docs files that changed.
   - Commit as `release: <version>`.
   - Push the release commit to `origin main`.

7. Tag:
   - Tags are annotated and bare: `git tag -a <version> -m "<version>"`.
   - Never use a `v` prefix for the Git tag or GitHub release title.
   - Verify the tag is annotated with `git cat-file -t <version>`; it must print `tag`.
   - Push the tag: `git push origin <version>`.

8. Create the GitHub release:
   - Use the existing annotated tag; do not let GitHub create a lightweight tag.
   - Command: `gh release create <version> --verify-tag --title <version> --generate-notes`.
   - If creating a draft, remember the release workflow only triggers on `release.published`; publish the draft when ready.

9. Monitor release automation:
   - The release workflow is `cuenv-release.yml` and triggers on a published release. Its manual-dispatch example may show a `v` prefix; ignore that stale example and use the bare tag.
   - Watch the release run with `gh run list --repo cuenv/cuenv --workflow cuenv-release.yml --event release --limit 5`, then `gh run watch <run-id> --repo cuenv/cuenv`.
   - The workflow builds binaries, uploads GitHub release assets, publishes the CUE module using `cue mod publish v$TAG`, updates Homebrew, and deploys docs. The internal `v$TAG` in CUE publishing does not change the project rule that Git tags and release titles are bare.
   - Dependabot does not rebase its pull requests automatically (`rebase-strategy: "disabled"` in `.github/dependabot.yml`), so open Cargo updates may conflict after the release commit. Do not rebase them while the release runs, because their CI competes with the release builds for x86 runners; comment `@dependabot rebase` on one when it is about to be merged.

## Guardrails

- Never run `cargo publish --workspace` for cuenv; use `cuenv release publish` when crates.io publishing is needed.
- `cuenv release version` is allowed when the user asks for the changeset-driven path, but it consumes changesets and does not update `cue.mod/module.cue`; that is expected for release-only bumps.
- Stop before tagging if `Cargo.toml`, `Cargo.lock`, and `apps/cuetty/Cargo.lock` disagree on the cuenv version, or if `docs/releases/<version>.md` is missing.
- Stop before release creation if the tag is missing, lightweight, prefixed with `v`, or not pushed to origin.
