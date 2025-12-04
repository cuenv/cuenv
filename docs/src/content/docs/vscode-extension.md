---
title: VSCode Extension
description: Official VSCode integration for cuenv
---

The official Cuenv VSCode extension brings powerful IDE integration for managing environments, running tasks, and visualizing dependencies directly from your editor.

## Installation

### From VS Code Marketplace

Search for "Cuenv" in the VS Code Extensions panel or install via command line:

```bash
code --install-extension cuenv.cuenv-vscode
```

### Manual Installation

Download the `.vsix` file from the [GitHub releases](https://github.com/cuenv/cuenv/releases) and install:

```bash
code --install-extension cuenv-vscode-0.1.1.vsix
```

## Requirements

- The `cuenv` binary must be installed and available in your PATH (or configured in settings)
- A project with an `env.cue` file in the workspace

## Features

### Activity Bar Explorer

The extension adds a **Cuenv** panel to the Activity Bar with three views:

- **Tasks**: Browse and run all defined tasks
- **Environments**: Switch between environments (development, production, etc.)
- **Variables**: View environment variables for the selected environment

### Task Explorer

View all tasks defined in your CUE configuration. Each task displays with:

- Task name and hierarchy (nested tasks shown with dot notation)
- Inline **Run** button to execute the task
- Dependencies and metadata from task definitions

Click the play button or use the context menu to run any task in the integrated terminal.

### Environment Switcher

Quickly switch between environments defined in your configuration:

1. Open the **Environments** view in the Cuenv panel
2. Click on an environment to activate it
3. All subsequent task runs will use that environment's variables

The currently selected environment affects:
- Task execution context
- Variables displayed in the Variables view
- Environment variables passed to the terminal

### Variables View

Inspect environment variables for the currently selected environment:

- Variables are grouped and displayed with their values
- Secret values are masked for security
- Use the **Copy Variable** command to copy non-secret values to clipboard

### Task Dependency Graph

Visualize task dependencies with an interactive Mermaid-based graph:

1. Click the **Show Dependency Graph** icon in the Tasks view title bar
2. A webview opens showing all tasks and their relationships
3. Arrows indicate dependency direction (`dependsOn` relationships)

This helps understand complex build pipelines and task orchestration.

### CodeLens Integration

When editing `env.cue` files, inline **Run** buttons appear above task definitions:

```cue
tasks: {
    build: {  // ▶ Run build (CodeLens button appears here)
        command: "cargo"
        args: ["build", "--release"]
    }
}
```

Click the CodeLens to run that specific task without navigating to the sidebar.

### Auto-Refresh

The extension automatically refreshes when CUE files change:

- Watches all `*.cue` files in the workspace
- Also watches parent `env.cue` files (for monorepo setups)
- Updates Tasks, Environments, and Variables views on file save

Use the **Refresh** button in the Tasks view title bar to manually refresh.

## Commands

| Command | Description |
|---------|-------------|
| `Cuenv: Refresh` | Refresh all data from cuenv CLI |
| `Cuenv: Run Task` | Run a specific task |
| `Cuenv: Set Environment` | Switch to a different environment |
| `Cuenv: Show Dependency Graph` | Open the task dependency visualization |
| `Cuenv: Copy Variable` | Copy a variable value to clipboard |

Access commands via the Command Palette (`Cmd+Shift+P` / `Ctrl+Shift+P`).

## Configuration

Configure the extension in VS Code settings:

| Setting | Default | Description |
|---------|---------|-------------|
| `cuenv.executablePath` | `cuenv` | Path to the cuenv executable |

Example `settings.json`:

```json
{
    "cuenv.executablePath": "/usr/local/bin/cuenv"
}
```

## Troubleshooting

### Extension not activating

The extension activates when:
- A CUE file is opened (`onLanguage:cue`)
- The workspace contains an `env.cue` file

Ensure your project has a valid `env.cue` in the root.

### Tasks not appearing

1. Verify `cuenv task` works in your terminal
2. Check the **Cuenv** output channel for errors (View > Output > select "Cuenv")
3. Ensure `cuenv.executablePath` points to a valid cuenv binary

### Environment variables not loading

1. Verify `cuenv env print` works in your terminal
2. Check that the selected environment exists in your configuration
3. The "Base" environment is always available as the default

## Recommended Extensions

For the best experience, also install:

- **[CUE Language Support](https://marketplace.visualstudio.com/items?itemName=cue-lang.vscode-cue)**: Syntax highlighting, formatting, and validation for CUE files
