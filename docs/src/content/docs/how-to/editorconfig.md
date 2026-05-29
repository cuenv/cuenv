---
title: Generate EditorConfig
description: Generate .editorconfig files from typed .rules.cue editorconfig blocks with cuenv sync
---

import { Aside } from '@astrojs/starlight/components';

A `.editorconfig` keeps indentation, line endings, and charset consistent across
every editor on a team. The problem is that those files are copy-pasted by hand,
drift between directories, and quietly disagree with each other over time.

cuenv treats `.editorconfig` the same way it treats `.gitignore` and CODEOWNERS:
as a generated artifact. You describe editor settings once in a typed
`.rules.cue` file, and `cuenv sync` writes the `.editorconfig` next to it. One
schema, validated by CUE, instead of hand-maintained INI files scattered across
the repository.

<Aside type="caution" title="Partial support">
EditorConfig generation is **partial**. The `#EditorConfig` and
`#EditorConfigSection` schema is wired into the default `cuenv sync` rules
provider and backed by real `.rules.cue` fixtures, but reference docs and
section-glob validation examples are still being filled in. Confirm current
behavior against the [schema status](/reference/schema/status/) before relying on
it in a strict pipeline.
</Aside>

## Quick Start

Create a `.rules.cue` file with an `editorconfig` block:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules & {
    editorconfig: {
        "*": {
            indent_style:             "space"
            indent_size:              4
            end_of_line:              "lf"
            charset:                  "utf-8"
            insert_final_newline:     true
            trim_trailing_whitespace: true
        }
        "*.md": {
            trim_trailing_whitespace: false
        }
    }
}
```

Then run:

```bash
cuenv sync
```

This generates an `.editorconfig` in the same directory as the `.rules.cue`
file. This example is derived from `examples/env-basic/.rules.cue`.

## Configuration

The `editorconfig` block maps **glob patterns** to `#EditorConfigSection`
settings. Patterns use EditorConfig glob syntax, such as `*`, `*.md`, or
`Makefile`. The wildcard `*` is the common base, and more specific patterns
override it for matching files.

### Section fields

Every field on `#EditorConfigSection` is optional and maps directly to a
standard EditorConfig property:

| Field | Values | Description |
| --- | --- | --- |
| `indent_style` | `"tab"` \| `"space"` | Indentation character |
| `indent_size` | `int` \| `"tab"` | Columns per indent level, or `"tab"` to reuse `tab_width` |
| `tab_width` | `int` | Columns used to display a tab character |
| `end_of_line` | `"lf"` \| `"crlf"` \| `"cr"` | Line ending style |
| `charset` | `"utf-8"` \| `"utf-8-bom"` \| `"utf-16be"` \| `"utf-16le"` \| `"latin1"` | Character encoding |
| `trim_trailing_whitespace` | `bool` | Remove trailing whitespace on save |
| `insert_final_newline` | `bool` | Ensure files end with a newline |
| `max_line_length` | `int` \| `"off"` | Soft line-length limit, or `"off"` to disable |

Because the schema is typed, invalid values are rejected during CUE evaluation
rather than silently written into an INI file. For example, `indent_style: "tabs"`
fails to evaluate because only `"tab"` and `"space"` are allowed.

### Per-glob overrides

Group shared defaults under `*` and override per file type. This block is
derived from the repository-root `.rules.cue`:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules & {
    editorconfig: {
        "*": {
            indent_style:             "tab"
            indent_size:              4
            end_of_line:              "lf"
            charset:                  "utf-8"
            insert_final_newline:     true
            trim_trailing_whitespace: true
        }
        "*.rs": {
            indent_style: "space"
            indent_size:  4
        }
    }
}
```

Here every file uses tabs by default, while Rust files (`*.rs`) switch to
four-space indentation.

<Aside type="note" title="root = true is automatic">
You do not set `root = true` yourself. cuenv auto-injects `root = true` into the
`.editorconfig` generated at the repository root, which stops editors from
walking further up the directory tree. Nested directories that produce their own
`.editorconfig` are not marked as root.
</Aside>

## Commands

EditorConfig is handled by the default `cuenv sync` rules provider, alongside
ignore files and CODEOWNERS. There is no editorconfig-specific subcommand.

Generate or update all rules-managed files:

```bash
cuenv sync
```

Preview changes without writing them:

```bash
cuenv sync --dry-run
```

Validate that generated files are up to date in CI:

```bash
cuenv sync --check
```

Sync every `.rules.cue` across the workspace when a repository has more than one:

```bash
cuenv sync -A
```

## See Also

- [Ignore files](/how-to/ignore-files/) - `.rules.cue` ignore generation
- [CODEOWNERS](/how-to/codeowners/) - `.rules.cue` ownership rules
- [Schema status](/reference/schema/status/) - current rules schema coverage
