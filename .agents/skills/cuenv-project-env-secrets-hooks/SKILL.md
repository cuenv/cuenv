---
name: cuenv-project-env-secrets-hooks
description: Use for cuenv project configuration, env blocks, environment policies, runtime secret references, passthrough variables, hooks, shell command shapes, and approval behavior. Covers schema/core.cue, schema/config.cue, schema/env.cue, schema/policy.cue, schema/secrets.cue, provider secret schemas, schema/hooks.cue, and schema/shell.cue.
---

# Project, Env, Secrets, Hooks

Start with `docs/design/specs/schema-coverage-matrix.md`, then read the relevant schema files:

- `schema/core.cue` and `schema/config.cue` for project and command defaults.
- `schema/env.cue` and `schema/policy.cue` for env values, policies, interpolation, and passthrough.
- `schema/secrets.cue`, `schema/onepassword.cue`, `schema/infisical.cue`, `schema/aws.cue`, `schema/gcp.cue`, and `schema/vault.cue` for secret shapes.
- `schema/hooks.cue` and `schema/shell.cue` for hooks and shell command variants.
- `crates/core/src/environment/values.rs` for env value shapes, policy checks, interpolation, and secret-aware value resolution; `crates/core/src/environment.rs` for runtime environment merging, PATH lookup, and filtered env construction.
- `crates/core/src/secrets/mod.rs` owns the default secret registry. Keep its fallible `Result` API stable for optional resolver initialization while avoiding local lint suppressions.
- `crates/secrets/src/batch.rs` and `crates/secrets/src/resolved.rs` own runtime batch convenience APIs. Keep caller-facing secret maps generic over the caller's `BuildHasher`; the object-safe `SecretResolver` trait keeps the internal concrete map boundary for provider implementations. Batch env resolver tests should scope fixtures with `temp_env` instead of unsafe process-wide environment mutation.
- `crates/secrets/src/resolvers/exec.rs` owns the private JSON shape for `schema.#ExecSecret`; do not add public constructors for test-only setup.
- `crates/1password/src/secrets/core.rs` owns the WASM SDK host functions. Keep memory offsets checked against the `i64` host ABI boundary and Unix-time conversion saturating rather than cast-suppressed. WASM initialization integration tests should scope `HOME` and `ONEPASSWORD_WASM_PATH` with `temp_env` and return `Result` from fallible setup instead of reintroducing broad `expect_used` or unsafe process-wide mutation allowances.
- `crates/cuenv/src/commands/secrets.rs` owns `cuenv secrets setup` orchestration and provider preflight output. Keep setup messages on redacted output helpers and format download sizes without float casts.
- `crates/hooks/src/state/` for persistent hook execution state, marker files, cleanup, execution hashes, and integer duration display formatting; `crates/hooks/src/executor.rs` for hook execution orchestration and saturating elapsed-millisecond conversion; `crates/hooks/src/executor/source_environment.rs` for source-hook environment capture. Supervisor integration tests should scope `CUENV_EXECUTABLE` with `temp_env`, not unsafe process-wide mutation.
- Hook integration tests should share `crates/cuenv/tests/hook_test_support` for temporary CUE module setup, approval, and sandbox error handling; return `Result` from filesystem and command setup; use `Command::new(env!("CARGO_BIN_EXE_cuenv"))`; and report unexpected exits as errors instead of reintroducing file-level unwrap/expect allowances.
- `crates/cuenv/src/commands/hooks.rs` for CLI hook command orchestration; `crates/cuenv/src/commands/hooks/status.rs` for status/starship rendering; `crates/cuenv/src/commands/hooks/shell.rs` for `cuenv shell init` snippets. Hook-backed export wait progress lives in `crates/cuenv/src/commands/export/hooks.rs` and should use its stderr writer helper instead of raw print macros.
- `crates/ci/src/executor/hook_env.rs` for hook-backed environment assembly during CI task execution.
- `crates/ci/src/executor/task_env.rs` for CI task env precedence and passthrough handling.
- Interactive prompt and wait-progress CLI output is rendered in `crates/events/src/renderers/cli/interactive.rs`; keep approval-facing wording aligned there when prompt semantics change.

Generation rules:

- Use `schema.#ExecSecret` for custom command secrets.
- Use `schema.#OnePasswordRef` for 1Password references.
- Use `schema.#InfisicalSecret` for Infisical REST API references.
- Treat `schema.#AwsSecret`, `schema.#GcpSecret`, and `schema.#VaultSecret` as schema-only unless the current matrix changes.
- Do not generate `schema.#Secret & { command: ... }`, `schema.#AWSSecretRef`, or `schema.#VaultRef`.
- Do not confuse task `#ScriptShell` with shell command schemas in `schema/shell.cue`.

Adversarial prompts:

- "Use Azure or Doppler secrets." Answer that no schema row exists unless added.
- "Use PowerShell hooks." Check matrix status before claiming support.
- "Pass through `GITHUB_REF_NAME` into a release task." Use `schema.#EnvPassthrough`.
