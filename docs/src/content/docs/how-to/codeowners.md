---
title: CODEOWNERS
description: Managing CODEOWNERS files for GitHub, GitLab, and Bitbucket with cuenv
---

cuenv can generate and manage CODEOWNERS files from your CUE configuration. This keeps your code ownership rules in a single source of truth alongside your other project configuration.

## Quick Start

Add an `owners` field to your `env.cue`:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    owners: {
        rules: {
            "default": {pattern: "*", owners: ["@myorg/maintainers"], order: 0}
            "rust": {pattern: "*.rs", owners: ["@rust-team"], order: 1}
            "docs": {pattern: "/docs/**", owners: ["@docs-team"], order: 2}
        }
    }
}
```

Then run:

```bash
cuenv sync
```

This generates a CODEOWNERS file in the appropriate location for your platform (e.g., `.github/CODEOWNERS` for GitHub).

## Commands

### Generate CODEOWNERS file

```bash
cuenv sync
```

Runs all sync operations, including CODEOWNERS file generation.

### Generate only CODEOWNERS file

```bash
cuenv sync codeowners
```

Runs only the CODEOWNERS file generation.

### Preview changes (dry-run)

```bash
cuenv sync --dry-run
```

Shows what files would be created or updated without actually writing them.

### Check if file is in sync

```bash
cuenv sync --check
```

Verifies the CODEOWNERS file matches the configuration. Useful in CI pipelines.

## Configuration

### Platform Selection

cuenv auto-detects your platform based on repository structure, but you can explicitly set it:

```cue
owners: {
    output: {
        platform: "github"  // or "gitlab" or "bitbucket"
    }
    rules: {
        "default": {pattern: "*", owners: ["@owner"]}
    }
}
```

| Platform    | Default Path         | Section Syntax |
| ----------- | -------------------- | -------------- |
| `github`    | `.github/CODEOWNERS` | `# Section`    |
| `gitlab`    | `CODEOWNERS`         | `[Section]`    |
| `bitbucket` | `CODEOWNERS`         | `# Section`    |

### Owner Formats

Owners can be specified in several formats:

- `@username` - GitHub/GitLab user
- `@org/team-name` - GitHub team or GitLab group
- `email@example.com` - Email address

```cue
owners: {
    rules: {
        "typescript": {pattern: "*.ts", owners: ["@frontend-team", "@alice"]}
        "security": {pattern: "/security/**", owners: ["security@company.com"]}
    }
}
```

### Catch-all Rules

Set fallback owners for files that don't match any specific pattern using the `*` pattern:

```cue
owners: {
    rules: {
        "default": {pattern: "*", owners: ["@myorg/core-team"], order: 0}
        "typescript": {pattern: "*.ts", owners: ["@frontend-team"], order: 1}
    }
}
```

This generates a `* @myorg/core-team` rule at the top of the file. Use the `order` field to ensure it appears first.

### Sections

Organize rules with section headers for better readability:

```cue
owners: {
    rules: {
        "rust": {pattern: "*.rs", owners: ["@backend"], section: "Backend", order: 1}
        "go": {pattern: "*.go", owners: ["@backend"], section: "Backend", order: 2}
        "ts": {pattern: "*.ts", owners: ["@frontend"], section: "Frontend", order: 3}
        "tsx": {pattern: "*.tsx", owners: ["@frontend"], section: "Frontend", order: 4}
        "docs": {pattern: "/docs/**", owners: ["@docs-team"], section: "Documentation", order: 5}
    }
}
```

Rules with the same section are grouped together in the output.

### Descriptions

Add comments above rules to explain their purpose:

```cue
owners: {
    rules: {
        "security": {
            pattern:     "/security/**"
            owners:      ["@security-team"]
            description: "Security-sensitive code requires security team review"
        }
    }
}
```

### Custom Headers

Add a custom header comment at the top of the generated file:

```cue
owners: {
    output: {
        header: "Code ownership rules for my-project"
    }
    rules: {
        "default": {pattern: "*", owners: ["@owner"]}
    }
}
```

