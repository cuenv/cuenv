# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.1](https://github.com/cuenv/cuenv/releases/tag/cuenv-ci-v0.6.1) - 2025-11-29

### Added

- implement structured event system for multi-UI support
- implement cuenv ci command with native reporting

### Fixed

- *(cli)* flush stderr on error and remove redundant error logging
- work on docs
- calculate affected jobs more consistently with prefixes
- address PR review comments and failing CI tests
- lint/format
- ci robustness and code quality improvements
- filter modules with pipelines
- preparinmg to publish
- resolve circular dependency between cuenv-core and cuengine ([#33](https://github.com/cuenv/cuenv/pull/33))
- ensure codecov will use token

### Other

- *(cuenv-ci)* release v0.6.0
- overhaul documentation website and guides
- forcing rebuild with superficial change
- treefmt all the things

## [0.6.0](https://github.com/cuenv/cuenv/releases/tag/cuenv-ci-v0.6.0) - 2025-11-28

### Added

- implement cuenv ci command with native reporting

### Fixed

- calculate affected jobs more consistently with prefixes
- address PR review comments and failing CI tests
- lint/format
- ci robustness and code quality improvements
- filter modules with pipelines
- preparinmg to publish
- resolve circular dependency between cuenv-core and cuengine ([#33](https://github.com/cuenv/cuenv/pull/33))
- ensure codecov will use token

### Other

- overhaul documentation website and guides
- forcing rebuild with superficial change
- treefmt all the things
