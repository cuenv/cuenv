---
id: RFC-0002
title: Output Formatting and Error Envelope Strategy
status: Draft
decision_date: 2025-09-25
approvers:
  - TBD
related_features:
  - features/cli/errors.feature:1
  - features/cli/help.feature:1
---

## Summary

This RFC documents the multi-format output strategy and error taxonomy for the cuenv CLI. It captures the semantics codified in [crates/cuenv-cli/src/cli.rs](crates/cuenv-cli/src/cli.rs:140), ensuring that human-friendly output, JSON automation hooks, and exit-code signalling remain predictable as the CLI evolves.

## Problem Statement

The CLI presently emits three output modes (`simple`, `env`, `json`) while simultaneously supporting a JSON envelope toggle (`--json`) and structured error types (`CliError`). Without explicit guidance:

- Downstream tooling cannot rely on stable JSON schemas.
- Users lack clarity on when secrets are redacted or how warnings are surfaced.
- Tests and documentation risk drifting from the implemented behaviour, especially for empty files such as [features/cli/errors.feature](features/cli/errors.feature:1).

Documenting the strategy provides a single source of truth that feature files and ADRs can reference.

## Goals

1. Define a contract for successful command output across supported formats.
2. Specify the JSON envelope schema for both success (`OkEnvelope`) and failure (`ErrorEnvelope`).
3. Clarify the mapping from logical error categories to exit codes and diagnostic presentation.
4. Ensure feature scenarios validate conformance without hardcoding implementation details.

## Non-goals

- Dictating shell-specific export formats (covered in [rfc-0005-environment-export-and-exec-invocation-contracts](/decisions/rfcs/rfc-0005-environment-export-and-exec-invocation-contracts/)).
- Revisiting the CLI command hierarchy (handled by [rfc-0001-cli-command-surface](/decisions/rfcs/rfc-0001-cli-command-surface/)).
- Changing existing default verbosity or logging behaviour.

## Proposed Approach

1. **Output Format Definitions**

   - `simple`: Human-readable text optimised for TUI output.
   - `env`: Line-delimited `KEY=VALUE` pairs, omitting secrets and invalid shell tokens.
   - `json`: Structured payload equivalent to `serde_json::Value`.
   - `--json` flag: Wrap output inside an envelope while respecting the chosen format for the `data` field.

2. **Error Envelope Contract**

   - Enumerate `CliError::{Config, Eval, Other}` as the canonical categories, each mapping to `code: config|eval|other`.
   - Provide optional `help` messages surfaced via miette when not in JSON mode.

3. **Exit Code Mapping**

   - Maintain the explicit mapping in [crates/cuenv-cli/src/cli.rs](crates/cuenv-cli/src/cli.rs:109).
   - Document future additions to the error taxonomy through ADR updates.

4. **Testing Strategy**

   - Expand BDD coverage in [features/cli/errors.feature](features/cli/errors.feature:1) to assert envelope structure, redaction rules, and exit codes.
   - Introduce snapshot tests to detect schema regressions.

5. **Documentation Alignment**
   - Update help and README snippets to reuse canonical examples defined here.

## Alternatives Considered

| Option                                                       | Outcome                  | Reason Rejected                                                                                                                                               |
| ------------------------------------------------------------ | ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Output dedicated JSON payloads per command without envelopes | Command-specific schemas | Lacked uniformity across commands, harder for automation                                                                                                      |
| Default to JSON for all operations                           | Machine-friendly         | Degraded human ergonomics and contradicted CLI norms                                                                                                          |
| Inline secrets in env output                                 | Convenience              | Violates security posture defined in [adr-0004-environment-export-filtering-policy](/decisions/adrs/adr-0004-environment-export-filtering-policy/) |

## Impact on Users

- Script authors can rely on a stable envelope structure.
- Error codes become self-documenting, aiding CI pipelines.
- Users toggling between human and machine modes experience consistent messaging.

## Migration Plan

- Publish this RFC for review alongside exemplar tests.
- Populate [features/cli/errors.feature](features/cli/errors.feature:1) with scenarios referencing the exported schema.
- On acceptance, emit ADR-0005 to freeze the taxonomy (already drafted).

## Features Alignment

| Feature Specification                                        | Coverage | Notes                                                         |
| ------------------------------------------------------------ | -------- | ------------------------------------------------------------- |
| [features/cli/errors.feature](features/cli/errors.feature:1) | Pending  | Will assert JSON envelopes, redaction, and exit codes.        |
| [features/cli/help.feature](features/cli/help.feature:1)     | Pending  | Help output must describe formatting flags based on this RFC. |

## Open Questions

1. Should we expose per-command schemas through an introspection command?
2. How do we communicate experimental error categories without breaking automation?
3. Do we need locale-aware formatting for human mode?

## Related Artifacts

| Artifact                                                                                                             | Purpose                                                    |
| -------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------- |
| [crates/cuenv-cli/src/cli.rs](crates/cuenv-cli/src/cli.rs:140)                                                       | Source of `OutputFormat`, envelopes, and error mappings.   |
| [adr-0005-cli-error-taxonomy-and-exit-codes](/decisions/adrs/adr-0005-cli-error-taxonomy-and-exit-codes/) | Ratified decision capturing the taxonomy defined here.     |
| [readme.md](readme.md:368)                                                                                           | Public documentation that will mirror the output contract. |
