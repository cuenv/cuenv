# Cuenv VSCode Extension

VSCode integration for [Cuenv](https://github.com/cuenv/cuenv).

## Features

- **Task Explorer:** View and run CUE-defined tasks directly from the sidebar.
- **Environment Switcher:** Easily toggle between environments (e.g., `development`, `production`).
- **Seamless Execution:** Runs tasks in the integrated terminal with the correct environment context.

## Requirements

- `cuenv` binary must be installed and available in your PATH (or configured in settings).
- A project with an `env.cue` file.

## Configuration

- `cuenv.executablePath`: Path to the `cuenv` executable (default: `cuenv`).
