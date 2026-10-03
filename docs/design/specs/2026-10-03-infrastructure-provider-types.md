# Typed Infrastructure Configuration Generated from Provider Schemas

Status: **implemented** (experimental, like the rest of `cuenv infrastructure`).
Supersedes the cuenv/terraform module contract described in
`2026-09-27-terraform-provider-infrastructure-as-code-proof-of-concept.md`
(next step 12).

## Problem

`configuration` under `infrastructure.providers` and `infrastructure.resources`
is `{...}`: CUE accepts anything, and mistakes surface at plan time, after the
provider has been downloaded and launched, without file positions.

The first answer was a separate repository, `github.com/cuenv/terraform`, that
ran the Terraform CLI, rendered every provider's schema as a CUE module and
published it to a registry. In practice:

- The two repositories drifted. cuenv/terraform `main` switched to camelCase
  field names and `Resource.PascalCase` definitions; cuenv's engine keys
  configuration by Terraform attribute names and rejects the result
  (`unsupported argument 'overrideSpecial'`).
- Nothing tied the imported module to the provider binary. A 3.9.1 module next
  to a 3.9.2 binary type-checked arguments the binary does not accept.
- A whole-provider package is paid on every evaluation, including the shell
  hook: importing `hashicorp/aws` cost 1.3 s per prompt.
- Publishing needs a registry, a schedule, credentials and a Terraform CLI,
  and only covers the providers someone added to a list.

cuenv already fetches every provider's schema over gRPC to validate
configuration. It can render the same schema as CUE itself.

## Decision

cuenv generates the types locally, from the provider binary it will run, and
nothing is published.

```console
$ cuenv infrastructure provider add hashicorp/random@3.9.1
```

1. Installs the provider exactly as `plan` would (same cache, same checks) and
   records the registry's archive SHA-256 for every platform the release is
   published for.
2. Launches it, reads `GetProviderSchema`, and renders CUE into `cue.mod/gen`.
3. Pins version, schema digest and archive hashes in `cuenv.lock`.
4. Prints the import lines and the `providers` entry to paste. cuenv never
   edits the user's CUE files.

`cuenv sync infrastructure` regenerates every locked provider from
`cuenv.lock`; `--check` fails when the generated files differ, for CI.

`cuenv infrastructure provider remove <source>` deletes the generated package
and the lock entry.

The generated files are not committed. As with managed codegen files, each
provider directory is listed in a `cuenv infrastructure` section of the
module root's `.gitignore` (maintained by `provider add`, `provider remove`
and `sync infrastructure`), and `cuenv sync` recreates them from
`cuenv.lock`, the only committed artifact. Until it has run after a clone,
anything that evaluates the project fails with `cannot find package`; that
is the same contract codegen has, and `cuenv sync` is already the required
step after a clone. Generating on demand inside every evaluation was
rejected: it would make the shell hook and unrelated commands download
providers.

The typed path is covered end to end by
`generated_provider_types_type_the_project` in
`crates/cuenv/tests/infrastructure_lifecycle.rs` (the `cuenv-infrastructure-e2e`
flake check): it renders types from the test provider binary into a temporary
module, runs a typed plan, and checks that a misspelled argument fails
evaluation and a stale `schemaDigest` is refused.

cuenv/terraform is no longer part of the typing story. It can be archived.

## Generated layout

One CUE package for the provider block and one per managed resource type, so a
project only loads the resources it imports.

```text
cue.mod/gen/registry.terraform.io/hashicorp/random/
├── provider.cue                         package random_provider
└── resources/
    ├── random_password/resource.cue     package random_password
    ├── random_pet/resource.cue          package random_pet
    └── ...
```

- Import path: the fully qualified provider source address,
  `<hostname>/<namespace>/<type>`, for example
  `registry.terraform.io/hashicorp/random`. The first element always contains a
  dot, as CUE requires for non-standard-library imports, and nothing else can
  claim the path. Resources live under `.../resources/<resource type>`.
