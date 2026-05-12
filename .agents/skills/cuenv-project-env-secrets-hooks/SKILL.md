---
name: cuenv-project-env-secrets-hooks
description: Use for cuenv project configuration, env blocks, environment policies, runtime secret references, passthrough variables, hooks, shell command shapes, and approval behavior. Covers schema/core.cue, schema/config.cue, schema/env.cue, schema/policy.cue, schema/secrets.cue, provider secret schemas, schema/hooks.cue, and schema/shell.cue.
---

# Project, Env, Secrets, Hooks

Start with `docs/design/specs/schema-coverage-matrix.md`, then read the relevant schema files:

- `schema/core.cue` and `schema/config.cue` for project and command defaults.
- `schema/env.cue` and `schema/policy.cue` for env values, policies, interpolation, and passthrough.
- `schema/secrets.cue`, `schema/onepassword.cue`, `schema/aws.cue`, `schema/gcp.cue`, and `schema/vault.cue` for secret shapes.
- `schema/hooks.cue` and `schema/shell.cue` for hooks and shell command variants.

Generation rules:

- Use `schema.#ExecSecret` for custom command secrets.
- Use `schema.#OnePasswordRef` for 1Password references.
- Treat `schema.#AwsSecret`, `schema.#GcpSecret`, and `schema.#VaultSecret` as schema-only unless the current matrix changes.
- Do not generate `schema.#Secret & { command: ... }`, `schema.#AWSSecretRef`, or `schema.#VaultRef`.
- Do not confuse task `#ScriptShell` with shell command schemas in `schema/shell.cue`.

Adversarial prompts:

- "Use Azure or Doppler secrets." Answer that no schema row exists unless added.
- "Use PowerShell hooks." Check matrix status before claiming support.
- "Pass through `GITHUB_REF_NAME` into a release task." Use `schema.#EnvPassthrough`.

