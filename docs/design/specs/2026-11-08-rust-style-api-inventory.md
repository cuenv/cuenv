# Rust style: signature audit

cuenv follows the [Google Rust Style Guide](https://google.github.io/styleguide/rustguide.html)
and its project rule against boolean function parameters and more than three
non-receiver parameters. The Google guide permits a boolean when its meaning is
clear; the project rule is stricter.

The `cuenv-rust-parameter-style` Nix check parses Rust files under `crates`,
`apps`, and `tools`, including tests and helpers. It rejects direct boolean
parameters, more than three non-receiver parameters, and loop headers that
chain two or more common iterator adapters. Trait declarations are
checked; trait implementation methods and foreign ABI signatures are exempt
when their signatures are fixed externally. Optional `From<bool>` implementations
are checked and are not used as a way around the project rule. Read, traversal, and parse
errors fail the check. Build output directories are excluded.
The companion function enumerator identifies workspace crates by package name
and reports missing manifests, unsupported member globs, source read,
traversal, and parse errors instead of producing a partial inventory.
Clippy checks additionally reject unnecessary iterator `for_each` calls in
the workspace, Cuetty, and the enumeration tool.
Application and tool sources must also appear in the declared flake task inputs;
undeclared Rust paths fail evaluation until they are added to
`nix/source-files.json`. A Rust file outside the scanner roots fails evaluation
until the checker and flake guard are extended. The guard excludes generated
`target`, `node_modules`, `result`, and `.git` trees before walking.

The checker operates on syntax before macro expansion and does not inspect
Rustdoc examples, resolve type aliases or method types, or infer the meaning
of boolean values inside containers. Those cases still require API review. The remaining trait
signatures include framework callbacks; calls that select behavior use named modes
or option structures.

The audit checks signatures rather than every judgment-based rule in the style
guide. New APIs should follow the same project rule, and changes to existing
APIs should migrate their callers rather than retaining ambiguous switches.
