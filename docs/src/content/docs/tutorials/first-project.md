---
title: Your first cuenv project
description: Create a project, define env + tasks in CUE, and run them with cuenv.
---

You’ll create a tiny project with an `env.cue`, then run commands and tasks through cuenv so configuration is validated before anything executes.

## 1) Install cuenv

Follow [Install cuenv](/how-to/install/) for your platform, then verify:

```bash
cuenv version
```

## 2) Create a project directory

```bash
mkdir my-cuenv-project
cd my-cuenv-project
```

## 3) Create `env.cue`

Create a minimal configuration with a typed environment and a couple of tasks:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
  NODE_ENV: "development" | "production"
  PORT:     "3000"
}

tasks: {
  hello: { command: "echo", args: ["Hello from cuenv"] }
  dev:   { command: "bun",  args: ["run", "dev"] }
}
```

If you don’t use Bun, replace the `dev` task with whatever you run locally (for example `npm`, `pnpm`, `cargo`, `python`, etc.).

## 4) Inspect the environment

```bash
cuenv env print
```

## 5) Run a command with the environment

```bash
cuenv exec -- env
```

## 6) Run tasks

```bash
# List tasks
cuenv task

# Run a task
cuenv task hello
```

## Next steps

- [Configure a project](/how-to/configure-a-project/)
- [Typed environments](/how-to/typed-environments/)
- [Run tasks](/how-to/run-tasks/)
- [Secrets](/how-to/how-to/secrets/)
- [CLI reference](/reference/cli/)