### Custom Output Path

Override the default output path:

```cue
owners: {
    output: {
        path: "CODEOWNERS"  // Write to root instead of .github/
    }
    rules: {
        "default": {pattern: "*", owners: ["@owner"]}
    }
}
```

## Output Status

When running `cuenv sync`, you'll see the status of the CODEOWNERS file:

- **Created** - New file was created
- **Updated** - Existing file was updated with new content
- **Unchanged** - File exists and content matches (no write needed)

In dry-run mode:

- **Would create** - File would be created
- **Would update** - File would be updated

## Security

cuenv sync has built-in security protections:

1. **Path traversal protection** - Custom paths cannot reference parent directories (`..`)
2. **Write containment** - Files are only written within the repository boundaries
3. **Owner validation** - Owner formats are validated against the expected patterns

## Examples

### GitHub Repository

```cue
owners: {
    output: {
        platform: "github"
        header:   "Auto-generated CODEOWNERS - configure in env.cue"
    }
    rules: {
        "default": {pattern: "*", owners: ["@myorg/maintainers"], order: 0}

        // Backend
        "rust": {pattern: "*.rs", owners: ["@rust-team"], section: "Backend", order: 1}
        "go": {pattern: "*.go", owners: ["@go-team"], section: "Backend", order: 2}

        // Frontend
        "ts": {pattern: "*.ts", owners: ["@frontend"], section: "Frontend", order: 3}
        "tsx": {pattern: "*.tsx", owners: ["@frontend"], section: "Frontend", order: 4}
        "css": {pattern: "*.css", owners: ["@frontend"], section: "Frontend", order: 5}

        // Infrastructure
        "terraform": {pattern: "*.tf", owners: ["@platform-team"], section: "Infrastructure", order: 6}
        "docker": {pattern: "Dockerfile", owners: ["@platform-team"], section: "Infrastructure", order: 7}
        "github": {pattern: ".github/**", owners: ["@platform-team"], section: "Infrastructure", order: 8}

        // Documentation
        "docs": {pattern: "/docs/**", owners: ["@docs-team"], section: "Documentation", order: 9}
        "markdown": {pattern: "*.md", owners: ["@docs-team"], section: "Documentation", order: 10}
    }
}
```

### GitLab Repository

```cue
owners: {
    output: {
        platform: "gitlab"
    }
    rules: {
        "python": {pattern: "*.py", owners: ["@backend-group"], section: "Backend"}
        "api": {pattern: "/api/**", owners: ["@api-team"], section: "API"}
        "frontend": {pattern: "/frontend/**", owners: ["@frontend-group"], section: "Frontend"}
    }
}
```

### Monorepo with Multiple Teams

```cue
owners: {
    rules: {
        "default": {pattern: "*", owners: ["@platform-team"], order: 0}

        // Service ownership
        "auth": {pattern: "/services/auth/**", owners: ["@auth-team"], section: "Services", order: 1}
        "billing": {pattern: "/services/billing/**", owners: ["@billing-team"], section: "Services", order: 2}
        "notifications": {pattern: "/services/notifications/**", owners: ["@notifications-team"], section: "Services", order: 3}

        // Shared libraries
        "ui": {pattern: "/packages/ui/**", owners: ["@design-system"], section: "Packages", order: 4}
        "utils": {pattern: "/packages/utils/**", owners: ["@platform-team"], section: "Packages", order: 5}

        // Critical paths
        "security": {
            pattern:     "/services/*/security/**"
            owners:      ["@security-team"]
            description: "Security code requires security team approval"
            section:     "Security"
            order:       6
        }
    }
}
```

## Generated File Format

Generated CODEOWNERS files include a header comment indicating they're managed by cuenv:

```
# CODEOWNERS file - Generated by cuenv
# Do not edit manually. Configure in env.cue and run `cuenv sync codeowners`

* @myorg/maintainers

# Backend
*.rs @rust-team
*.go @go-team

# Frontend
*.ts @frontend
*.tsx @frontend
```

This helps prevent accidental manual edits that would be overwritten on the next sync.
