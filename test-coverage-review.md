# Test Coverage Review Checklist

This checklist tracks systematic review of each crate's test coverage.
Updated automatically by the Ralph Wiggum coverage script.

## Review Criteria

For each crate, verify:
1. **Existing tests are meaningful** - not just smoke tests
2. **Critical paths are covered** - error handling, edge cases
3. **Tests match the code's intent** - testing behavior, not implementation
4. **No missing test scenarios** - happy path, error path, boundary conditions

## Crate Review Status (sorted by coverage, lowest first)

| Crate | Coverage | Reviewed | Tests Added | Notes |
|-------|----------|----------|-------------|-------|
| dagger | 0.0% | [x] | [x] | Added 8 tests for constructor, accessors, and factory |
| aws | 11.8% | [x] | [x] | Added 15 tests for config, JSON extraction, error handling |
| 1password | 18.1% | [x] | [x] | Added 32 tests for config, WASM utils, host functions, resolver |
| vault | 25.8% | [x] | [x] | Added 12 tests for config, path handling, serialization |
| homebrew | 43.6% | [x] | [x] | Added 23 tests for formula generation, config, backend |
| gcp | 44.4% | [x] | [x] | Added 17 tests for config, resource name parsing, resolver |
| tools/oci | 52.8% | [x] | [x] | Added 34 tests for error, platform, cache modules |
| cuenv | 53.9% | [x] | [x] | Added 38 tests: coordinator protocol (16), discovery (6), CLI (16) |
| ignore | 54.3% | [x] | [x] | Added 18 tests for builder, validation, error types |
| secrets | 72.5% | [x] | [x] | Added 14 tests for SecretError, SecretSpec in lib.rs |
| editorconfig | 72.9% | [x] | [x] | Added 19 tests for optional builders, file ops, error handling |
| events | 74.0% | [x] | [x] | Added 54 tests: bus (18), event (36 for all event types) |
| ci | 74.2% | [x] | [x] | Added 26 tests: context (7), flake/error (10), provider/local (9) |
| github | 75.4% | [x] | [x] | Added 10 tests: config (Default, serde, merge, permissions) |
| release | 77.2% | [x] | [x] | Added 12 tests: backends (BackendContext, PublishResult builders) |
| cubes | 79.8% | [x] | [x] | Added 60 tests: lib.rs (5 error types), cube.rs (24 for FileMode, FormatConfig, accessors, CUE loading), formatter.rs (12 for all languages, edge cases), generator.rs (12 for scaffold/managed modes, check mode), config.rs (15 for biome/prettier/rustfmt configs) |
| core | 83.6% | [ ] | [ ] | Pending review (large crate: 10161 lines) |
| workspaces | 84.3% | [ ] | [ ] | Pending review |
| buildkite | 87.0% | [ ] | [ ] | Pending review |
| cuengine | 87.6% | [ ] | [ ] | Pending review |
| codeowners | 90.4% | [ ] | [ ] | Pending review |
| bitbucket | 94.5% | [ ] | [ ] | Pending review |
| gitlab | 95.2% | [ ] | [ ] | Pending review |

## Session Log

<!-- Claude will append notes here as it reviews each crate -->
