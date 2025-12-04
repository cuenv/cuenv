---
title: Editor Setup
description: Configure your editor for the best cuenv experience
---

cuenv uses [CUE](https://cuelang.org/) for all configuration. Setting up your editor with CUE support significantly improves the development experience with syntax highlighting, validation, and autocompletion.

## Visual Studio Code

We recommend using VS Code with the official Cuenv extension and CUE language support.

### Cuenv Extension

The official **[Cuenv Extension](https://marketplace.visualstudio.com/items?itemName=cuenv.cuenv-vscode)** provides:

- Task Explorer with inline run buttons
- Environment switcher
- Variables view with secret masking
- Task dependency graph visualization
- CodeLens integration in `env.cue` files

See the [VSCode Extension documentation](/vscode-extension/) for full details.

### CUE Language Support

For syntax highlighting, formatting, and validation of CUE files:

- **[CUE (Official)](https://marketplace.visualstudio.com/items?itemName=cue-lang.vscode-cue)** or **[CUE (Community)](https://marketplace.visualstudio.com/items?itemName=asvetliakov.vscode-cue)**

### Configuration

Create a `.vscode/settings.json` in your project (or add to your global settings) to enable formatting on save:

```json
{
  "[cue]": {
    "editor.defaultFormatter": "asvetliakov.vscode-cue",
    "editor.formatOnSave": true
  }
}
```

If you are using the official extension, replace the formatter ID with `cue-lang.vscode-cue`.

### Schema Validation

cuenv projects are standard CUE modules. If you run `cue mod init` in your project root, the CUE language server should automatically find and validate your schemas, provided you import them correctly:

```cue
import "github.com/cuenv/cuenv/schema"
```

## Neovim / Vim

For Neovim, we recommend using [nvim-lspconfig](https://github.com/neovim/nvim-lspconfig) to configure the CUE Language Server (`cuelsp`).

1. Install the `cue` binary (which includes the LSP).
2. Configure `lspconfig`:

```lua
require'lspconfig'.dagger.setup{} -- Dagger's cuelsp is often used, or generic 'cuelsp'
```

*Note: The official CUE LSP is evolving. Check the [CUE documentation](https://cuelang.org/docs/) for the latest setup instructions.*

## IntelliJ / GoLand

JetBrains IDEs have a [CUE plugin](https://plugins.jetbrains.com/plugin/10480-cue) available.

1. Install the "CUE" plugin from the Marketplace.
2. Restart the IDE.
3. Navigate to **Settings > Languages & Frameworks > CUE** to configure the CUE executable path if necessary.
