---
title: CLI Reference
description: Command-line interface for cuenv
---

The `cuenv` CLI provides tools for managing environments, executing tasks, and integrating with your shell.

## Global Options

| Option              | Description                                         | Default |
| ------------------- | --------------------------------------------------- | ------- |
| `--level, -L`       | Set logging level (trace, debug, info, warn, error) | warn    |
| `--json`            | Emit JSON envelope regardless of format             | false   |
| `--environment, -e` | Apply environment-specific overrides                | none    |

:::caution[Breaking Change in 0.16.0]
The short flag for `--level` changed from `-l` to `-L`. The `-l` short flag is now used for `--label` in task execution. Update any scripts using `cuenv -l debug` to `cuenv -L debug`.
:::

## Commands

### `cuenv version`

Show version information.

```bash
cuenv version [OPTIONS]
```

**Options:**

- `--output-format <FORMAT>`: Output format (simple, json, env). Default: simple.

### `cuenv env`

Environment variable operations.

#### `cuenv env print`

Print environment variables from CUE package.

```bash
cuenv env print [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--output-format <FORMAT>`: Output format (env, json, simple). Default: `env`

#### `cuenv env load`

Load environment and execute hooks in background.

```bash
cuenv env load [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`

#### `cuenv env check`

Check hook status and output environment for shell.

```bash
cuenv env check [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--shell <SHELL>`: Shell type for export format (bash, zsh, fish). Default: `bash`

### `cuenv task`

Execute a task defined in CUE configuration.

```bash
cuenv task [NAME] [OPTIONS] [-- TASK_ARGS...]
```

**Arguments:**

- `[NAME]`: Name of the task to execute. If not provided, lists available tasks.
- `[TASK_ARGS]`: Arguments to pass to the task (positional and --named values).

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `-l, --label <LABEL>`: Execute all tasks matching given labels (repeatable, AND semantics).
- `--output-format <FORMAT>`: Output format when listing tasks (simple, json). Default: `simple`
- `--materialize-outputs <DIR>`: Materialize cached outputs to this directory on cache hit.
- `--show-cache-path`: Print the cache path for this task key.
- `--backend <BACKEND>`: Force specific execution backend (`host` or `dagger`).
- `--help`: Print task-specific help (when task name is provided).

**Label-based execution:**

You can execute multiple tasks at once using labels:

```bash
# Execute all tasks with the 'test' label
cuenv task -l test

# Execute tasks matching both 'test' AND 'unit' labels
cuenv task -l test -l unit
```

Labels are defined in your CUE task configuration and allow grouping related tasks across projects.

:::tip
Use the global `-e` flag to apply environment-specific overrides: `cuenv -e production task build`
:::

### `cuenv exec`

Execute a command with CUE environment variables.

```bash
cuenv exec [OPTIONS] -- <COMMAND> [ARGS]...
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`

:::tip
Use the global `-e` flag to apply environment-specific overrides: `cuenv -e production exec -- npm start`
:::

### `cuenv shell`

Shell integration commands.

#### `cuenv shell init`

Generate shell integration script.

```bash
cuenv shell init <SHELL>
```

**Arguments:**

- `<SHELL>`: Shell type (fish, bash, zsh).

#### `cuenv env status`

Show hook execution status.

```bash
cuenv env status [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--wait`: Wait for hooks to complete before returning.
- `--timeout <SECONDS>`: Timeout in seconds for waiting. Default: `300`
- `--output-format <FORMAT>`: Output format (text, short, starship). Default: `text`

#### `cuenv env inspect`

Inspect cached hook state for the current config.

```bash
cuenv env inspect [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`

### `cuenv env list`

List available environments defined in your configuration.

```bash
cuenv env list [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--output-format <FORMAT>`: Output format (simple, json). Default: `simple`

### `cuenv ci`

Run CI pipelines defined in your CUE configuration.

```bash
cuenv ci [OPTIONS]
```

**Options:**

