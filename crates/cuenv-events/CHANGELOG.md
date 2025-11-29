# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.0](https://github.com/cuenv/cuenv/releases/tag/cuenv-events-v0.6.0) - 2025-11-29

### Added

- implement structured event system for multi-UI support

### Fixed

- correct event bus test to match actual behavior
- address PR review comments and CI failures
- *(cli)* flush stderr on error and remove redundant error logging
- work on docs
- preparinmg to publish
- resolve circular dependency between cuenv-core and cuengine ([#33](https://github.com/cuenv/cuenv/pull/33))
- ensure codecov will use token

### Other

- overhaul documentation website and guides
- forcing rebuild with superficial change
- treefmt all the things
