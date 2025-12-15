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
        defaultOwners: ["@myorg/maintainers"]
        rules: [
            {pattern: "*.rs", owners: ["@rust-team"]},
            {pattern: "/docs/**", owners: ["@docs-team"]},
        ]
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
    rules: [
        {pattern: "*", owners: ["@owner"]},
    ]
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
    rules: [
        {pattern: "*.ts", owners: ["@frontend-team", "@alice"]},
        {pattern: "/security/**", owners: ["security@company.com"]},
    ]
}
```

### Default Owners

Set fallback owners for files that don't match any pattern:

```cue
owners: {
    defaultOwners: ["@myorg/core-team"]
    rules: [
        {pattern: "*.ts", owners: ["@frontend-team"]},
    ]
}
```

This generates a `* @myorg/core-team` rule at the top of the file.

### Sections

Organize rules with section headers for better readability:

```cue
owners: {
    rules: [
        {pattern: "*.rs", owners: ["@backend"], section: "Backend"},
        {pattern: "*.go", owners: ["@backend"], section: "Backend"},
        {pattern: "*.ts", owners: ["@frontend"], section: "Frontend"},
        {pattern: "*.tsx", owners: ["@frontend"], section: "Frontend"},
        {pattern: "/docs/**", owners: ["@docs-team"], section: "Documentation"},
    ]
}
```

Rules with the same section are grouped together in the output.

### Descriptions

Add comments above rules to explain their purpose:

```cue
owners: {
    rules: [
        {
            pattern:     "/security/**"
            owners:      ["@security-team"]
            description: "Security-sensitive code requires security team review"
        },
    ]
}
```

### Custom Headers

Add a custom header comment at the top of the generated file:

```cue
owners: {
    output: {
        header: "Code ownership rules for my-project"
    }
    rules: [
        {pattern: "*", owners: ["@owner"]},
    ]
}
```

### Custom Output Path

Override the default output path:

```cue
owners: {
    output: {
        path: "CODEOWNERS"  // Write to root instead of .github/
    }
    rules: [
        {pattern: "*", owners: ["@owner"]},
    ]
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
    defaultOwners: ["@myorg/maintainers"]
    rules: [
        // Backend
        {pattern: "*.rs", owners: ["@rust-team"], section: "Backend"},
        {pattern: "*.go", owners: ["@go-team"], section: "Backend"},

        // Frontend
        {pattern: "*.ts", owners: ["@frontend"], section: "Frontend"},
        {pattern: "*.tsx", owners: ["@frontend"], section: "Frontend"},
        {pattern: "*.css", owners: ["@frontend"], section: "Frontend"},

        // Infrastructure
        {pattern: "*.tf", owners: ["@platform-team"], section: "Infrastructure"},
        {pattern: "Dockerfile", owners: ["@platform-team"], section: "Infrastructure"},
        {pattern: ".github/**", owners: ["@platform-team"], section: "Infrastructure"},

        // Documentation
        {pattern: "/docs/**", owners: ["@docs-team"], section: "Documentation"},
        {pattern: "*.md", owners: ["@docs-team"], section: "Documentation"},
    ]
}
```

### GitLab Repository

```cue
owners: {
    output: {
        platform: "gitlab"
    }
    rules: [
        {pattern: "*.py", owners: ["@backend-group"], section: "Backend"},
        {pattern: "/api/**", owners: ["@api-team"], section: "API"},
        {pattern: "/frontend/**", owners: ["@frontend-group"], section: "Frontend"},
    ]
}
```

### Monorepo with Multiple Teams

```cue
owners: {
    defaultOwners: ["@platform-team"]
    rules: [
        // Service ownership
        {pattern: "/services/auth/**", owners: ["@auth-team"], section: "Services"},
        {pattern: "/services/billing/**", owners: ["@billing-team"], section: "Services"},
        {pattern: "/services/notifications/**", owners: ["@notifications-team"], section: "Services"},

        // Shared libraries
        {pattern: "/packages/ui/**", owners: ["@design-system"], section: "Packages"},
        {pattern: "/packages/utils/**", owners: ["@platform-team"], section: "Packages"},

        // Critical paths
        {
            pattern:     "/services/*/security/**"
            owners:      ["@security-team"]
            description: "Security code requires security team approval"
            section:     "Security"
        },
    ]
}
```

## Generated File Format

Generated CODEOWNERS files include a header comment indicating they're managed by cuenv:

```
# CODEOWNERS file - Generated by cuenv
# Do not edit manually. Configure in env.cue and run `cuenv sync codeowners`

# Default owners for all files
* @myorg/maintainers

# Backend
*.rs @rust-team
*.go @go-team

# Frontend
*.ts @frontend
*.tsx @frontend
```

This helps prevent accidental manual edits that would be overwritten on the next sync.
