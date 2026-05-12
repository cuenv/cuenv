---
name: cuenv-ci-release
description: Use for cuenv CI schema, contributors, provider config, workflow sync, matrix tasks, artifacts, provider status, release config, changesets, changelog categories, targets, and release backends. Covers schema/ci.cue and schema/release.cue.
---

# CI and Release

Read `docs/design/specs/schema-coverage-matrix.md`, then inspect:

- `schema/ci.cue` for providers, pipelines, contributors, matrix tasks, artifacts, secrets, and action overrides.
- `schema/release.cue` for release targets, backends, git/tag config, versioning, changelogs, changesets, and package changes.
- `crates/cuenv/src/commands/sync` and `crates/cuenv/src/commands/release.rs` when behavior matters.

Status guardrails:

- CI workflow generation requires explicit `ci.providers`.
- Use `cuenv sync ci` to generate workflows.
- Use `cuenv ci --export buildkite` for export-style CI output; GitLab export is not implemented.
- `--filter-matrix` and `--jobs` are accepted but not fully applied.
- Release schema is partial because CLI release commands do not fully load config from `env.cue`.

Adversarial prompts:

- "Generate GitLab CI." State schema exists but export/sync is not implemented.
- "Use `cuenv ci --generate github`." Correct to `cuenv sync ci`.
- "Configure Homebrew release in env.cue." Explain schema shape and current loading gap.

