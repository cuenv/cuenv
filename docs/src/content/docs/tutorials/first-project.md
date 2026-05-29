---
title: Your first cuenv project
description: Create a project, define a typed environment plus tasks in CUE, and run them — with the exact output you should see at every step.
---

import { Steps, Aside } from '@astrojs/starlight/components';

By the end of this page you will have replaced a `.env` file and a couple of
`Makefile` targets with a single, typed `env.cue` — and watched cuenv print
`Hello from cuenv` for you. Everything below is copy-pasteable, and every step
shows the output you should see, so you can tell at a glance that it worked.

This tutorial is derived from the runnable
[`examples/env-basic`](https://github.com/cuenv/cuenv/tree/main/examples/env-basic)
and
[`examples/task-basic`](https://github.com/cuenv/cuenv/tree/main/examples/task-basic)
projects.

## Before you start

You need two command-line tools:

- **`cuenv`** — follow [Install cuenv](/how-to/install/), then check it runs:

  ```bash
  cuenv version
  ```

  ```text
  cuenv 0.50.0 - Event-driven CLI with inline TUI for cuenv
  ```

- **`cue`** — the CUE CLI, used once to set up the project module. Install it
  from the [CUE releases](https://github.com/cue-lang/cue/releases) or via your
  package manager (`brew install cue`, `nix profile install nixpkgs#cue`, etc.).
  cuenv configuration *is* CUE, so this is the same toolchain your editor uses
  (see [Editor Setup](/how-to/editor-setup/)).

## Build the project

<Steps>

1. **Create a project directory.**

   ```bash
   mkdir my-cuenv-project
   cd my-cuenv-project
   ```

2. **Initialise a CUE module.**

   cuenv projects are standard CUE modules. Every cuenv command looks for a
   `cue.mod/` directory and refuses to run without one, so this step comes
   *first* — before you write any config.

   ```bash
   cue mod init github.com/me/my-cuenv-project
   ```

   Use any module path you like; `github.com/<you>/<project>` is the convention.
   This creates `cue.mod/module.cue`.

   <Aside type="caution" title="Don't skip this">
   If you write `env.cue` first and run a cuenv command, you'll get
   `No CUE module found (looking for cue.mod/)`. Initialising the module is what
   makes the `import "github.com/cuenv/cuenv/schema"` line in the next step
   resolvable.
   </Aside>

3. **Add the cuenv schema as a dependency.**

   The schema that gives you `#Project`, `#Task`, and friends is published as a
   CUE module. Fetch it once into your module:

   ```bash
   cue mod get github.com/cuenv/cuenv@latest
   ```

   This records the dependency in `cue.mod/module.cue`:

   ```cue
   deps: {
       "github.com/cuenv/cuenv@v0": {
           v: "v0.50.0"
       }
   }
   ```

4. **Write `env.cue`.**

   Create `env.cue` with a typed environment and two tasks:

   ```cue
   package cuenv

   import "github.com/cuenv/cuenv/schema"

   schema.#Project & {
       name: "my-project"
   }

   env: {
       // An enum with a default: only these values are valid, and the
       // project falls back to "development" if you don't set NODE_ENV.
       NODE_ENV: "development" | "production" | *"development"

       // Environment values exported to your shell are strings.
       PORT: "3000"
   }

   tasks: {
       hello: schema.#Task & {
           command: "echo"
           args: ["Hello from cuenv"]
       }

       // CUE interpolation: this reuses the value of NODE_ENV above.
       greet: schema.#Task & {
           command: "echo"
           args: ["Hello, \(env.NODE_ENV)!"]
       }
   }
   ```

   <Aside type="note" title="Why is PORT a string?">
   Environment variables that cuenv exports to your shell are always strings, so
   this site keeps them quoted as `"3000"`. CUE also accepts `int` and `bool`
   (you may see `PORT: 3000` or `DEBUG: true` in other examples) — cuenv
   stringifies those on export. Strings are the safest default while you learn.
   See [Typed environments](/how-to/typed-environments/) for the full set of
   constraints.
   </Aside>

</Steps>

## See it work

You now have a complete project. Run these four commands to get your first win.

### Print the resolved environment

```bash
cuenv env print
```

```text
NODE_ENV=development
PORT=3000
```

`NODE_ENV` resolved to its default, and `PORT` is the value you set. Want JSON
instead? Add `--output json`:

```bash
cuenv env print --output json
```

```json
{
  "NODE_ENV": "development",
  "PORT": "3000"
}
```

<Aside type="tip" title="What just happened">
You described your environment **once**, in a typed file, and cuenv validated it
and resolved it for you. Try changing `NODE_ENV` to `"staging"` and re-running
`cuenv env print` — evaluation fails before anything runs, because `"staging"`
isn't one of the allowed values. That's the whole pitch: invalid config can't
reach your commands.
</Aside>

### List your tasks

```bash
cuenv task
```

```text
Tasks:
├─ greet
└─ hello

(2 tasks, 0 groups, 0 cached)
```

### Run a task

```bash
cuenv task hello
```

```text
> [hello] echo Hello from cuenv
Hello from cuenv
Task 'hello' succeeded
Output:
Hello from cuenv
```

There it is — `Hello from cuenv`, run through your typed project. The `greet`
task shows interpolation in action:

```bash
cuenv task greet
```

```text
> [greet] echo Hello, development!
Hello, development!
Task 'greet' succeeded
```

### Run any command in the environment

`cuenv exec` runs an arbitrary command with your resolved environment applied —
the same way you'd use `cuenv exec -- npm start` or `cuenv exec -- cargo run` in
a real project:

```bash
cuenv exec -- printenv PORT
```

```text
3000
```

## Next steps

You've replaced a `.env` file and a Makefile target with one validated config.
From here:

- [Configure a project](/how-to/configure-a-project/) — splitting config across
  files, schemas, and composition.
- [Typed environments](/how-to/typed-environments/) — enums, numeric bounds,
  regex, and per-environment overrides instead of `.env`.
- [Run tasks](/how-to/run-tasks/) — parallel groups, ordered sequences,
  `dependsOn`, and content-addressed caching.
- [Secrets](/how-to/secrets/) — resolve credentials at runtime from 1Password,
  AWS, and more, with redaction built in.
- [CLI reference](/reference/cli/) — every command and flag.

If something didn't print what you expected, see
[Troubleshooting](/how-to/troubleshooting/), and check the
[schema status](/reference/schema/status/) page before relying on any feature.
