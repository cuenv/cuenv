---
id: ADR-0005
title: CLI Error Taxonomy and Exit Codes
status: Accepted
decision_date: 2025-09-25
approvers:
  - Core Maintainers
related_features:
  - features/cli/errors.feature:1
  - features/cli/help.feature:1
supersedes: []
superseded_by: []
---

## Context

The cuenv CLI surfaces distinct error categories to users and automation. The implementation in [crates/cuenv-cli/src/cli.rs](crates/cuenv-cli/src/cli.rs:14) defines `CliError::{Config, Eval, Other}`, each mapping to a specific exit code and diagnostic presentation. To maintain consistency across commands and documentation, we must formally accept this taxonomy.

This ADR builds on [rfc-0002-output-formatting-and-error-envelope-strategy](/decisions/rfcs/rfc-0002-output-formatting-and-error-envelope-strategy/).

## Decision

1. **Error Categories**
   - `Config`: Represents CLI usage errors, configuration issues, and validation failures; MUST exit with code `2`.
   - `Eval`: Represents evaluation or FFI failures; MUST exit with code `3`.
   - `Other`: Represents unexpected errors; MUST also exit with code `3`.

2. **Diagnostic Presentation**
   - Human-readable output MUST use `miette` to render `CliError` values with optional help text.
   - JSON mode MUST wrap errors in `ErrorEnvelope` with fields `status: "error"` and `error.code` set to `config|eval|other`.

3. **Extensibility**
   - New error categories MUST be added by extending `CliError` and MUST document exit codes and envelope schema updates.
   - Deprecating an error type requires a future ADR referencing this decision.

4. **Consistency Across Commands**
   - All commands MUST convert internal errors to `CliError` variants before rendering, ensuring uniform exit codes.

## Consequences

- Automation (CI, SDKs) can reliably interpret exit codes and JSON envelopes.
- Users receive consistent messaging with actionable help text.
- Future additions must respect or supersede this taxonomy, preventing ad-hoc error handling.

## Alignment with Features

| Feature Scenario                                                       | Impact                                                                        |
| ---------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| [features/cli/errors.feature](features/cli/errors.feature:1) — Pending | Must verify exit codes, envelope structure, and redaction rules per this ADR. |
| [features/cli/help.feature](features/cli/help.feature:1) — Pending     | Help output should highlight error categories and exit codes.                 |

## Related Documents

- [rfc-0002-output-formatting-and-error-envelope-strategy](/decisions/rfcs/rfc-0002-output-formatting-and-error-envelope-strategy/)
- [crates/cuenv-cli/src/cli.rs](crates/cuenv-cli/src/cli.rs:14)
- [readme.md](readme.md:770)

## Status

Accepted — taxonomy implemented in the CLI and relied upon by integration tests.

## Notes

If telemetry or analytics are added later, they MUST reference these error codes to maintain cross-tool consistency.
