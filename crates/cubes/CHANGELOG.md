# Changelog

All notable changes to cuenv-cubes will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.14.2] - Unreleased

### Added

- Initial release of cuenv-cubes
- `schema.#Cube` schema in `/schema/cubes.cue`
- `cuenv sync cubes` CLI command
- Support for managed (always regenerate) and scaffold (create once) file modes
- Language schemas: TypeScript, JavaScript, JSON, JSONC, YAML, TOML, Rust, Go, Python, Markdown, Shell, Dockerfile, Nix
- Project discovery for syncing cubes across entire CUE module
- Auto-detection of CUE package name from env.cue
