# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.5](https://github.com/cuenv/cuenv/releases/tag/cuenv-dagger-v0.8.5) - 2025-12-06

### Added

- *(ci)* support local changes against references with --from
- add support for task arguments (positional and named)
- dagger backend support working

### Fixed

- address PR review feedback
- *(cli)* flush stderr on error and remove redundant error logging
- work on docs
- preparinmg to publish
- resolve circular dependency between cuenv-core and cuengine ([#33](https://github.com/cuenv/cuenv/pull/33))
- ensure codecov will use token

### Other

- release v0.7.1
- overhaul documentation website and guides
- forcing rebuild with superficial change
- treefmt all the things

## [0.7.1](https://github.com/cuenv/cuenv/releases/tag/cuenv-dagger-v0.7.1) - 2025-12-01

### Added

- dagger backend support working

### Fixed

- _(cli)_ flush stderr on error and remove redundant error logging
- work on docs
- preparing to publish
- resolve circular dependency between cuenv-core and cuengine ([#33](https://github.com/cuenv/cuenv/pull/33))
- ensure codecov will use token

### Other

- overhaul documentation website and guides
- forcing rebuild with superficial change
- treefmt all the things
