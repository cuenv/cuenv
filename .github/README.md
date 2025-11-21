# GitHub Configuration

This directory contains GitHub-specific configuration files for the cuenv repository.

## Copilot Configuration

The repository is configured with comprehensive GitHub Copilot instructions to help the coding agent work effectively.

### Structure

```
.github/
├── copilot-instructions.md        # Main Copilot instructions (repository-wide)
├── copilot-setup-steps.yaml      # Automated environment setup
├── instructions/                  # Scoped instructions for specific file types
│   ├── rust-code.instructions.md
│   ├── cue-config.instructions.md
│   └── testing.instructions.md
└── agents/                        # Custom agent profiles
    ├── rust-expert.md
    ├── go-cue-expert.md
    ├── documentation-expert.md
    └── testing-expert.md
```

### Copilot Instructions

**Main Instructions** (`copilot-instructions.md`):
- Repository overview and architecture
- Build and test procedures with timing expectations
- Development workflow with Nix
- Common tasks and validation scenarios
- Troubleshooting guide

**Setup Steps** (`copilot-setup-steps.yaml`):
- Automated environment verification
- Dependency installation steps
- Build and test validation
- Environment variable configuration

**Scoped Instructions** (`instructions/`):
These provide detailed guidance for specific file types and are automatically applied when working with matching files:

- `rust-code.instructions.md` - Applied to `crates/**/*.rs`
  - Rust coding standards and conventions
  - FFI safety guidelines
  - Testing requirements
  - Performance considerations

- `cue-config.instructions.md` - Applied to `**/*.cue` and `schema/**/*`
  - CUE language best practices
  - Schema design guidelines
  - Configuration patterns
  - Validation strategies

- `testing.instructions.md` - Applied to `**/tests/**/*` and test files
  - Testing strategies and patterns
  - Test organization
  - Coverage requirements
  - Performance testing

### Custom Agents

Custom agents are specialized profiles that can be assigned specific tasks. They have domain expertise and follow specific guidelines:

- **Rust Expert** (`agents/rust-expert.md`)
  - Rust code development and FFI integration
  - Memory safety and performance optimization
  - Scope: Rust source files and Cargo configuration

- **Go and CUE Expert** (`agents/go-cue-expert.md`)
  - Go FFI bridge development
  - CUE language integration and schema design
  - Scope: Go bridge code and CUE files

- **Documentation Expert** (`agents/documentation-expert.md`)
  - Technical writing and documentation maintenance
  - API documentation and examples
  - Scope: Markdown files and documentation

- **Testing Expert** (`agents/testing-expert.md`)
  - Test development and quality assurance
  - Test coverage and performance testing
  - Scope: Test files and test infrastructure

## How It Works

### For Copilot
When GitHub Copilot coding agent works on this repository:

1. It reads `copilot-instructions.md` for repository-wide context
2. It uses `copilot-setup-steps.yaml` to set up the environment
3. It applies scoped instructions based on file patterns
4. It can leverage custom agents for specialized tasks

### For Developers
Developers benefit from:

- Clear, documented workflows and standards
- Automated environment setup verification
- Consistent code quality through enforced guidelines
- Domain-specific expertise through custom agents

## Best Practices

When updating Copilot instructions:

1. **Keep instructions current** - Update when code patterns or workflows change
2. **Be specific** - Provide concrete examples and commands
3. **Test thoroughly** - Verify instructions work as documented
4. **Maintain consistency** - Use consistent terminology across files
5. **Document timing** - Include expected duration for long-running operations
6. **Provide context** - Explain why, not just how

## Maintenance

### When to Update

Update Copilot instructions when:
- Build or test procedures change
- New coding patterns are adopted
- Project structure is reorganized
- Common issues are encountered
- Dependencies are updated
- New tools are introduced

### How to Update

1. Identify which file(s) need updates:
   - Global changes → `copilot-instructions.md`
   - Language-specific → `instructions/*.instructions.md`
   - Agent behavior → `agents/*.md`
   - Setup process → `copilot-setup-steps.yaml`

2. Make changes following markdown best practices
3. Test the instructions work as documented
4. Update examples if needed
5. Commit with descriptive message

## Additional Resources

- [GitHub Copilot Documentation](https://docs.github.com/en/copilot)
- [Copilot Best Practices](https://docs.github.com/en/copilot/tutorials/coding-agent/get-the-best-results)
- [Custom Instructions Guide](https://docs.github.com/en/copilot/how-tos/configure-custom-instructions)
- [Custom Agents Documentation](https://github.blog/ai-and-ml/github-copilot/)

## Workflows

This directory also contains GitHub Actions workflows:

- `ci.yml` - Continuous integration (build, test, lint)
- `release-please.yml` - Automated releases
- `deploy.yml` - Deployment automation
- `schema-check.yml` - CUE schema validation

## Dependabot

`dependabot.yml` configures automated dependency updates for:
- Cargo (Rust) dependencies
- GitHub Actions versions
- Go modules (if applicable)

---

For questions about Copilot configuration, see the [main README](../readme.md) or open a discussion.
