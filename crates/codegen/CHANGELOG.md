# Changelog

All notable changes to cuenv-codegen will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- Renamed "Blueprint" to "Cube" throughout the crate ("CUE Cubes" - much cooler name!)
- `Blueprint` struct is now `Cube`
- `BlueprintData` struct is now `CubeData`
- `blueprint` module is now `cube`
- Error variant `CodegenError::Blueprint` is now `CodegenError::Cube`

### Added
- Initial implementation of cuenv-codegen
- CUE Cube loading and evaluation
- File generation with managed vs scaffold modes
- JSON code formatting
- CUE schemas for multiple languages (TypeScript, Rust, Go, Python, etc.)
- Config generation for biome.json, .prettierrc, rustfmt.toml
- Example cube for Node.js API project
- Comprehensive test coverage

### Features
- Schema-wrapped code blocks using CUE
- Format configuration embedded in schemas
- Automatic formatter config generation
- Support for conditional file generation
- File mode handling (managed vs scaffold)

### Documentation
- README with overview and examples
- Inline documentation for all public APIs
- Example cubes

## [0.14.0] - 2024-12-14

Initial release (Phase 1 implementation)
