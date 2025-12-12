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
  dev: { command: "cargo", args: ["run"] }
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
  dev: { command: "bun", args: ["run", "dev"] }
}
```

## 4) Run tasks per service

```bash
cuenv task dev -p services/api
cuenv task dev -p services/web
```

## Next steps

- [Configure a project](/how-to/configure-a-project/)
- [Run tasks](/how-to/run-tasks/)
