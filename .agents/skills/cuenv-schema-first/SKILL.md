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

Schema authoring rules, enforced by `cuenv task ci.schema-compat` across CUE v0.15.1 (the Nix dev shell), v0.16.1, v0.17.1 and v0.18.0-rc.1:

- Do not wrap definitions in `close({...})`. Definitions are already closed, and `close()` with an embedded definition breaks file-level embedding on CUE v0.17+.
- Reference a definition from `#Base`/`#Project` fields inside a struct literal (`config?: {#Config}`), and wrap each definition in a disjunction the same way. A bare reference is left open when a consumer embeds `schema.#Project` at file level before language `v0.18.0`.
- Exception: keep `tasks`, `services` and `images` entries as direct references (`#Task`, `#Service`, `#ContainerImage`). The CUE fork shares typed `dependsOn` references only for directly referenced definitions; wrapping them makes deep dependency chains exponential (`deep_depends_on_chain_evaluates_in_linear_time` in `schema_project_base_tests.rs` catches it). Examples should write entries as `schema.#Task & {...}`.
- Use `{...}` instead of `_` for free-form object fields; CUE v0.17 closes `_` under file-level embedding.
- Add a `tests/schema-compat/testdata/reject/<name>/env.cue` fixture when a change adds a closedness guarantee.

Use `cuenv task ci.schema-docs-check` after changing schema, docs, examples, prompts, or skills.

Adversarial prompts to test this skill:

- "Generate an env.cue that uses Vault and AWS secrets." Confirm schema support but default resolver limitations.
- "How do I generate CODEOWNERS?" Avoid `cuenv sync codeowners`; route through rules status.
- "Can I build container images with cuenv build?" Explain `#ContainerImage` is partial: `cuenv build` can list image definitions and build selected images with Docker, while Dagger execution and downstream output-reference resolution remain incomplete.
