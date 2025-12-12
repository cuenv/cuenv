---
title: Migration Guide
description: Upgrade notes and breaking changes between cuenv versions
---

This page documents **breaking changes** and how to update your configuration when upgrading cuenv.

## Split schema: `#Base` and `#Project`

The monolithic `schema.#Cuenv` type has been replaced with two composable types:

- **`schema.#Base`**: composable configuration (config/env/workspaces)
- **`schema.#Project`**: project (leaf) configuration (extends `#Base` with project-only fields)

### Before

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

env: {...}
tasks: {...}
```

### After

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
  env: {...}
  tasks: {...}
}
```

### Optional: use `#Base` for shared config

If you want shared configuration in a parent directory, you can keep a separate CUE value constrained by `#Base`:

```cue
import "github.com/cuenv/cuenv/schema"

schema.#Base & {
  env: {
    SHARED: "1"
  }
}
```

## `name` is required for projects

`schema.#Project` requires a `name` field:

```cue
schema.#Project & {
  name: "my-project"
}
```

This name is used as the stable identifier for cross-project references (see `schema.#TaskRef`).

## CI field rename: `_ci` → `ci`

If your configuration used `_ci`, rename it to `ci`.

## Import/path notes

The schema entrypoint is still imported from:

`github.com/cuenv/cuenv/schema`

Only the **root type name** changed (`#Cuenv` → `#Project`), and a new composable type (`#Base`) was introduced.