- CUE resolves these imports from `cue.mod/gen` without a `deps` entry, so
  `cue.mod/module.cue` is untouched and no registry is consulted.
- Package names: resource types (`random_pet`) are already valid identifiers.
  The provider package is the provider type with `-` replaced by `_` and
  `_provider` appended (`random_provider`, `google_beta_provider`), imported
  with a qualifier: `"registry.terraform.io/hashicorp/random:random_provider"`.
  The suffix is required, not cosmetic: a CUE field label shadows an import of
  the same name, so `providers: random: random.#Provider` resolves `random` to
  the field and fails with `imported and not used`.
- A registry hostname with a port cannot be an import path (`:` introduces a
  package qualifier); `provider add` refuses such sources.

## Generated definitions

```cue
// provider.cue
package random_provider

#Provider: {
	source:        "registry.terraform.io/hashicorp/random"
	version:       "3.9.1"
	schemaDigest:  "sha256:..."
	configuration: #Configuration
}

#Configuration: {}
```

```cue
// resources/random_pet/resource.cue
package random_pet

// #Resource declares a managed resource of type random_pet.
//
// The resource `random_pet` generates random pet names ...
#Resource: {
	type: "random_pet"
	configuration: #Configuration
	...
}

#Configuration: {
	// Arbitrary map of values that, when changed, will trigger recreation of resource. ...
	keepers?: {[string]: string}
	// The length (in words) of the pet name. Defaults to 2
	// The provider chooses a value when this is not set.
	length?: number
	// A string to prefix the name with.
	prefix?: string
	// The character to separate words in the pet name. Defaults to "-"
	// The provider chooses a value when this is not set.
	separator?: string
}
```

Used as:

```cue
import (
	"registry.terraform.io/hashicorp/random:random_provider"
	"registry.terraform.io/hashicorp/random/resources/random_pet"
)

infrastructure: {
	providers: random: random_provider.#Provider
	resources: pet: random_pet.#Resource & {configuration: length: 3}
}
```

- `#Provider` binds `source`, `version` and `schemaDigest` to the generated
  types, so they cannot disagree (a hand-written `version: "3.9.0"` is a CUE
  conflict). It is closed: `path` cannot be added to it.
- `#Resource` binds `type`. It stays open (`...`) for `provider` and
  `dependsOn`; `#ManagedResource` in cuenv's schema closes it.
- `#Configuration` is closed. Unknown arguments are evaluation errors with
  file positions.

### Field rules

| Provider schema                    | CUE                                        |
| ---------------------------------- | ------------------------------------------ |
| required attribute                 | `name!: T`                                 |
| optional, or optional and computed | `name?: T`                                 |
| computed only                      | omitted: configuration cannot set it       |
| write-only                         | omitted: cuenv does not send write-only values |
| `string` / `number` / `bool`       | `string` / `number` / `bool`               |
| `list(T)`, `set(T)`                | `[...T]`                                   |
| `map(T)`                           | `{[string]: T}`                            |
| `object({a = T})`                  | `{a!: T}` (every attribute must be given, as in Terraform) |
| `tuple([A, B])`                    | `[A, B]`                                   |
| `dynamic`                          | `_`                                        |
| nested attributes (protocol 6)     | the attribute rules, recursively           |
| nested block, single or group      | `name?: {...}`; `name!:` when `min_items` ≥ 1 |
| nested block, list or set          | `name?: [...{...}]`, with `list.MinItems`/`list.MaxItems`; `name!:` when `min_items` ≥ 1 |
| nested block, map                  | `name?: {[string]: {...}}`                 |

Optional attributes are not `T | null`. Terraform treats an explicit null like
an absent attribute and so does cuenv's engine, so `| null` adds nothing but
three error messages per mistake (one per disjunct). Descriptions, deprecation
(`// Deprecated: ...`), sensitivity and "computed when not set" become
comments. Names that are not plain identifiers are quoted, and so are names
that equal a CUE keyword or an identifier the generated code refers to
(`bool`, `number`, `string`, `int`, `float`, `bytes`, `list`): an identifier
label shadows the predeclared one for every reference in its struct
(`hashicorp/random`'s `random_password` has a `number` attribute next to
`length!: number`), while a quoted label cannot be referenced.

