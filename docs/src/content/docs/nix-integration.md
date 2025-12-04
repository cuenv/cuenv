---
title: Nix Integration
description: Integration with Nix package manager and development environments
---

cuenv integrates with [Nix](https://nixos.org/) to provide reproducible development environments. When you have a `flake.nix` in your project, cuenv can automatically load the Nix development environment alongside your CUE configuration.

:::note
Nix integration is currently in active development. Some features described here may not be fully implemented yet.
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

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

env: {
    NODE_ENV: "development"
    DATABASE_URL: "postgresql://localhost/myapp_dev"
}

hooks: {
    onEnter: {
        // Load Nix flake environment
        nix: schema.#NixFlake
    }
}

tasks: {
    dev: {
        command: "pnpm"
        args: ["run", "dev"]
    }
}
```

## The #NixFlake Hook

cuenv provides a built-in `#NixFlake` hook type that loads your Nix development environment:

```cue
import "github.com/cuenv/cuenv/schema"

hooks: {
    onEnter: {
        nix: schema.#NixFlake
    }
}
```

### How It Works

The `#NixFlake` hook:

1. Detects `flake.nix` and `flake.lock` in your project
2. Runs `nix print-dev-env` to get the environment
3. Sources the environment variables into your shell
4. Tracks `flake.nix` and `flake.lock` as inputs for cache invalidation

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

## Configuration Patterns

### Nix + Environment Variables

Combine Nix-provided tools with cuenv's typed environment:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

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
        nix: schema.#NixFlake
    }
}

tasks: {
    // These commands come from Nix
    dev:    {command: "pnpm", args: ["run", "dev"]}
    build:  {command: "pnpm", args: ["run", "build"]}

    db: {
        migrate: {command: "psql", args: ["-f", "migrations/up.sql"]}
        reset:   {command: "psql", args: ["-f", "migrations/reset.sql"]}
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

## direnv Compatibility

cuenv works alongside direnv. If you're already using direnv with Nix:

**.envrc:**

```bash
use flake
```

You can still use cuenv for additional typed configuration:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

// Don't load Nix via cuenv if direnv handles it
env: {
    APP_NAME: "my-app"
    LOG_LEVEL: "debug"
}

tasks: {
    dev: {command: "bun", args: ["run", "dev"]}
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

3. **Use cachix** for custom binary caches:
   ```bash
   cachix use my-cache
   ```

### Lock File Issues

```
error: lock file needs to be updated
```

**Fix:** Update your lock file:

```bash
nix flake update
```

## Best Practices

### 1. Pin Your Dependencies

Always commit `flake.lock` for reproducibility:

```bash
git add flake.lock
git commit -m "chore: update nix flake lock"
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
      - uses: cachix/install-nix-action@v25
        with:
          extra_nix_config: |
            experimental-features = nix-command flakes
      - run: nix develop --command cuenv task ci
```

## See Also

- [Hooks Documentation](/configuration/#hooks) - Hook configuration
- [Shell Integration](/installation/#shell-integration) - Shell setup
- [Nix Flakes Manual](https://nixos.wiki/wiki/Flakes) - Official Nix flakes documentation
- [direnv](https://direnv.net/) - Alternative environment loading
