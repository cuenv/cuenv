---
title: Editor Setup
description: Get CUE syntax highlighting and validation in VS Code, Neovim, and JetBrains IDEs for editing cuenv configs.
---

cuenv configuration is just [CUE](https://cuelang.org/). Everything you author — `env.cue`, tasks, secrets, CI pipelines — is a CUE file. So the single biggest editor upgrade is **CUE language support**: syntax highlighting, formatting, and on-the-fly validation against the cuenv schema.

The good news: CUE language support is genuinely available today across the major editors, and it is what you actually want when writing `env.cue`. Set it up once and a typo in a field name or a wrong type is flagged before you ever run `cuenv`.

:::note
There is an in-repo VS Code extension specific to cuenv (Task Explorer, environment switcher, dependency graph). It is **in development and not yet published** to any marketplace. This page covers what is shipping today; see [Where things stand](#cuenv-specific-vs-code-extension-in-development) below for the honest status.
:::

## Visual Studio Code

Install a CUE language extension from the Marketplace. Both of these give you highlighting, formatting, and validation:

- **[CUE (Official)](https://marketplace.visualstudio.com/items?itemName=cue-lang.vscode-cue)** — published by the CUE language team (`cue-lang.vscode-cue`).
- **[CUE (Community)](https://marketplace.visualstudio.com/items?itemName=asvetliakov.vscode-cue)** — a community alternative (`asvetliakov.vscode-cue`).

### Format on save

Add a `.vscode/settings.json` to your project (or use your global settings) so CUE files are formatted automatically:

```json
{
  "[cue]": {
    "editor.defaultFormatter": "cue-lang.vscode-cue",
    "editor.formatOnSave": true
  }
}
```

If you installed the community extension instead, set the formatter ID to `asvetliakov.vscode-cue`.

### Schema validation

cuenv projects are standard CUE modules. With a `cue.mod/module.cue` in your project root, the CUE tooling can resolve and validate the cuenv schema as long as you import it the same way the examples do:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "my-project"
	env: {
		GREETING: "hello from cuenv"
	}
}
```

Misspell a field or assign the wrong type and the language server underlines it in place — you catch it before running [`cuenv env print`](/reference/cli/).

### cuenv-specific VS Code extension (in development)

You may have seen references to a dedicated "Cuenv" VS Code extension with a Task Explorer, environment switcher, Variables view, and a dependency-graph webview. **That extension is not published.** It lives in the repository at [`integrations/vscode`](https://github.com/cuenv/cuenv/tree/main/integrations/vscode) at version `0.0.0-dev`, it is **not on the VS Code Marketplace**, and there is no `.vsix` release to install. Treat its features as planned, not shipped — do not rely on them.

If you want to experiment with the work-in-progress, you can build and install it locally from a clone of the repo:

```bash
git clone https://github.com/cuenv/cuenv.git
cd cuenv/integrations/vscode

# Install dev dependencies, bundle, and package a local .vsix
bun install
bun run bundle
bun run package

# Install the locally built extension
code --install-extension cuenv-vscode-0.0.0-dev.vsix
```

This is for adventurous users only. The extension is unfinished, its behavior may change without notice, and nothing here is guaranteed to work. The reliable, supported editor experience today is plain CUE language support plus the [`cuenv` CLI](/reference/cli/) in your terminal.

## Neovim / Vim

The CUE language server lives in the [CUE project itself](https://cuelang.org/). With [nvim-lspconfig](https://github.com/neovim/nvim-lspconfig), point at the `cue` LSP rather than any unrelated server.

1. Install the `cue` binary (it ships the language server).
2. Configure `lspconfig` for CUE:

```lua
-- Recent nvim-lspconfig ships a `cue` server config that runs `cue lsp`.
require('lspconfig').cue.setup({})
```

:::caution
The CUE language server is still evolving, and the exact server name and
launch command can change between `cue` and `nvim-lspconfig` releases. If
`require('lspconfig').cue.setup({})` does not work for your versions, check
`:help lspconfig-all` for the current CUE entry and follow the
[CUE documentation](https://cuelang.org/docs/) for the latest setup. Do **not**
use unrelated LSP server configs (for example a tool's own embedded `cuelsp`) —
those configure a different language server, not CUE.
:::

For pure syntax highlighting without an LSP, the built-in `cue` filetype detection in modern Neovim plus a Treesitter CUE grammar (`:TSInstall cue`) is enough to get readable, colorized `env.cue` files.

## IntelliJ / GoLand

JetBrains IDEs have a real, published CUE plugin:

1. Install the **[CUE plugin](https://plugins.jetbrains.com/plugin/10480-cue)** from the JetBrains Marketplace.
2. Restart the IDE.
3. Open **Settings > Languages & Frameworks > CUE** to point at your `cue` executable if it is not auto-detected.

This gives you syntax highlighting and CUE awareness for `env.cue` and the rest of your configuration.

## Verify it works

Whatever editor you chose, the proof is the same: open an `env.cue`, introduce a deliberate type error, and confirm the editor flags it. Then fix it and confirm the warning clears. For a config to validate against, start from a real example such as [`examples/env-basic`](https://github.com/cuenv/cuenv/tree/main/examples/env-basic):

```bash
cuenv env print --path examples/env-basic --package examples
```

If the CLI agrees with your editor, your schema resolution is wired correctly.

## Next steps

- Install the CLI itself: [Install cuenv](/how-to/install/).
- Build your first config in the [Quick Start tutorial](/tutorials/first-project/).
- Define [typed environments](/how-to/typed-environments/) instead of `.env` files.
- Replace your `Makefile` with [task orchestration](/how-to/run-tasks/).
- Check the [schema status](/reference/schema/status/) page before relying on any feature — including editor tooling.
