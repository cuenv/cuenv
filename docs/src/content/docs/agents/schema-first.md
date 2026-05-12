---
title: Schema-first workflow
description: How agents should answer cuenv questions without hallucinating stale features
---

Cuenv agents must not rely on training-data memory for feature support. The current checkout is authoritative.

## Required order

1. Read the relevant files under `schema/**/*.cue`.
2. Check `docs/design/specs/schema-coverage-matrix.md` for implementation status.
3. Inspect the Rust owner or CLI surface named by the matrix when behavior matters.
4. Use docs and examples only after they agree with the schema and matrix.

## Do not generate stale examples

- Use `schema.#ExecSecret` for custom command secrets, not `schema.#Secret & { command: ... }`.
- Use `schema.#AwsSecret`, `schema.#GcpSecret`, and `schema.#VaultSecret` only with a status note that runtime resolvers are not registered by default.
- Use `cuenv env print --output json`, not `--output-format`.
- Use `cuenv sync ci`, not `cuenv ci --generate`.
- Do not recommend `cuenv sync ignore` or `cuenv sync codeowners`; rules are handled through the default sync provider and `.rules.cue` schemas.
- Do not treat `schema.#NixFlake` as a schema type. `#NixFlake` is a contrib hook in `contrib/nix`.

## Required PR check

Run:

```bash
cuenv task ci.schema-docs-check
```

This check verifies that every exported schema definition has a matrix row and that repo-local skills point agents back to the matrix.