The generated files contain constraints only, no `error()`, so they work with
any CUE language version the consumer's module declares.

Not generated (yet): data sources, computed outputs for references between
resources, provider functions, ephemeral resources. They wait for engine
support; types for things the engine cannot use would mislead.

## Schema digest

`sha256:<hex>` over a canonical JSON document of what the generated
`#Configuration` definitions encode, for the provider block and every managed
resource:

- per block: each attribute's cty type (protocol 6 nested attributes
  recursively, with their nesting), `required`/`optional`/`computed`,
  `sensitive`, `write_only`, `deprecated`; each nested block's nesting mode,
  `min_items`, `max_items` and block.
- object keys sorted; resources sorted by type.
- excluded: descriptions and deprecation messages (cosmetic), schema versions
  (state upgrades, not configuration), server capabilities, protocol version.

The digest is a property of the schema, not of cuenv's renderer, so a cuenv
upgrade that changes the rendered text does not invalidate it (it only makes
`sync infrastructure --check` report the new text). A future change to what
the digest covers must use a new prefix.

## Lockfile

`cuenv.lock` gains a section keyed by the fully qualified source:

```toml
version = 5

[infrastructure_providers."registry.terraform.io/hashicorp/random"]
version = "3.9.1"
schema_digest = "sha256:..."

[infrastructure_providers."registry.terraform.io/hashicorp/random".platforms]
darwin_amd64 = "sha256:..."
darwin_arm64 = "sha256:..."
linux_amd64 = "sha256:..."
```

- `platforms` maps Terraform platform names to the archive SHA-256 the
  registry reports for that build. `provider add` records every platform the
  release is published for, so a lock made on Linux works on macOS.
- A lockfile with this section is format version 5. Lockfiles without it stay
  at version 4, so upgrading cuenv does not rewrite existing lockfiles. cuenv
  versions that only know version 4 refuse a version 5 lockfile instead of
  silently dropping the section in `cuenv sync lock`.
- `cuenv sync lock` preserves the section.
- One version per provider source per CUE module: the generated import path
  has no version, and the lock has one entry per source.

## Verification at plan time

After launching a provider, the engine computes the digest of the live schema
and refuses to continue when:

- the provider's `schemaDigest` (set by `#Provider`) differs: the types were
  generated from a different binary; run `cuenv sync infrastructure`.
- `cuenv.lock` pins the source at a different version than the configuration
  asks for, or at a different schema digest.

When `cuenv.lock` pins a provider, installation and cache reuse also require
the archive SHA-256 recorded for the current platform. A lock that has no
entry for the current platform is an error, not a fallback.

Projects without a lock entry and without `schemaDigest` behave as before:
untyped configuration still works and is validated at plan time.

## Commands

| Command | Effect |
| --- | --- |
| `cuenv infrastructure provider add <source>@<version>` | install, generate, lock, print snippet |
| `cuenv infrastructure provider remove <source>` | delete generated package and lock entry |
| `cuenv sync infrastructure` | regenerate every locked provider |
| `cuenv sync infrastructure --check` | fail when generated files are missing or differ |
| `cuenv sync infrastructure --dry-run` | report what would change |

`<source>` is `namespace/type` or `hostname/namespace/type`; `<version>` is an
exact version. `--path` selects the directory whose CUE module (the nearest
`cue.mod`) receives the files and the lock entry.

`sync infrastructure` only rewrites directories whose `provider.cue` carries
cuenv's generated-code header, and refuses otherwise rather than delete files
it did not write.

## Follow-ups

- Local `path` providers: `provider add --executable`.
- Generate only selected resources for very large providers (`hashicorp/aws`
  has about 1700 resource types).
- `#Outputs` per resource and references between resources (requires
  apply-time re-planning in the engine).
- Data sources.
- Verify the registry's GPG signature over `SHA256SUMS`.
