# Changelog

All notable changes to the cuenv CLI will be documented in this file.

## [Unreleased]

### Features

- Add label-based task execution with `--label/-l` flag ([#178](https://github.com/cuenv/cuenv/pull/178))
  - Execute all tasks matching given labels using AND semantics
  - Discover tasks across all projects in CUE module scope
  - Repeatable flag: `-l test -l unit` executes tasks with both labels

### Breaking Changes

- **Log level short flag changed from `-l` to `-L`** ([#178](https://github.com/cuenv/cuenv/pull/178))
  - The `-l` short flag is now used for `--label`
  - Update any scripts using `cuenv -l debug` to `cuenv -L debug`
