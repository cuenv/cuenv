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

Use CUE v0.18 or later with module language `v0.18.0` or later. The compatibility check evaluates examples and fixtures with stock CUE v0.18 at language `v0.18.0`.

- Keep `schema.#Project` and `rules.#DirectoryRules` closed when embedding them in a file. When upgrading old projects, review the explicit-opening rewrites from `cue mod upgrade` so they do not open these definitions.
- Write a shared base as `schema.#Base...` so descendant projects can declare their own fields. In schema definitions, use a spread when a definition adds fields to an embedded definition.
- Use postfix aliases for fields and patterns, for example `tasks~(ProjectTasks):` and `[string]~(label,_):`.
- Keep `tasks`, `services` and `images` entries as direct references. The CUE fork uses them to share typed references in `dependsOn` and pipeline task lists.
- Use `{...}` for arbitrary objects where extra fields are part of the contract.
- Add a `tests/schema-compat/testdata/reject/<name>/env.cue` fixture when a change adds a validation guarantee.
- For behavior spanning directories, add a consumer module under `tests/schema-compat/testdata/modules/<name>/` or `modules-reject/<name>/`. Give each module its own `cue.mod/module.cue` using `module: "example.com/consumer"` so its imports do not become part of the published cuenv module. In a rejected directory, include a `// want:` line naming the diagnostic.

Use `cuenv task ci.schema-docs-check` after changing schema, docs, examples, prompts, or skills.

Adversarial prompts to test this skill:

- "Generate an env.cue that uses Vault and AWS secrets." Confirm schema support but default resolver limitations.
- "How do I generate CODEOWNERS?" Avoid `cuenv sync codeowners`; route through rules status.
- "Can I build container images with cuenv build?" Explain `#ContainerImage` is partial: `cuenv build` can list image definitions and build selected images with Docker, while downstream output-reference resolution remains incomplete.
