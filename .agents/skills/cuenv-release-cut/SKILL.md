---
name: cuenv-release-cut
description: "Use when cutting a cuenv release manually: bumping the workspace Cargo version and internal crate dependency versions, refreshing Cargo.lock, updating cue.mod/module.cue, creating annotated bare-version tags, creating GitHub releases, and monitoring the release workflow."
---

# Cuenv Release Cut

Use this for an actual cuenv release operation. For release schema/config changes, use `cuenv-ci-release` first; this skill is for the maintainer workflow that mutates versions, tags, and GitHub releases.

Read the current release rules before acting:

- `AGENTS.md` release section for project policy.
- `docs/src/content/docs/how-to/releases.md` for release-tooling limits.
- `docs/src/content/docs/how-to/develop-cuenv.md` for maintainer release notes.
- `Cargo.toml`, `Cargo.lock`, and `cue.mod/module.cue` for the current version state.
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
   - In `cue.mod/module.cue`, set `custom."github.com/cuenv/cuenv".version` to the same version.
   - Do not change `cue.mod/module.cue` `language.version`; that is the CUE language version.

3. Refresh and verify the lockfile:
   - Run `cuenv exec -- cargo update --workspace`.
   - Run `cuenv exec -- cargo metadata --locked --format-version 1`.
   - If the installed `cuenv` rejects the bumped marker with `Project requires cuenv <new>; this CLI is <old>`, use direct Cargo for this Cargo-owned step instead: `cargo update --workspace` and `cargo metadata --locked --format-version 1`. Record that fallback in the release notes or final summary.
   - Inspect `git diff -- Cargo.lock`; expected release-only lockfile changes are workspace package version entries matching the new version.

4. Validate before committing or tagging:
   - Release work is a full-gate trigger. Run `cuenv fmt --fix`, `git diff --check`, and `nix flake check -L --accept-flake-config`.
   - If `cuenv fmt --fix` is blocked by the same bumped-marker version guard, run the formatter through the checked-out release tree instead: `nix run .#cuenv -- fmt --fix`. This can require building the new cuenv binary and may take several minutes; do not cancel it.
   - If docs, schema, prompts, examples, or `.agents/skills/**` changed, also run `cuenv task ci.schema-docs-check`. Use `nix run .#cuenv -- task ci.schema-docs-check` if the installed CLI is version-gated out.
   - Do not tag, create a GitHub release, publish, request review, merge, or release if the full flake check failed or was skipped.

5. Commit and push:
   - Stage `Cargo.toml`, `Cargo.lock`, `cue.mod/module.cue`, and any changelog/docs files that changed.
   - Commit as `release: <version>`.
   - Push the release commit to `origin main`.

6. Tag:
   - Tags are annotated and bare: `git tag -a <version> -m "<version>"`.
   - Never use a `v` prefix for the Git tag or GitHub release title.
   - Verify the tag is annotated with `git cat-file -t <version>`; it must print `tag`.
   - Push the tag: `git push origin <version>`.

7. Create the GitHub release:
   - Use the existing annotated tag; do not let GitHub create a lightweight tag.
   - Command: `gh release create <version> --verify-tag --title <version> --generate-notes`.
   - If creating a draft, remember the release workflow only triggers on `release.published`; publish the draft when ready.

8. Monitor release automation:
   - The release workflow is `cuenv-release.yml` and triggers on a published release. Its manual-dispatch example may show a `v` prefix; ignore that stale example and use the bare tag.
   - Watch the release run with `gh run list --repo cuenv/cuenv --workflow cuenv-release.yml --event release --limit 5`, then `gh run watch <run-id> --repo cuenv/cuenv`.
   - The workflow builds binaries, uploads GitHub release assets, publishes the CUE module using `cue mod publish v$TAG`, updates Homebrew, and deploys docs. The internal `v$TAG` in CUE publishing does not change the project rule that Git tags and release titles are bare.

## Guardrails

- Never run `cargo publish --workspace` for cuenv; use `cuenv release publish` when crates.io publishing is needed.
- `cuenv release version` is allowed when the user asks for the changeset-driven path, but it consumes changesets and does not update `cue.mod/module.cue`; fix the marker before committing.
- Stop before tagging if `Cargo.toml`, `Cargo.lock`, and `cue.mod/module.cue` disagree on the cuenv version.
- Stop before release creation if the tag is missing, lightweight, prefixed with `v`, or not pushed to origin.
