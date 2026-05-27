---
title: EditorConfig
description: Generate .editorconfig files from .rules.cue
---

cuenv generates `.editorconfig` files from directory-scoped `.rules.cue`
configuration. Each `.rules.cue` file writes `.editorconfig` in the same
directory, and the repository-root file gets `root = true` automatically.

## Quick Start

Create a `.rules.cue` file:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules & {
    editorconfig: {
        "*": {
            indent_style: "space"
            indent_size: 2
            end_of_line: "lf"
            charset: "utf-8"
            insert_final_newline: true
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

Validate generated files in CI with:

```bash
cuenv sync --check
```

## Section globs

The `editorconfig` map keys are EditorConfig section globs. Write the glob only;
cuenv adds the square brackets in the generated file.

Valid examples:

```cue
editorconfig: {
    "*": {indent_style: "space"}
    "*.rs": {indent_size: 4}
    "{Makefile,*.mk}": {indent_style: "tab"}
    "docs/**.md": {trim_trailing_whitespace: false}
}
```

Invalid examples:

```cue
editorconfig: {
    // Empty section globs are rejected.
    "": {indent_style: "space"}

    // Do not include the generated brackets in the key.
    "[*.rs]": {indent_style: "space"}
}
```

Section globs also cannot contain line breaks.

## Supported fields

| Field | Values |
| --- | --- |
| `indent_style` | `"tab"` or `"space"` |
| `indent_size` | integer or `"tab"` |
| `tab_width` | integer |
| `end_of_line` | `"lf"`, `"crlf"`, or `"cr"` |
| `charset` | `"utf-8"`, `"utf-8-bom"`, `"utf-16be"`, `"utf-16le"`, or `"latin1"` |
| `trim_trailing_whitespace` | boolean |
| `insert_final_newline` | boolean |
| `max_line_length` | integer or `"off"` |

Unknown fields are rejected. For example, use `indent_size`, not `indent`.

## See Also

- [Ignore files](/how-to/ignore-files/) - `.rules.cue` ignore generation
- [CODEOWNERS](/how-to/codeowners/) - `.rules.cue` ownership rules
- [Schema status](/reference/schema/status/) - current rules schema coverage
