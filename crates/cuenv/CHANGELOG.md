# Changelog

All notable changes to the cuenv CLI will be documented in this file.

## [Unreleased]

### Features

- Add label-based task execution with `--label/-l` flag ([#178](https://github.com/cuenv/cuenv/pull/178))
  - Execute all tasks matching given labels using AND semantics
  - Discover tasks across all projects in CUE module scope
  - Repeatable flag: `-l test -l unit` executes tasks with both labels

- **Secrets**: Add 1Password WASM SDK integration for HTTP mode
  - New `cuenv secrets setup onepassword` command downloads WASM SDK
  - When `OP_SERVICE_ACCOUNT_TOKEN` is set, uses HTTP mode for faster batch resolution
  - Secrets are resolved at runtime in `env print` command
  - Secret values are automatically redacted from command output

- **CI**: Enhanced workflow generation and validation
  - Add `--format` option for dynamic pipeline output (`buildkite`, `github`)
  - Add `--check` flag to validate workflows are in sync without writing
  - Add `--from` flag for custom base ref in affected detection
  - Add `--force` flag to overwrite existing workflow files
  - Auto-inject 1Password WASM setup step in GitHub workflows
  - Monorepo-ready workflow naming with project prefix
  - Add environment field for pipelines for secret resolution

- **Sync**: Add `--check` flag across sync commands for CI validation
  - `cuenv sync --check` exits with error if files are out of sync
  - `cuenv sync codeowners --check` validates CODEOWNERS file
  - `cuenv sync cubes --diff` shows unified diff for changed files

- Add cache index display in task list output

### Bug Fixes

- Resolve secrets in `env print` command
- Redact secret values from exec command output
- Improve Buildkite changed files detection for shallow clones

### Breaking Changes

- **Log level short flag changed from `-l` to `-L`** ([#178](https://github.com/cuenv/cuenv/pull/178))
  - The `-l` short flag is now used for `--label`
  - Update any scripts using `cuenv -l debug` to `cuenv -L debug`

### Internal

- Migrate from OpenSSL to rustls for TLS
- Apply Rust coding standards from CLEANUP.md
- Remove disabled hermetic execution paths
