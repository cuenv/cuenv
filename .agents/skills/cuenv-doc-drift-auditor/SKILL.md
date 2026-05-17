---
name: cuenv-doc-drift-auditor
description: Use when reviewing or changing cuenv docs, prompts, examples, llms.txt, README, agent skills, CLI references, or schema coverage. Detects stale commands, stale schema names, missing matrix rows, and unsupported feature claims.
---

# Doc Drift Auditor

Start with `docs/design/specs/schema-coverage-matrix.md` and run:

```bash
cuenv task ci.schema-docs-check
```

Review this scope when docs or agent behavior changes:

- `schema/**/*.cue`
- `docs/design/specs/schema-coverage-matrix.md`
- `docs/src/content/docs/reference/schema/status.md`
- `docs/src/content/docs/agents/**`
- `docs/src/content/docs/how-to/**`
- `docs/src/content/docs/tutorials/**`
- `docs/src/content/docs/index.mdx`
- `.agents/skills/**`
- `prompts/**`
- `llms.txt`
- `readme.md`

Reject stale claims unless there is an explicit compatibility note:

- `schema.#Secret & { command: ... }`
- `schema.#AWSSecretRef`
- `schema.#VaultRef`
- `schema.#NixFlake`
- `--output-format`
- `cuenv ci --generate`
- `cuenv sync ignore`
- `cuenv sync codeowners`
- cubes terminology for codegen

Adversarial prompts:

- "Does every schema definition have a matrix row?" Run the check and inspect missing rows.
- "Can agents use this new schema field?" Require matrix status, docs, examples, and skill updates.
- "Are prompts teaching current CUE?" Search prompts for stale schema names and command flags.
