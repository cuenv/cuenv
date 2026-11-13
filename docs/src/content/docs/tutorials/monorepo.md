---
title: Use cuenv in a monorepo
description: Compose configuration across directories and run tasks per service.
---

This tutorial shows a simple pattern for a repository with multiple services, each with its own `env.cue`, while sharing common configuration.

## 1) Create a layout

```text
my-monorepo/
├── cue.mod/
│   └── module.cue
├── shared/
│   └── env.cue
└── services/
    ├── api/
    │   └── env.cue
    └── web/
        └── env.cue
```

## 2) Define shared config

`shared/env.cue`:

```cue
package shared

#SharedEnv: {
  LOG_LEVEL: "debug" | "info" | "warn" | "error" | *"info"
}
```

## 3) Define per-service config

`services/api/env.cue`:

```cue
package cuenv

import (
  "github.com/cuenv/cuenv/schema"
  "my-monorepo/shared"
)

schema.#Project & {
  name: "api"
}

env: shared.#SharedEnv & {
  SERVICE_NAME: "api"
  PORT:         "8080"
}

tasks: {
  dev: schema.#Task & { command: "cargo", args: ["run"] }
}
```

`services/web/env.cue`:

```cue
package cuenv

import (
  "github.com/cuenv/cuenv/schema"
  "my-monorepo/shared"
)

schema.#Project & {
  name: "web"
}

env: shared.#SharedEnv & {
  SERVICE_NAME: "web"
  PORT:         "3000"
}

tasks: {
  dev: schema.#Task & { command: "bun", args: ["run", "dev"] }
}
```

## 4) Run tasks per service

```bash
cuenv task dev -p services/api
cuenv task dev -p services/web
```

## 5) Read files of another service, or of the repository

A task that reads files outside its own service names them through a file
reference instead of a `../` path, so caching and `cuenv ci` know which changes
matter. Import the other service by its directory and the package `cuenv`, and
say which of its files the task reads; name files at the module root, such as a
lockfile, from there:

```cue
// services/web/env.cue — any language version
import (
  "github.com/cuenv/cuenv/schema"
  api "my-monorepo/services/api:cuenv"
)

tasks: {
  build: schema.#Task & {
    command: "bun"
    args: ["run", "build"]
    inputs: [
      "src/**",
      api.#Files & {paths: ["openapi.yaml"]},
      schema.#ModuleFiles & {paths: ["bun.lock"]},
    ]
  }
}
```

`api` declares nothing about the files `web` reads, and no task of `api` runs.
Every path must match at least one file, so renaming `openapi.yaml` in `api` is
an error for `web` rather than a silently smaller input set. See
[Files of another project, or of the module](/how-to/run-tasks/#files-of-another-project-or-of-the-module).

## Next steps

- [Configure a project](/how-to/configure-a-project/)
- [Run tasks](/how-to/run-tasks/)