- `--dry-run`: Show what would be executed without running it.
- `--pipeline <NAME>`: Force a specific pipeline to run.
- `--generate <PROVIDER>`: Generate static CI workflow files (e.g., `github`, `buildkite`).
- `--format <FORMAT>`: Output dynamic pipeline in specified format (`buildkite`, `github`).
- `--from <REF>`: Base ref to compare against (branch name or commit SHA) for affected detection.
- `--force`: Overwrite existing workflow files when generating.
- `--check`: Check if CI workflows are in sync without writing files.

**Example:**

```bash
# Run CI pipeline
cuenv ci

# See what would run without executing
cuenv ci --dry-run

# Generate GitHub Actions workflow files
cuenv ci --format github

# Check if workflows are in sync (useful for CI validation)
cuenv ci --format github --check

# Generate static Buildkite bootstrap pipeline
cuenv ci --generate buildkite

# Output dynamic Buildkite pipeline (pipe to buildkite-agent)
cuenv ci --format buildkite | buildkite-agent pipeline upload

# Compare against a specific base ref
cuenv ci --from main
```

**Workflow Generation:**

cuenv can generate CI workflow files for different providers:

- **GitHub Actions**: Creates `.github/workflows/*.yml` files with monorepo-aware naming
- **Buildkite**: Creates `.buildkite/pipeline.yml` bootstrap or outputs dynamic YAML

The `--check` flag validates that generated workflows match existing files, exiting with an error if they differ. This is useful for enforcing workflow consistency in CI.

### `cuenv sync`

Sync generated files from CUE configuration. When run without a subcommand, executes all sync operations.

```bash
cuenv sync [OPTIONS] [SUBCOMMAND]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--dry-run`: Show what would be generated without creating files.
- `--check`: Check if files are in sync without making changes (exits with error if out of sync).
- `-A, --all`: Sync all projects in the workspace.

**Subcommands:**

- `ignore`: Generate ignore files only (.gitignore, .dockerignore, etc.)
- `codeowners`: Sync CODEOWNERS file from CUE configuration
- `cubes`: Sync files from CUE cube configurations

**Example:**

```bash
# Run all sync operations
cuenv sync

# Generate only ignore files
cuenv sync ignore

# Preview what would be generated
cuenv sync --dry-run

# Check if all files are in sync (for CI)
cuenv sync --check

# Sync CODEOWNERS file
cuenv sync codeowners

# Sync cubes with diff output
cuenv sync cubes --diff

# Sync from a specific directory
cuenv sync --path ./project

# Sync all projects in the workspace
cuenv sync --all
```

**Output Status:**

- `Created` - New file was created
- `Updated` - Existing file was updated
- `Unchanged` - File content unchanged, no write needed

**Security:**

- Must be run within a Git repository
- Tool names cannot contain path separators or `..`
- Files are only written within the Git repository

**Configuration:**

Add an `ignore` field to your `env.cue`:

```cue
ignore: {
    // Simple format: list of patterns
    git: ["node_modules/", ".env", "*.log"]
    docker: ["node_modules/", ".git/", "target/"]

    // Extended format: custom filename
    custom: {
        patterns: ["*.tmp", "cache/"]
        filename: ".myignore"
    }
}
```

Tool names map to ignore files as `.{tool}ignore` (e.g., `git` creates `.gitignore`, `docker` creates `.dockerignore`). Use the extended format with `filename` to override this default.

See the [Ignore Files guide](/how-to/ignore-files/) for more details.

### `cuenv tui`

Start an interactive TUI dashboard for monitoring cuenv events.

```bash
cuenv tui
```

The TUI connects to a running cuenv coordinator to display real-time events from other cuenv commands. To use:

1. Run a cuenv command (e.g., `cuenv task build`) in another terminal
2. Run `cuenv tui` to watch the events

### `cuenv web`

Start a web server for streaming cuenv events.

```bash
cuenv web [OPTIONS]
```

**Options:**

- `-p, --port <PORT>`: Port to listen on. Default: `3000`
- `--host <HOST>`: Host to bind to. Default: `127.0.0.1`

### `cuenv changeset`

Manage changesets for release management.

#### `cuenv changeset add`

Add a new changeset.

