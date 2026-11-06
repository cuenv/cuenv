# Write-only infrastructure resource arguments

Issue: [#571](https://github.com/cuenv/cuenv/issues/571).
Dependency [#570](https://github.com/cuenv/cuenv/issues/570) is implemented by
`0eb7da22` / [#572](https://github.com/cuenv/cuenv/pull/572).
Infrastructure remains experimental.

## Native provider arguments

Preserve the provider's Terraform attribute names and compose input constraints:

```cue
#Value: string | #Secret
#WriteOnly: #Secret & {writeOnly: true}

value_wo: #Value & #WriteOnly & schema.#OnePasswordRef & {
    ref: "op://Infrastructure/Cloudflare root/credential"
}
```

Generated configuration supplies `#Value & #WriteOnly` on the native write-only
string field. Users write `value_wo: schema.#WriteOnly & <secret>`, with the
provider's companion version argument separately. cuenv currently requires a
secret descriptor; Terraform itself also permits literal write-only input.
The marker survives CUE export but never reaches the resolver or provider as metadata.

The live attribute's `write_only` flag is authoritative for every provider and
resource. An ordinary `value` and a write-only `value_wo` remain independent
arguments, and provider validation owns any conflict between them. Attribute
suffixes have no semantic meaning to cuenv. A write-only field without `_wo`
works identically, and an ordinary field ending in `_wo` remains ordinary.
Requiredness, nested paths, state and outputs all retain the original schema;
write-only attributes never become output reference sources or destinations.
Marking an ordinary string as write-only fails during classification, before
secret resolution or provider configuration/resource calls.

This replaces 0.69.0's limited logical aliases. There is no alias table, schema
projection, inferred pairing or configuration-path translation. Existing alias
users must use the provider's native write-only field. No lockfile format change
or state migration is needed. Managed write-only schemas use contract marker 2
in the schema digest to fence generated configurations from older runtimes,
including the contract-1 runtime in 0.69.0.
Affected existing pins require `cuenv infrastructure provider add <source>@<same-version>`;
sync/init verify pins and cannot update this digest. Providers without write-only
attributes retain their existing digest rules. Declarations with neither an applicable lock pin nor `schemaDigest` have
no downgrade fence: use a compatible cuenv build for every plan/apply.

## Execution and persistence

- Classification retains native provider paths and the original marked descriptor
  for CUE specialization/integrity checks. No paths are translated.
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
- The digest domain remains version 5 and includes structured provider paths and
  presence, never write-only values, descriptor payloads or their hashes.
  Text plans show `value_wo = (write-only)`; JSON plans remain value-free.

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
composition on native fields, value-free plans/state pull, ordinary-field marker
and literal rejection before resolution, and
resolver-free state/destroy operations. The existing fake-provider wrapper injects
invalid MessagePack/JSON/unknown states after the Plugin Framework's own checks.

Parallel security, operations, Terraform protocol, CUE/schema and maintainer
reviews identified and drove fixes for unknown ancestors, nested requiredness,
ordinary `writeOnly` field compatibility, null replacement paths, redactor overlap
and Unicode escaping, and descriptor identity preservation. Findings and final
validation results are recorded in the pull request.

The native-name correction was reviewed by the same five personas. Confirmed
findings: marker eligibility must move into schema classification when removing
the binding pass; names must not grant semantics; ordinary and write-only fields
must remain independent; the digest must fence contract 1; optional null is absence, while required null
and invalid write-only schemas still fail. All are addressed by
the correction and its regression tests.

Run focused Nix development-shell tests/clippy and the infrastructure end-to-end
flake check, schema documentation validation and formatting. Cross-crate runtime
and provider-boundary changes trigger the final
`nix flake check -L --accept-flake-config` gate before review readiness.

Automatic versioning, companion-name inference, non-string secret conversion,
ephemeral resources and set support remain deferred.
