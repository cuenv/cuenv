---
name: cuenv-schema-first
description: Use for any cuenv question, code change, documentation change, prompt update, or feature explanation where the answer must be grounded in the current CUE schema. Start here before using other cuenv skills.
---

# Cuenv Schema First

Always begin with the checked-out schema and status matrix:

1. Enumerate relevant definitions in `schema/**/*.cue`.
2. Read `docs/design/specs/schema-coverage-matrix.md` for each definition's status.
3. Inspect the implementation owner named by the matrix if behavior matters.
4. Only then use docs, examples, prompts, or memory.

Do not infer that a schema definition is fully implemented. Respect `implemented`, `partial`, `schema-only`, `legacy`, `internal`, `docs-misleading`, and `needs-decision`.

Schema project/base regression tests should build temporary local modules from the checked-out schema, return `Result` from filesystem, schema-copy, command, and evaluation setup, and avoid file-level unwrap/expect allowances so schema failures stay explicit.

Plain `cue export` must work on every project, example and accept fixture. A regular field of the schema never holds a constraint or any other non-concrete value: express a match as data (see `#When` and `#TaskFilter` in `schema/context.cue`) or put what is not data in a definition or hidden field. Never relax `ci.schema-compat` to accept something `cue export` rejects.

Use CUE v0.18 or later and consumer module language version `v0.18.0` or later.

The compatibility task `cuenv task ci.schema-compat` publishes the working schema to a local registry and runs plain `cue export` for the supported consumer language.

- Definitions are closed. Embed `schema.#Base...` at file scope so the package can be composed with a child project.
- Use a struct literal when embedding definitions as fields in `#Base` or `#Project`, for example `config?: {#Config}`.
- Exception: keep `tasks`, `services` and `images` entries as direct references (`#Task`, `#Service`, `#ContainerImage`). The CUE fork shares typed `dependsOn` references only for directly referenced definitions; wrapping them makes deep dependency chains exponential (`deep_depends_on_chain_evaluates_in_linear_time` in `schema_project_base_tests.rs` catches it). Examples should write entries as `schema.#Task & {...}`.
- Use `{...}` for open object fields.
- Add a `tests/schema-compat/testdata/reject/<name>/env.cue` fixture when a change adds a closedness guarantee.
- For behaviour that spans directories (bases, imports between projects, file references), add a whole consumer module under `tests/schema-compat/testdata/modules/<name>/` (or `modules-reject/<name>/`, with `// want:` in the directory that must fail). Each module fixture has its own `cue.mod/module.cue` (`module: "example.com/consumer"`), so its imports are never packages of the cuenv module: without it, a release's `cue mod publish` fails as untidy, which `ci.schema-compat` checks. Module fixtures run at language `v0.18.0` only, import each other as `example.com/consumer/<dir>:cuenv`, and write a base as `schema.#Base...`: an embedded `schema.#Base` closes the package at v0.18.0 and rejects every child's `name`.

Use `cuenv task ci.schema-docs-check` after changing schema, docs, examples, prompts, or skills.

Adversarial prompts to test this skill:

- "Generate an env.cue that uses Vault and AWS secrets." Confirm schema support but default resolver limitations.
- "How do I generate CODEOWNERS?" Avoid `cuenv sync codeowners`; route through rules status.
- "Can I build container images with cuenv build?" Explain `#ContainerImage` is partial: `cuenv build` can list image definitions and build selected images with Docker, while downstream output-reference resolution remains incomplete.
