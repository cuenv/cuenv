# Write-only infrastructure resource arguments

Issue: [#571](https://github.com/cuenv/cuenv/issues/571).
Dependency [#570](https://github.com/cuenv/cuenv/issues/570) is implemented by
`0eb7da22` / [#572](https://github.com/cuenv/cuenv/pull/572).
Infrastructure remains experimental.

## Value composition

Expose one logical value property and compose its constraints:

```cue
#Value: string | #Secret
#WriteOnly: #Secret & {writeOnly: true}

value: #Value & #WriteOnly & schema.#OnePasswordRef & {
    ref: "op://Infrastructure/Cloudflare root/credential"
}
```

Generated configuration supplies the base `#Value` constraint. Users write
`value: schema.#WriteOnly & <secret>` on that same field, with the provider's
companion version argument separately. Write-only input is always a secret;
a literal conflicts with the refinement. The marker survives CUE export but
never reaches the resolver or the provider as metadata.

CUE unification cannot change a provider's storage behavior. Explicit bindings
in `crates/infrastructure/src/write_only.rs`, shared by the generator and engine,
map the logical input to a real write-only provider attribute. They are keyed by
canonical provider source and resource type, never guessed by suffix:

| Provider | Resource | Logical field | Provider field |
| --- | --- | --- | --- |
| `hashicorp/aws` | `aws_ssm_parameter` | `value` | `value_wo` |
| `hashicorp/aws` | `aws_db_instance` | `password` | `password_wo` |
| `hashicorp/azurerm` | `azurerm_key_vault_secret` | `value` | `value_wo` |
| `example.com/test/fake` (tests) | `fake_write_only` | `value` | `value_wo` |

The live schema must have a nullable ordinary string argument and an optional
write-only string destination. Missing, ambiguous or incompatible bindings
fail before resolution/resource calls. Generated types omit the raw alias of a
bound pair. Standalone write-only string fields retain their provider names and
require the same refinement. Provider state and `#Outputs` keep provider names;
no write-only source or destination becomes an output reference.

Bindings are runtime-owned and verified against the schema; no user-supplied or
generated binding metadata is trusted. No lockfile/binding-identity field is
needed. Providers exposing managed write-only attributes add a write-only contract
marker to their schema digest, so an old binary cannot accept newly generated
types and silently send a marked secret to an ordinary persisted argument.
Affected existing pins require `cuenv infrastructure provider add <source>@<same-version>`;
sync/init verify pins and cannot update this digest. Providers without write-only
attributes retain their existing digest rules. Unpinned/untyped declarations have
no downgrade fence: use a compatible cuenv build for every plan/apply.

## Execution and persistence

- Classification retains logical paths and the original marked descriptor for
  CUE specialization/integrity checks. Only RPC configuration translates paths.
  Resolver payloads and memoization identity omit the marker, so one descriptor
  shared by ordinary and write-only arguments resolves once per command.
- Declaration validation keeps secrets unknown. Plan/apply resolve lazily after
  CUE evaluation and register values before giving them to a provider. Destroy,
  import, state commands and unlock never resolve resource argument secrets.
- Proposed state always has null at write-only slots. Strict validation checks
  every schema-marked slot, including unconfigured ones, on initial/final/create/
  replacement/destroy plans, upgrades, refreshes, imports and apply responses.
  Unknown leaves and unknown containers holding write-only descendants fail;
  null containers are allowed. Legacy-provider tolerance cannot bypass this.
- Reject an invalid apply response before compatibility fallback, tainting,
  `unknown_as_null`, state construction or recovery writes. Abort later mutations,
  retain prior state, and preserve replacement deletion aftermath. The remote
  object may already have changed; no rollback or recovery file is claimed.
- The confirmed plan retains null write-only configuration slots, descriptor/path
  metadata, and no write-only values in its digest or public debug output. Apply
  reconstructs RPC configuration from the command's memoized secret values and
  checks cancellation again before mutation.
- Preserve a provider's replacement request at a write-only path or its containing
  object even when both state values compare equal as null. Create-time spurious
  replacement paths remain ignored as before.
- The digest domain advances to version 5 and includes structured logical/provider
  path pairs and presence, never write-only values, descriptor payloads or their
  hashes. Text plans show `value = (write-only)`; JSON plans remain value-free.

**Rotation is manual.** A changed secret without a changed companion version does
not plan an update. cuenv does not infer version arguments or create fingerprints.
Switching an old persisted argument to write-only does not erase historical state.
Providers remain trusted programs: opaque private bytes and unrelated attributes
cannot be audited for arbitrary encodings of a value.

## Diagnostics and unsupported contexts

The ordinary event registry ignores strings shorter than four bytes. Each
provider therefore owns extra protection for every nonempty write-only value,
line fragment, JSON/debug escape, and Go JSON escape (including U+2028/U+2029).
A matching provider string is suppressed whole to avoid exposing fragments from
overlapping secrets or disrupting the ordinary redactor. This covers diagnostics,
paths, gRPC failures, live logs and retained logs; it expires with the provider.
Request and apply-step `Debug` output omits private payload bytes.

Numbers, booleans, structured write-only values, set elements, provider/data inputs
and ephemeral resources are outside this increment. Descriptor-shaped markers in
unsupported/dynamic positions fail closed. Ordinary schema-defined fields named
`writeOnly` or `resolver` keep their normal meanings. Ordinary environment/state
secret deserialization rejects the reserved marker instead of ignoring it.

## Validation and adversarial review

Focused tests cover schema/classification, null-state construction, generated
CUE types, digest behavior, malformed markers, nested requiredness, unknown
containers, replacement paths, provider receipt, manual rotation, short/escaped
redaction, failed create/update state rejection, and recovery-file absence.
The generated CLI lifecycle checks the actual `#WriteOnly & #ExecSecret`
composition, value-free plans/state pull, raw-alias and literal rejection, and
resolver-free state/destroy operations. The existing fake-provider wrapper injects
invalid MessagePack/JSON/unknown states after the Plugin Framework's own checks.

Parallel security, operations, Terraform protocol, CUE/schema and maintainer
reviews identified and drove fixes for unknown ancestors, nested requiredness,
ordinary `writeOnly` field compatibility, null replacement paths, redactor overlap
and Unicode escaping, and descriptor identity preservation. Findings and final
validation results are recorded in the pull request.

Run focused Nix development-shell tests/clippy and the infrastructure end-to-end
flake check, schema documentation validation and formatting. Cross-crate runtime
and provider-boundary changes trigger the final
`nix flake check -L --accept-flake-config` gate before review readiness.

Automatic versioning, companion-name inference, non-string secret conversion,
ephemeral resources and set support remain deferred.
