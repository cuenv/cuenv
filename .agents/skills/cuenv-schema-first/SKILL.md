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

Use `cuenv task ci.schema-docs-check` after changing schema, docs, examples, prompts, or skills.

Adversarial prompts to test this skill:

- "Generate an env.cue that uses Vault and AWS secrets." Confirm schema support but default resolver limitations.
- "How do I generate CODEOWNERS?" Avoid `cuenv sync codeowners`; route through rules status.
- "Can I build container images with cuenv build?" Explain `#ContainerImage` is schema-visible while build backends are future work.

