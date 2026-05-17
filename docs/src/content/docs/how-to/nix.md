---
title: Nix Integration
description: Integration with Nix package manager and development environments
---

cuenv integrates with [Nix](https://nixos.org/) to provide reproducible development environments. When you have a `flake.nix` in your project, cuenv can automatically load the Nix development environment alongside your CUE configuration.

:::note
The Nix runtime and `#NixFlake` hook paths are implemented. Check the
[schema status](/reference/schema/status/) page before relying on other runtime
surfaces.
:::

## Overview

The Nix integration allows you to:

- Automatically load Nix flake development environments
- Combine Nix-provided tools with cuenv's typed environment variables
- Ensure reproducible builds across different machines
- Share consistent development environments with your team

## Prerequisites

### Install Nix

If you don't have Nix installed:

```bash
# Official installer (multi-user)
sh <(curl -L https://nixos.org/nix/install) --daemon

# Or use the Determinate Systems installer (recommended)
curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh
```

### Enable Flakes

Add to `~/.config/nix/nix.conf` or `/etc/nix/nix.conf`:

```ini
experimental-features = nix-command flakes
```

## Basic Setup

### Project Structure

A typical project with Nix integration:

```
my-project/
├── env.cue          # cuenv configuration
├── flake.nix        # Nix flake definition
├── flake.lock       # Locked dependencies
├── cuenv.lock       # cuenv runtime/sync lock state
└── src/
```

### Example flake.nix

```nix
{
  description = "My project development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
      in {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            nodejs_20
            pnpm
            postgresql
            redis
          ];

          shellHook = ''
            echo "Development environment loaded"
          '';
        };
      }
    );
}
```

### Example env.cue with Nix Hook

```cue
package cuenv

import (
    "github.com/cuenv/cuenv/schema"
    xNix "github.com/cuenv/cuenv/contrib/nix"
)

schema.#Project & {
  name: "my-project"
  runtime: schema.#NixRuntime
}

env: {
    NODE_ENV: "development"
    DATABASE_URL: "postgresql://localhost/myapp_dev"
}

hooks: {
    onEnter: {
        // Load Nix flake environment
        nix: xNix.#NixFlake
    }
}

tasks: {
    dev: schema.#Task & {
        command: "pnpm"
        args: ["run", "dev"]
    }
}
```

## The #NixFlake Hook

cuenv provides a contrib `#NixFlake` hook type that loads your Nix development environment:

```cue
import xNix "github.com/cuenv/cuenv/contrib/nix"

hooks: {
    onEnter: {
        nix: xNix.#NixFlake
    }
}
```

### How It Works

The `#NixFlake` hook:

1. Detects `flake.nix` and `flake.lock` in your project
2. Runs `nix print-dev-env` to get the environment
3. Sources the environment variables into your shell
4. Tracks `flake.nix` and `flake.lock` as inputs for cache invalidation

When the project also declares `runtime: schema.#NixRuntime`, `cuenv sync`
records the Nix runtime state in `cuenv.lock` using a digest derived from the
checked-in `flake.lock`.

### Hook Definition

```cue
#NixFlake: #ExecHook & {
    order:     10          // Run early in hook sequence
    propagate: false       // Do not auto-export to child processes
    command:   "nix"
    args:      ["print-dev-env"]
    source:    true        // Source output as shell script
    inputs:    ["flake.nix", "flake.lock"]
}
```

## Nix and cuenv Lockfiles

Commit both lockfiles:

- `flake.lock` pins Nix flake inputs.
- `cuenv.lock` records cuenv-managed runtime state and sync-managed external
  inputs.

For a Nix runtime project, refresh cuenv's lock state after changing
`flake.lock`:

```bash
cuenv sync lock
```

Validate drift in CI:

```bash
cuenv sync --check
```

See [Lockfiles](/how-to/lockfiles/) for the full `flake.lock` versus
`cuenv.lock` boundary.

## Configuration Patterns

### Nix + Environment Variables

Combine Nix-provided tools with cuenv's typed environment:

```cue
package cuenv

import (
    "github.com/cuenv/cuenv/schema"
    xNix "github.com/cuenv/cuenv/contrib/nix"
)

schema.#Project & {
  name: "my-project"
  runtime: schema.#NixRuntime
}

// Environment variables (typed by CUE)
env: {
    // App configuration
    NODE_ENV: "development" | "production"
    PORT:     3000

    // Database (Nix provides the postgres binary)
    PGHOST:     "localhost"
    PGPORT:     "5432"
    PGDATABASE: "myapp_dev"
}

hooks: {
    onEnter: {
        // Nix provides: bun, psql, redis-cli, etc.
        nix: xNix.#NixFlake
    }
}

tasks: {
    // These commands come from Nix
    dev:   schema.#Task & {command: "pnpm", args: ["run", "dev"]}
    build: schema.#Task & {command: "pnpm", args: ["run", "build"]}

    db: schema.#TaskGroup & {
        type: "group"
        migrate: schema.#Task & {command: "psql", args: ["-f", "migrations/up.sql"]}
        reset:   schema.#Task & {command: "psql", args: ["-f", "migrations/reset.sql"]}
    }
}
```

### Multiple Environments

Use different Nix outputs for different scenarios:

```nix
# flake.nix
{
  outputs = { self, nixpkgs, ... }:
    # ...
    {
      devShells = {
        default = pkgs.mkShell {
          buildInputs = with pkgs; [ bun ];
        };

        ci = pkgs.mkShell {
          buildInputs = with pkgs; [ bun chromium ];
        };

        production = pkgs.mkShell {
          buildInputs = with pkgs; [ bun ];
        };
      };
    };
}
```

## Using cuenv with direnv

cuenv works alongside direnv. If you're already using direnv with Nix:

**.envrc:**

```bash
use flake
```

You can still use cuenv for additional typed configuration:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

// Don't load Nix via cuenv if direnv handles it
env: {
    APP_NAME: "my-app"
    LOG_LEVEL: "debug"
}

tasks: {
    dev: schema.#Task & {command: "bun", args: ["run", "dev"]}
}
```

## Troubleshooting

### Nix Not Found

```
error: hook failed: command 'nix' not found
```

**Fix:** Ensure Nix is installed and in your PATH:

```bash
# Check Nix installation
nix --version

# If using nix-daemon, ensure it's running
sudo systemctl status nix-daemon
```

### Flake Not Found

```
error: hook failed: 'flake.nix' not found in current directory
```

**Fix:** Ensure you're in a directory with a `flake.nix` file, or create one:

```bash
nix flake init
```

### Slow Environment Loading

Nix evaluation can be slow on first run. Tips:

1. **Use binary caches** in `flake.nix`:

   ```nix
   nixConfig = {
     extra-substituters = ["https://cache.nixos.org"];
   };
   ```

2. **Enable the nix-daemon** for better caching

3. **Use a project binary cache** for custom builds. In GitHub Actions, cuenv can generate setup for Namespace cache volumes or Cachix:

   ```cue
   ci: {
     contributors: [contributors.#NamespaceCache]
     provider: github: namespaceCache: {}
   }
   ```

   Cachix remains supported for projects that use it:

   ```bash
   cachix use my-cache
   ```

### Lockfile Issues

```
error: lock file needs to be updated
```

If Nix reports the lock issue, update `flake.lock`:

```bash
nix flake update
```

If cuenv reports that `cuenv.lock` is stale, refresh cuenv's lock state:

```bash
cuenv sync lock
```

Commit both lockfiles after the update.

## Best Practices

### 1. Pin Your Dependencies

Always commit both `flake.lock` and `cuenv.lock` for reproducibility:

```bash
git add flake.lock cuenv.lock
git commit -m "chore: update lockfiles"
```

### 2. Separate Concerns

- Use Nix for **tooling** (compilers, formatters, databases)
- Use cuenv for **configuration** (environment variables, secrets, tasks)

### 3. Document Required Tools

List Nix-provided tools in your README:

```markdown
## Development Setup

This project uses Nix for tooling. Enter the development shell:

\`\`\`bash
nix develop

# Or with cuenv (loads automatically)

cd project-dir
\`\`\`

**Provided tools:**

- Bun
- PostgreSQL 16
- Redis 7
```

### 4. CI/CD Integration

Use Nix in CI for reproducible builds:

```yaml
# .github/workflows/ci.yml
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: namespacelabs/nscloud-cache-action@v1
        if: runner.os == 'Linux'
        with:
          cache: nix
      - run: nix develop --command cuenv task ci
```

## See Also

- [Hooks Documentation](/how-to/configure-a-project/#hooks) - Hook configuration
- [Lockfiles](/how-to/lockfiles/) - `flake.lock` and `cuenv.lock` guidance
- [Shell Integration](/how-to/install/#shell-integration) - Shell setup
- [Nix Flakes Manual](https://nixos.wiki/wiki/Flakes) - Official Nix flakes documentation
- [direnv](https://direnv.net/) - Alternative environment loading