```bash
cuenv changeset add [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to project root. Default: `.`
- `-s, --summary <SUMMARY>`: Summary of the change (required).
- `-d, --description <DESC>`: Detailed description of the change.
- `-P, --packages <PKG:BUMP>`: Package and bump type (format: `package:bump`, e.g., `my-pkg:minor`). Can be specified multiple times.

**Example:**

```bash
cuenv changeset add -s "Add new feature" -P my-pkg:minor -P other-pkg:patch
```

#### `cuenv changeset status`

Show pending changesets.

```bash
cuenv changeset status [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to project root. Default: `.`

### `cuenv release`

Release management operations.

#### `cuenv release version`

Calculate and apply version bumps from changesets.

```bash
cuenv release version [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to project root. Default: `.`
- `--dry-run`: Show what would change without making changes.

#### `cuenv release publish`

Publish packages in topological order.

```bash
cuenv release publish [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to project root. Default: `.`
- `--dry-run`: Show what would be published without publishing.

### `cuenv secrets`

Manage secret provider integrations.

#### `cuenv secrets setup`

Set up a secret provider by downloading required components.

```bash
cuenv secrets setup <PROVIDER> [OPTIONS]
```

**Arguments:**

- `<PROVIDER>`: Provider to set up. Currently supported: `onepassword`

**Options:**

- `--wasm-url <URL>`: Override the default WASM URL (for 1Password).

**Example:**

```bash
# Set up 1Password WASM SDK for HTTP mode
cuenv secrets setup onepassword
```

This downloads the 1Password WASM SDK to enable HTTP-based secret resolution. When `OP_SERVICE_ACCOUNT_TOKEN` is set, cuenv uses this for faster, batched secret resolution instead of the `op` CLI.

See the [Secrets Guide](/how-to/secrets/) for more details on secret management.

### Security Commands

#### `cuenv allow`

Approve configuration for hook execution.

```bash
cuenv allow [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--note <NOTE>`: Optional note about this approval.
- `-y, --yes`: Approve without prompting.

#### `cuenv deny`

Revoke approval for hook execution.

```bash
cuenv deny [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--all`: Revoke all approvals for this directory.

### `cuenv completions`

Generate shell completion setup instructions.

```bash
cuenv completions <SHELL>
```

**Arguments:**

- `<SHELL>`: Shell type (bash, zsh, fish, elvish, powershell).

**Example:**

```bash
# Show setup instructions for fish
cuenv completions fish
```

## Shell Completions

cuenv provides dynamic shell completions that include task names with descriptions directly from your CUE configuration. The completions are always up-to-date because they query your project's configuration at completion time.

### Setup

Add one of the following lines to your shell configuration file:

#### Bash

Add to `~/.bashrc`:

```bash
source <(COMPLETE=bash cuenv)
```

#### Zsh

Add to `~/.zshrc`:

```zsh
source <(COMPLETE=zsh cuenv)
```

#### Fish

Add to `~/.config/fish/config.fish`:

```fish
COMPLETE=fish cuenv | source
```

#### Elvish

Add to `~/.elvish/rc.elv`:

```text
eval (E:COMPLETE=elvish cuenv | slurp)
```

#### PowerShell

Add to your `$PROFILE`:

```powershell
$env:COMPLETE = "powershell"; cuenv | Out-String | Invoke-Expression; Remove-Item Env:\COMPLETE
```

### Features

Once completions are set up, you get:

- **Command completion**: Tab-complete all cuenv commands and options
- **Task name completion**: Tab-complete task names defined in your CUE configuration
- **Task descriptions**: See task descriptions in completion suggestions (shell dependent)

**Example:**

```bash
# Type 'cuenv task ' then press Tab to see available tasks
cuenv task <TAB>
# Shows: build  test  lint  deploy  ...

# Type 'cuenv -e ' then press Tab to see environment options
cuenv -e <TAB>
# Shows: development  staging  production  ...
```

:::note
We recommend re-sourcing completions on upgrade. The completion system calls the cuenv binary during completion, so the shell setup and binary should stay in sync.
:::
