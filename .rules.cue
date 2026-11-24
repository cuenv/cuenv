package cuenv

import "github.com/cuenv/cuenv/schema/rules"

// Define directory-scoped rules using the canonical struct shape to avoid
// deserialization ambiguities during isolated .rules.cue evaluation.
rules.#DirectoryRules & {
    // Example fixtures carry their own .rules.cue files; they are synced
    // per directory by their tests, never merged into this repository.
    exclude: ["examples"]

    // Ignore patterns for generated ignore files
    ignore: {
        git: [
            "_tests",
            ".cache",
            ".claude/worktrees",
            // Local cargo state; the shared config and audit ignores are tracked.
            ".cargo/*",
            "!.cargo/audit.toml",
            "!.cargo/config.toml",
            ".cuenv",
            ".test",
            ".worktrees",
            "*.vsix",
            "bdd_test_runs",
            "crates/cuengine/vendor",
            "crates/cuengine/libcue-bridge",
            "dist",
            "node_modules",
            "result",
            "sbom.json",
            // Quint / Apalache / TLC scratch output
            "specs/quint/_apalache-out",
            "specs/quint/tmp",
            "target",
        ]
    }

    // EditorConfig settings
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

    // Code ownership rules - aggregated to single CODEOWNERS at repo root
    owners: {
        rules: {
            default: {
                pattern: "**"
                owners: ["@rawkode"]
            }
        }
    }
}
