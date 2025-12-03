---
title: CLI Reference
description: Command-line interface for cuenv
---

The `cuenv` CLI provides tools for managing environments, executing tasks, and integrating with your shell.

## Global Options

| Option              | Description                                         | Default |
| ------------------- | --------------------------------------------------- | ------- |
| `--level, -l`       | Set logging level (trace, debug, info, warn, error) | warn    |
| `--json`            | Emit JSON envelope regardless of format             | false   |
| `--environment, -e` | Apply environment-specific overrides                | none    |

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
cuenv task [NAME] [OPTIONS]
```

**Arguments:**

- `[NAME]`: Name of the task to execute. If not provided, lists available tasks.

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--materialize-outputs <DIR>`: Materialize cached outputs to this directory on cache hit.
- `--show-cache-path`: Print the cache path for this task key.

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

### `cuenv ci`

Run CI pipelines defined in your CUE configuration.

```bash
cuenv ci [OPTIONS]
```

**Options:**

- `--dry-run`: Show what would be executed without running it.
- `--pipeline <NAME>`: Force a specific pipeline to run.
- `--generate <PROVIDER>`: Generate CI workflow file (currently only `github` is supported).

**Example:**

```bash
# Run CI pipeline
cuenv ci

# See what would run without executing
cuenv ci --dry-run

# Generate GitHub Actions workflow
cuenv ci --generate github
```

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

```elvish
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
