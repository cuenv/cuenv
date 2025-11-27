---
title: Troubleshooting
description: Common issues and solutions for cuenv
---

This guide covers common issues you may encounter when using cuenv and how to resolve them.

## Installation Issues

### Command Not Found

If `cuenv` is not recognized after installation:

```bash
# Ensure cargo bin is in PATH
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc

# Verify installation
which cuenv
cuenv version
```

### Build Failures from Source

**Missing Rust toolchain:**

```bash
# Install or update Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update
```

**Missing Go (required for CUE engine):**

The cuengine crate requires Go for the CUE evaluation bridge. Ensure Go 1.21+ is installed:

```bash
# Check Go version
go version

# Install via your package manager or https://go.dev/dl/
```

**OpenSSL/TLS errors:**

```bash
# Ubuntu/Debian
sudo apt install pkg-config libssl-dev

# Fedora/RHEL
sudo dnf install openssl-devel

# macOS
brew install openssl
```

## CUE Evaluation Errors

### Syntax Errors

CUE syntax errors will show the file and line number:

```
error: CUE evaluation failed
  --> env.cue:5:10
    |
  5 |     PORT: 8080
    |          ^^^^ expected string, found int
```

**Common fixes:**

- Environment variables must be strings: use `PORT: "8080"` not `PORT: 8080`
- Check for missing commas between fields
- Ensure proper brace matching

### Constraint Violations

When values don't match constraints:

```
error: constraint violation
  NODE_ENV: "invalid" does not satisfy "development" | "staging" | "production"
```

**Fix:** Ensure your values match the defined constraints in your schema.

### Import Errors

```
error: cannot find package "github.com/myorg/schemas"
```

**Fixes:**

1. Ensure the CUE module is properly initialized:

   ```bash
   cue mod init github.com/myorg/myproject
   ```

2. Check that imported packages exist in `cue.mod/`

3. For external dependencies, ensure they're fetched:
   ```bash
   cue get go github.com/myorg/schemas
   ```

### Circular References

```
error: circular reference detected
  a -> b -> a
```

CUE does not allow circular references. Restructure your configuration to eliminate cycles.

## Shell Integration Issues

### Hooks Not Triggering

If environment hooks don't run when entering a directory:

1. **Verify shell integration is loaded:**

   ```bash
   # Check if cuenv functions exist
   type _cuenv_hook 2>/dev/null || echo "Shell integration not loaded"
   ```

2. **Re-source your shell config:**

   ```bash
   # Bash
   source ~/.bashrc

   # Zsh
   source ~/.zshrc

   # Fish
   source ~/.config/fish/config.fish
   ```

3. **Regenerate shell integration:**
   ```bash
   # Remove old integration and regenerate
   cuenv shell init bash >> ~/.bashrc
   ```

### Configuration Not Approved

cuenv requires explicit approval before executing hooks for security:

```
error: configuration not approved
  Run 'cuenv allow' to approve this configuration
```

**Fix:**

```bash
# Review the configuration first
cat env.cue

# Approve if it looks safe
cuenv allow

# Or approve with a note
cuenv allow --note "Reviewed on 2025-01-15"
```

### Environment Variables Not Loading

1. **Check if env.cue exists and is valid:**

   ```bash
   cuenv env print
   ```

2. **Verify the package name:**

   ```bash
   # Default package is 'cuenv'
   cuenv env print --package cuenv
   ```

3. **Check for evaluation errors:**
   ```bash
   cuenv env check
   ```

## Task Execution Issues

### Task Not Found

```
error: task 'build' not found
```

**Fixes:**

1. List available tasks:

   ```bash
   cuenv task
   ```

2. Check task is defined in `env.cue`:

   ```cue
   tasks: {
       build: {
           command: "npm"
           args: ["run", "build"]
       }
   }
   ```

3. Verify correct working directory:
   ```bash
   pwd
   ls env.cue
   ```

### Task Dependency Failures

When a task fails due to dependency:

```
error: task 'build' failed: dependency 'test' exited with code 1
```

**Fix:** Run the failing dependency directly to see full output:

```bash
cuenv task test
```

### Command Not Found in Task

```
error: task 'lint' failed: command 'eslint' not found
```

The command must be available in your PATH. Options:

1. Install the missing tool globally
2. Use Nix integration to provision tools (when available)
3. Use full path to the command:
   ```cue
   tasks: {
       lint: {
           command: "./node_modules/.bin/eslint"
           args: ["."]
       }
   }
   ```

## Performance Issues

### Slow CUE Evaluation

Large CUE configurations can be slow to evaluate. Tips:

1. **Enable caching:**

   ```bash
   cuenv task build --cache read-write
   ```

2. **Split large configurations** into smaller, focused files

3. **Avoid complex computed values** where possible

### High Memory Usage

If cuenv uses excessive memory:

1. Check for recursive or deeply nested structures
2. Simplify constraint expressions
3. Report the issue with a minimal reproduction case

## Debug Mode

Enable verbose logging for debugging:

```bash
# Set log level
cuenv --level debug task build

# Or trace for maximum detail
cuenv --level trace env print
```

## Getting Help

If you can't resolve an issue:

1. **Search existing issues:** [GitHub Issues](https://github.com/cuenv/cuenv/issues)

2. **Create a new issue** with:

   - cuenv version (`cuenv version`)
   - Operating system and version
   - Complete error message
   - Minimal `env.cue` to reproduce

3. **Join the discussion:** [GitHub Discussions](https://github.com/cuenv/cuenv/discussions)

## See Also

- [Installation Guide](/installation/)
- [Configuration Guide](/configuration/)
- [Contributing Guide](/contributing/)
