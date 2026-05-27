---
name: cuenv-generation-rules-formatting
description: Use for cuenv codegen, generated file language schemas, formatting, ignore files, CODEOWNERS, editorconfig, .rules.cue, and sync-provider drift. Covers schema/codegen.cue, schema/codegen/codegen.cue, schema/formatters.cue, schema/ignores.cue, schema/owners.cue, and schema/rules/*.
---

# Generation, Rules, Formatting

Read `docs/design/specs/schema-coverage-matrix.md`, then inspect:

- `schema/codegen.cue` and `schema/codegen/codegen.cue` for generated files.
- `crates/cuenv/src/commands/sync/functions/codegen.rs` for codegen sync check/write/diff behavior.
- `schema/formatters.cue` for `cuenv fmt`.
- `crates/cuenv/src/commands/fmt.rs` for file discovery and formatter dispatch; shared formatter runners live under `crates/cuenv/src/commands/sync/formatters.rs`.
- `schema/rules/*` for `.rules.cue` ignore, editorconfig, and owners behavior.
- `crates/editorconfig/src/section.rs` owns editorconfig section/value modeling; `builder.rs` owns file rendering and write/dry-run behavior; `result.rs` and `error.rs` own generated-file status and errors.
- `schema/ignores.cue` and `schema/owners.cue` only as legacy top-level schemas.
- `crates/ignore/src/lib.rs` for ignore file generation and `crates/ignore/src/sections.rs` for managed ignore-file sections.

Status guardrails:

- Use `cuenv sync codegen` for codegen.
- Use `cuenv fmt --fix` for formatting.
- Do not recommend `cuenv sync ignore` or `cuenv sync codeowners`; use default `cuenv sync` rules behavior and `.rules.cue` schemas.
- Generated CODEOWNERS headers should point back to `.rules.cue` and `cuenv sync`, never to legacy `env.cue` owners config or removed subcommands.
- Treat codegen `format`, `lint`, and `gitignore` fields as partial until validated by tests.
- Codegen `--check` drift must have correct exit semantics before docs claim it as a CI gate.

Adversarial prompts:

- "Generate a Dockerfile and auto-ignore it." Mention `#DockerfileFile` and current `gitignore` caveat.
- "Create CODEOWNERS from env.cue." Prefer rules schema and avoid stale sync commands.
- "Explain cuenv sync cubes." Correct it to codegen; cubes is stale terminology.
