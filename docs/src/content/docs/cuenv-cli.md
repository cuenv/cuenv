---
title: CLI Reference
description: Command-line interface for cuenv
---

The `cuenv` CLI provides tools for managing environments, executing tasks, and integrating with your shell.

## Global Options

| Option              | Description                                     | Default |
| ------------------- | ----------------------------------------------- | ------- |
| `--level, -l`       | Set logging level (trace, debug, info, warn, error) | warn    |
| `--json`            | Emit JSON envelope regardless of format         | false   |

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
- `-e, --env <ENVIRONMENT>`: Apply environment-specific overrides (e.g., development, production).
- `--materialize-outputs <DIR>`: Materialize cached outputs to this directory on cache hit.
- `--show-cache-path`: Print the cache path for this task key.

### `cuenv exec`

Execute a command with CUE environment variables.

```bash
cuenv exec [OPTIONS] -- <COMMAND> [ARGS]...
```

**Options:**
- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `-e, --env <ENVIRONMENT>`: Apply environment-specific overrides.

### `cuenv shell`

Shell integration commands.

#### `cuenv shell init`

Generate shell integration script.

```bash
cuenv shell init <SHELL>
```

**Arguments:**
- `<SHELL>`: Shell type (fish, bash, zsh).

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
