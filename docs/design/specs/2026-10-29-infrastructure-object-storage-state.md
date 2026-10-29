# S3-Compatible Object Storage as an Infrastructure State Backend

Status: **proposed**. The decision record is
[ADR-0012](../../src/content/docs/decisions/adrs/adr-0012-per-project-object-storage-state.md),
and the issue is #548. `cuenv infrastructure` remains experimental. This
document specifies how the backend is built. Its last section records the
review that produced it.

## Goal

Store each project's infrastructure state in its own object in an
S3-compatible bucket (AWS S3, Cloudflare R2, MinIO), with its own format
version. Upgrading cuenv in one project must never change another project's
state. Keep every guarantee the SQL backends give: the lock check and the
write are atomic, compare-and-swap works, ownership is enforced, recovery
files are bound to the backend, and nothing secret appears in errors.

```cue
infrastructure: state: s3: {
	bucket:          "acme-cuenv-state"
	region:          "auto"
	endpoint:        "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com"
	accessKeyId:     schema.#OnePasswordRef & {ref: "op://platform/r2-state/access-key-id"}
	secretAccessKey: schema.#OnePasswordRef & {ref: "op://platform/r2-state/secret-access-key"}
}
```

## Non-goals

- One object per resource (see the ADR).
- A Durable Object or other deployed service as a backend.
- Issuing prefix-scoped credentials. The layout allows them; documentation
  recipes ship, automation does not.
- Lock expiry or leases.
- An intent journal written before each provider call.
- `state push`.
- Reading or writing bucket lifecycle and versioning configuration.
- A module-wide uniqueness rule for projects without `infrastructure`.
- Changing the SQL schema, apart from preserving fence rows (see Migration).

## Keys

### Layout

```
[<root>/]<module>/_<package>/<project>/<env>/state.json
[<root>/]<module>/_<package>/<project>/<env>/history/<serial:020>.json
[<root>/]<module>/_<package>/<project>/<env>/.probe-<random>          (transient)
[<root>/]<module>/_<package>/<project>/_migration.json
[<root>/]_locks/<module>/_<package>/<project>/<env>/<lock id>.json
```

- **`<module>`** is the `module:` of `cue.mod/module.cue`, without any `@vN`
  suffix (as `TenantKey` already does).
- **`<package>`** is the CUE package of the evaluated `ProjectInstance`.
- **`<project>`** is the project's `name`.
- **`<env>`** is the `--env` name, or `_default` without `--env`.

`TenantKey` does not change. `S3StateStore` is constructed for one invocation
and holds the package. `TenantLock` gains `package: Option<String>`, which the
SQL stores set to `None`.

### Validation (`state/object_key.rs`)

Every segment is checked before any request is built, and failures fail
closed:

| Segment | Rule |
| --- | --- |
| module path | CUE module rules: lowercase ASCII `[a-z0-9._-]`, each element starts with `[a-z0-9]`, first element contains a `.`, no empty element, no `..` |
| `root` | Elements `^[a-z0-9][a-z0-9_-]*$`. No `.`, so the first dotted element of any key is where the module path starts. |
| package | `^[A-Za-z][A-Za-z0-9_]*$` |
| project | `^[A-Za-z0-9][A-Za-z0-9._-]*$`. Never `.`, never `..`, no `..` anywhere in the name. |
| environment | The `#InfrastructureName` regex, enforced in Rust. State-only commands accept undeclared `--env` values, so the schema alone does not cover this. |
| environment set | No two declared environment names may differ only by case, because MinIO on a case-folding filesystem would merge them. |

Every allowed character is RFC 3986 unreserved, so segments are used
verbatim. The request builder then asserts that `Url::path()` equals the
expected key byte for byte. That defeats dot-segment normalisation and any
encoding difference from what was signed.

- `_locks` cannot be a module path's first element, because the first element
  must contain a `.`.
- `_<package>` cannot be a module path element, because elements start with
  `[a-z0-9]`.
- `_default`, `_migration.json` and `history` cannot be environment names,
  because those start with a letter. The project-level `_migration.json` name
  starts with `_`, so it cannot collide with an environment directory either.

### Location display

The full `s3://<bucket>/<key>` of `state.json` appears in:

- the `plan`, `apply`, `destroy` and `state list` headers;
- `--json` output, as `stateLocation`;
- every lock, owner, vanished-state and rename error.

## `state.json`

```json
{
  "format": {"read": 1, "write": 1},
  "writtenBy": "cuenv 0.62.0",
  "serial": 42,
  "writer": "6f1c…",
  "tenant": {"modulePath": "github.com/acme/infra", "package": "cuenv",
             "project": "web", "environment": null},
  "owner": {"instance": "services/web:cuenv", "claimedAt": "2026-10-29T12:00:00Z"},
  "lock": {"lockIdentifier": "…", "holder": "…", "acquiredAt": "…",
           "cuenvVersion": "0.62.0",
           "lastChange": {"address": "random_pet.pet", "action": "create", "at": "…"}},
  "lastReleased": "…",
  "migration": null,
  "tombstone": null,
  "records": {"random_pet.pet": { "...": "ManagedResource as JSON" }}
}
```

### Fields

- **`serial`** increases on every write and never resets for a key. A
  re-created or restored object is seeded from the highest history serial.
- **`writer`** is a fresh random value on every write. Together with `serial`,
  it means no two bodies are ever identical, so an ETag cannot repeat (no ABA,
  even where the ETag is a content MD5).
- **`tenant`** must equal the identity the key encodes. A reader refuses any
  object whose `tenant` does not match, which stops copied or planted objects.
- **Records** keep `generation` and `serial` exactly as in SQL
  (`RecordVersion`).
- **Unknown fields are preserved** at every level (serde flatten into a
  retained map), so a writer that is allowed to write never drops fields it
  does not know.
- **Stored text is untrusted.** The holder, owner, tombstone and manifest text
  can be written by anyone with bucket write access. Control characters are
  stripped before display (as in `sql.rs`), and any key or command built from
  this text is re-validated against the rules above before it is shown.

### Format versions

- A reader refuses `format.read` newer than it knows. A writer refuses
  `format.write` newer than it knows. The refusal names the cuenv version the
  object needs and its `writtenBy`.
- A writer keeps the object's existing format numbers. An additive field that
  older writers carry safely through preservation bumps nothing. No ordinary
  command ever raises either number.
- The first release writes `{read: 1, write: 1}` only. `cuenv infrastructure
  state upgrade` ships together with the first bump and is the only way to
  apply it. The refusal message names that command once it exists.
- **Conformance test:** a write by build N followed by a refresh-only apply by
  build N+1 leaves the object writable by N.

### Size

The cap is 16 MiB. Reads are bounded at the cap.

- At 50% of the cap, a warning is printed.
- Above 90%, an apply that would create resources refuses before any provider
  call. `destroy`, `state remove`, `state restore` and applies with no creates
  are never refused, so a full tenant can always shrink.

## Write protocol

Every mutation follows the same steps:

1. GET `state.json` and keep the exact ETag string.
2. Check the object: tenant, format, and that the lock in it is the writer's
   own (for mutations that need the lock).
3. Build the new body with `serial + 1` and a fresh `writer`.
4. PUT with `If-Match: <etag>`. Creation uses `If-None-Match: *`.

### ETags

- ETags are opaque. They are echoed back byte for byte, quotes included.
- MD5 is never computed or compared. Multipart upload is never used.
- A weak (`W/`) ETag means a caching layer sits in front of the store, so it
  is refused.
- Only the S3 API endpoint is supported, never an R2 custom domain: cached
  domains have relaxed consistency.

### Outcomes

| Response | Meaning | Action |
| --- | --- | --- |
| 200 | written | record the new ETag |
| 412 on the first send | someone else wrote | re-read. If our lock is gone: `LockLost`. Otherwise re-derive and retry (bounded). |
| 412 on a retry, 409 (`ConditionalRequestConflict`), 5xx, timeout, or a connection lost after sending | ambiguous | GET, then decide as below. Never re-send blind. |
| 404 on `If-Match` (AWS), or the object is absent while we hold a lock | the object vanished | `LockLost` naming the key and pointing to `state restore`. Never re-create. |
| 429 | not applied | wait for `Retry-After`, then retry |
| 403 / `ExpiredToken` | permanent | error. `ExpiredToken` names `state recover`. |

Deciding after an ambiguous response, by operation:

- **acquire:** the lock id in the body is ours → acquired. Otherwise report
  that the lock "may be held as `<id>`", as SQL does.
- **record write:** our `writer` and `serial` are present → recorded (the
  `ConditionalPut::is_recorded` analogue).
- **release or force-unlock:** `lastReleased` equals the id → released.
- **create with `If-None-Match`:** our `writer` is present → created.

### Pacing

- Writes to one tenant's `state.json` are at least `minWriteInterval` apart.
  The default is `"1s"` when the endpoint host ends in
  `.r2.cloudflarestorage.com`, and `"0s"` otherwise.
- **Verification item:** whether R2 returns 429 for sequential writes faster
  than one per second, or only for concurrent ones.

## `StateStore` mapping

| Method | Requests |
| --- | --- |
| `migrate()` | The capability probe, once per process, run under the lock path only. Touches no data. |
| `list(tenant)` | One GET of `state.json`. Absent means no records. |
| `addresses(tenant)` | One GET. Records are decoded as raw `Value`s, so one bad record cannot fail the listing. |
| `put` / `put_if_unchanged` / `delete` | The write protocol. The record version is checked inside the body that is read. |
| `put_many(tenant, lock, &[ManagedResource])` | New trait method, lock-fenced. The S3 store applies the whole slice in one write and is atomic; the default implementation loops over `put` and is documented as not atomic. |
| `acquire_lock` | Write the marker (`If-None-Match: *`), run the vanished-state guard, then write `state.json` with the lock set (`If-Match`, or `If-None-Match: *` for a new tenant). If acquisition definitely fails, delete the marker. |
| `unlock` | Write a history snapshot if `serial` advanced since acquire. Clear the lock and set `lastReleased` (`If-Match`). Delete our marker. |
| `force_unlock(tenant, id)` | GET, compare the id, then an `If-Match` write that clears the lock. Never touches records. |
| `current_lock` | One GET. |
| `locks()` | List `_locks/` (`encoding-type=url`), parse each key with the rules above, and GET the `state.json` built from the parsed segments. Report only locks that `state.json` confirms. Holder details come from `state.json`, never from the marker body. Stale markers are ignored. |
| `owner` / `claim_owner` | One GET; the write protocol. |
| `recovery_identity()` | sha256 of `cuenv-infrastructure-s3-recovery-v1\0` + the normalised endpoint host + `/` + bucket + `/` + root. Region, addressing and credentials are excluded, so a laptop and CI agree. |

### Engine changes

- **Refresh batching.** The engine still updates rows and emits events for
  each refresh as it happens, but defers the store write.
  - Deferred writes are flushed with `put_many` on the success, failure and
    interrupt paths, before the lock is released.
  - A failed flush is reported as an error and is never routed to
    `UnrecordedStore`, because a refresh created nothing.
  - Each address has a single action per schedule, so a deferred refresh is
    never overwritten by a later change in the same run.
- **Provider changes are never batched.** Each one is written with
  `put`/`put_if_unchanged` as soon as it happens, as today.

## Lock markers

The marker is written first, so every failure leaves at worst a stale marker,
which `locks()` ignores. A held lock is never hidden from `locks()`.

- **Acquire:** write the marker (`If-None-Match: *`, with the pre-generated
  id from `StateLock::generate`), then acquire in `state.json`.
- **Release:** clear the lock in `state.json`, then delete the marker.
- **Deletion:** a marker's key is unique per lock id, so deleting it without a
  condition can never remove another holder's marker.

## History and restore

### Snapshots

- `unlock()` writes `history/<serial:020>.json` when `serial` advanced since
  the lock was acquired. The snapshot is a copy of the committed body with
  `lock: null`.
  - The write uses `If-None-Match: *`. A 412 is resolved by a GET, and the
    snapshot counts as ours only if its `writer` matches.
  - A failed history write only warns.
- Pruning keeps `history` snapshots (default 10).
  - Snapshots are sorted by their parsed serial, never by list order.
  - Pruning runs only after the new snapshot's PUT is confirmed, and never
    deletes the snapshot just written.
  - `history: 0` deletes existing snapshots on the next write and disables
    the vanished-state guard.
- Reads of a snapshot apply the same tenant and format checks as reads of
  `state.json`.

### `cuenv infrastructure state restore [--serial N] [--yes]`

1. Refuse while the tenant has pending unrecorded changes.
2. Show the snapshot's serial, its `writtenBy` and the record addresses, then
   ask for confirmation unless `--yes` is given.
3. Write the snapshot back:
   - **When `state.json` exists:** under the tenant lock, an `If-Match` PUT of
     the snapshot's records and owner, with `serial = current + 1`, a fresh
     `writer` and the caller's lock.
   - **When `state.json` is absent:** create it with `If-None-Match: *` in a
     single write that carries the restorer's lock,
     `serial = max(history serial) + 1` and a fresh `writer`. A lost response
     is resolved by a GET on `writer`. This is the only path that may
     re-create a vanished `state.json`.
4. Give every restored record a fresh `generation`, so a rewound record can
   never match a `RecordVersion` an older unrecorded file expects.
5. Write a history entry.

Docs say never to copy history objects over `state.json` by hand. A hand copy
brings back the snapshot's lock and repeats an earlier ETag.

### Vanished-state guard

cuenv never deletes `state.json`: `destroy` leaves an empty object behind, and
`state rename` tombstones it. When acquire finds `state.json` absent, it
lists the tenant's `history/` and reads the project's `_migration.json`.

- **History exists, or the manifest lists the tenant:** refuse with "state
  vanished at s3://…", naming the highest history serial and
  `state restore`.
- **The listing or read fails** (403, timeout, truncated): refuse. The tenant
  is never treated as new on a failed check.
- **Neither exists:** the tenant is new. Create it with `If-None-Match: *`.

## Ownership

The owner record (`<directory>:<package>`) keeps today's behaviour: claimed
by the first apply or destroy, checked before any project secret is resolved.

When the recorded owner's directory is absent from this module's evaluation,
is not skipped (not `_`/`.`-prefixed, holds no `cue.mod`), and this instance
declares the same project:

- `plan` reports that the state is owned by `<old dir:pkg>`, which no longer
  declares the project;
- `apply`, `destroy` and `import` refuse and print the exact
  `cuenv infrastructure state adopt` command, with `-p` and `--package`;
- `state adopt` performs the transfer (`claim_owner(Transfer)` under the
  lock). It succeeds only if the owner in the body it read equals the
  `dir:pkg` that this invocation's evaluation judged absent. Otherwise it
  fails with `OwnedByAnotherInstance`.

The SQL backends gain the same help text and keep their behaviour otherwise.

## Rename detection

This runs when acquire finds the tenant new (no `state.json`, nothing for the
vanished-state guard). The command lists `<module>/_` across all packages and
treats a `state.json` as a probable rename when all of these hold:

- its owner **directory** equals this instance's directory;
- the module-wide evaluation the command already runs **no longer declares**
  its (package, project, environment). The no-flag identity counts as
  declared when the project has a top-level `infrastructure` configuration;
- it holds **at least one record or a held lock**. Destroyed or never-applied
  tenants are ignored.

`plan` reports a probable rename. `apply`, `destroy` and `import` refuse,
name the old identity, its record count and its location, and offer:

1. **The same project:** `cuenv infrastructure state rename …`. This option is
   offered only in a build that ships `state rename`.
2. **Retiring the old identity:**
   - `cuenv infrastructure state remove <address> --env <old>` works while the
     old identity is undeclared (state-only commands accept that).
   - `destroy` needs the old identity's configuration declared again first.
   - When the hit is only a held lock, the refusal also prints the `unlock`
     command.
3. **A separate project on purpose:** `--new-identity`.
   - It skips rename detection only. It never bypasses the owner check or the
     vanished-state guard, and a conformance test asserts both.
   - It is needed once: after the first write the tenant exists and detection
     no longer runs. Docs say not to add it to CI permanently.

Not detectable, and documented as such: a renamed module path, and a project
renamed and moved to a new directory in the same change.

Conformance tests:

- adding an environment;
- a second package in the same directory;
- destroying an environment, removing it, then adding another (allowed);
- removing an environment that still has records, then adding another
  (refused, then allowed with `--new-identity`);
- a package rename (detected);
- a project rename (detected).

### `cuenv infrastructure state rename --to-project P [--to-package K] [--to-env E]`

1. Lock the source.
2. Create the destination with `If-None-Match: *`, locked by the rename id,
   with `serial` continuing from the source.
3. Verify the destination.
4. Tombstone the source with `If-Match`: `tombstone: {movedTo: <key>}`, no
   records.
5. Unlock the destination.

Every step can be resumed by the rename id. Tombstones are displayed, never
followed. Their target is re-validated before it is displayed. The command is
named `rename` because Terraform's `state mv` moves addresses.

## Transport

- **reqwest with the existing `state/http.rs` rules:** no redirects, bounded
  bodies, timeouts, transient classification and `Retry-After`. 409 and AWS's
  404 on `If-Match` are classified as described in the write protocol.
- **SigV4, hand-written** on the `hmac` and `sha2` crates already in the
  lockfile:
  - header authentication only;
  - `x-amz-content-sha256` over the full payload;
  - canonical URI equal to the asserted request path;
  - checked against the AWS SigV4 test-suite vectors.

  `aws-sigv4` was rejected: it brings in four aws-smithy and credential crates
  plus `time`.
- **`quick-xml`** parses ListObjectsV2 responses and error bodies only. Bodies
  are bounded, DTDs are not processed, it is checked by `deny.toml`, and the
  `cargo tree` delta goes in the PR. Listings are capped in pages and total
  keys, and every key must start with the requested prefix and parse.
- **Headers:** no `x-amz-checksum-*`, and no `x-amz-server-side-encryption`
  (R2 rejects it). `x-amz-content-sha256` already gives payload integrity.
- **Addressing:** `auto` uses path style for an IP or `localhost` host, or
  when the bucket name contains `.`, and virtual-hosted style otherwise.
- **Redaction:**
  - `accessKeyId`, `secretAccessKey` and `sessionToken` are redacted from
    Debug output and errors;
  - the S3 error-body fields `StringToSign`, `CanonicalRequest` and
    `SignatureProvided` are stripped;
  - errors never quote the endpoint;
  - `reqwest::Error::without_url` is used, as in `d1.rs`;
  - a wrong region is reported with the `x-amz-bucket-region` response header.

## Capability probe

The probe runs from `migrate()`, once per process. It writes only random keys
inside the tenant prefix, so prefix-scoped credentials work and no
bucket-level permission is needed.

| Check | Run by | Expectation |
| --- | --- | --- |
| `If-None-Match: *` PUT, then a second identical PUT (own random key) | write commands | the second returns 412 |
| `If-Match` with a wrong ETag (own random key) | write commands | 412 |
| ETag strength | write commands | strong (not `W/`) |
| An unsigned GET of a probe key | all commands | must not return 200, because a public bucket means public secrets |

- A 429 during the probe is retried after `Retry-After`. It is never read as
  "conditions ignored".
- Probe objects are deleted without a condition afterwards.
- Read-only commands (`state list`, `state pull`, `state locks`) run only the
  unsigned-GET check, so read-only credentials work.
- **On R2 the unsigned-GET check cannot detect public access.** Public access
  on R2 goes through r2.dev or a custom domain, not the S3 endpoint. Output
  and docs say so, and tell users to keep r2.dev disabled and attach no custom
  domain to the state bucket.

## Configuration

### Schema

`schema/infrastructure.cue`:

```cue
#InfrastructureState: {
	turso?: {#TursoState}
	d1?:    {#D1State}
	s3?:    {#S3State}
}

#S3State: {
	bucket!:           string & =~"^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$"
	region!:           string & =~"^[a-z0-9-]+$"   // R2: "auto"
	endpoint?:         string                       // default https://s3.<region>.amazonaws.com
	root?:             string & =~"^[a-z0-9][a-z0-9_-]*(/[a-z0-9][a-z0-9_-]*)*$"
	addressing:        *"auto" | "virtual-hosted" | "path"
	accessKeyId!:      #InfrastructureStateToken
	secretAccessKey!:  #InfrastructureStateToken
	sessionToken?:     #InfrastructureStateToken
	minWriteInterval?: string
	history:           *10 | int & >=0
}
```

### Rules

- **Endpoint:** `https://`, or `http://` only for a loopback host (the Turso
  rule). No userinfo, query, fragment or path. A non-loopback `http://` is
  refused, because state carries plaintext provider secrets.
- **Bucket names with dots** require path addressing.
- **The "exactly one backend" error** lists all three backends.
- **The manifest type** gains an `S3` variant with `deny_unknown_fields`.
  Rust repeats every check before any secret is resolved.
- **Credentials:** `connect()` and `StateCredential` resolve two or three
  secrets instead of one. No ambient AWS credential chain is read (no
  environment variables, no `~/.aws`, no IMDS). Environment use is explicit
  through `#EnvPassthrough` (for example `AWS_ACCESS_KEY_ID` from a CI OIDC
  exchange).
- **`#InfrastructureStateToken`:** its "one token" comment is updated.

## Migration from SQL

### Flow

1. Keep `state` naming the SQL backend and add
   `infrastructure: migrateStateTo: s3: {…}`. While it is set, `plan` and
   `apply` warn.
2. Run `cuenv infrastructure state migrate [--env E | --all]`. `--all`
   enumerates tenants from the source's records, not from the config, so it
   includes undeclared environments and the no-flag identity. It is
   idempotent and resumable.
3. Switch `state` to `s3` and remove `migrateStateTo`.

### Per tenant

1. Refuse if unrecorded changes are pending.
2. Acquire the source lock as the fence:
   - id `migrated-<migration id>`;
   - holder "migrated to s3://…/state.json by cuenv X at T. Builds older than
     X do not protect this fence. Roll back with
     `cuenv infrastructure state migrate --rollback`".
3. Create the target with `If-None-Match: *`, carrying `migration: {id,
   source}` and a migrator lock.
4. Read back and compare the records. A mismatch names addresses only, never
   values.
5. Commit by releasing the target lock.
6. Update `_migration.json`.

A rerun recognises its own migration id and continues.

### Manifest

`_migration.json` lists each source tenant and its status (`pending`,
`migrated`, `rolled-back`).

- With `state: s3`, `plan` and `apply` read the manifest when their tenant is
  absent. If it lists the tenant as not migrated, they refuse and print
  `state migrate --env E`.
- Every tenant name in the manifest is validated before it is printed.

### Source schema

Acquiring a SQL lock requires the current SQL schema. If the source is older,
`state migrate` refuses without `--upgrade-source`, and explains that the
upgrade breaks projects pinned to older cuenv on that database.

### SQL fence handling

- New builds' SQL `migrate()` ignores locks whose id starts with `migrated-`.
  Today any held lock blocks a migration (`MigrationOutcome::LockHeld`).
- Every SQL table rebuild preserves those lock rows. A conformance test
  asserts that the fence survives `migrate()`.
- `unlock` refuses a fence id without `--migration-fence`.
- Docs and the fence holder text say that builds older than this release
  release fences on request, so pinned machines must upgrade before a project
  migrates. Turso users can enforce the minimum version in CI.

### Rollback

`--rollback` refuses if the target's `serial` or ETag moved since the commit,
unless `--copy-back` is given, which copies back with the same verification.
It then tombstones the target and releases the source fence.

## CLI changes

| Command | Change |
| --- | --- |
| `state list` | header with the `s3://` location |
| `state locks` | defaults to the current module (prefix `<module>/_`). `--all` lists every lock and prints the `--path`/`--package` needed to release each one. A 403 is reported as "credential is prefix-scoped". The help text no longer says "every project". |
| `state pull` | new. Prints the tenant as JSON, built from trait reads (`list`, `owner`, `current_lock`) so it works on every backend. Help: the output contains plaintext provider secrets. |
| `state restore` | new (see History and restore) |
| `state migrate` | new (see Migration from SQL) |
| `state rename` | new, phase 8 |
| `unlock <id>` | prints holder, `cuenvVersion`, age and `lastChange` before releasing. Refuses fence ids without `--migration-fence`. |
| `apply` / `destroy` / `import` | `--new-identity` |
| `state upgrade` | not in this release (see Format versions) |

### Uniqueness

`check_unique_name` keeps its behaviour: every package, infrastructure
projects only, fail closed, run on `plan`, `apply`, `destroy`, `import` and
`state adopt`. Its comparison becomes case-insensitive. Release note: two
existing infrastructure projects whose names differ only by case (`Web` and
`web`) now fail, with no warning release.

## Documentation

These pages need adding or updating:

- **`how-to/infrastructure.md`:**
  - the S3 backend with AWS, R2 and MinIO examples, including R2's
    `https://<ACCOUNT_ID>.r2.cloudflarestorage.com` endpoint and
    `region: "auto"`;
  - the access-scope caution: bucket read access means reading every
    project's plaintext provider secrets, and bucket write access means apply
    authority over every project. Recommend one bucket per trust boundary;
  - **least-privilege recipes**, end-to-end tested:
    - AWS IAM on `<root>/<module>/_*` and `<root>/_locks/<module>/_*`, with
      `s3:prefix` conditions on ListBucket covering the tenant prefix;
    - R2 temporary credentials with the same prefixes;
  - **backups:** on AWS, enable bucket versioning with noncurrent-version
    expiry. On R2, history is the only backup;
  - **lifecycle rules:** never apply age-based expiry to live keys. Expire
    `history/` only, if at all. Explain how to purge a destroyed project's
    snapshots;
  - **history and secrets:** history keeps rotated secrets for up to `history`
    further writes;
  - **credentials:** keep the state credential separate from provider
    credentials. PR `plan` jobs hold the state credential;
  - **migration:** the migration flow and its fence caveat for older builds;
  - **R2:** keep r2.dev disabled and attach no custom domain to the state
    bucket;
  - **renames:** rename and adopt behaviour, and what cannot be detected.
- Replace the sentence "Moving a project to a different module or renaming it
  starts from empty state" with the new rename behaviour.
- `reference/cli.md`: every new or changed command and flag.
- `docs/design/specs/schema-coverage-matrix.md`: the `s3` block and
  `migrateStateTo`.
- `.agents/skills/` entries that describe state backends.

## Phasing

Each phase is one PR. The full flake gate runs where `CLAUDE.md` requires it:
new production dependencies, schema or CLI changes, and cross-crate behaviour.

1. **Conformance suite.** A generic `StateStore` suite over Memory, the Turso
   fake and the D1 fake, including fence survival across SQL `migrate()`.
2. **Keys.** `object_key.rs`, the case-insensitive `check_unique_name`, and
   environment case checks.
3. **Transport.** SigV4 (test vectors) and the S3 transport on `http.rs`,
   adding `quick-xml`. Full flake gate.
4. **Fake S3.** A stateful in-process stand-in with ETags,
   `If-Match`/`If-None-Match`, 409, 412, 404 and 429, dropped responses, and
   a mode that ignores conditions. `fake_server.rs` parses every body as JSON
   and cannot be reused as it is.
5. **Store.** The S3 store passing the conformance suite, plus `put_many` with
   the engine flush, lock markers, history, the vanished-state guard, and
   `state restore`.
6. **Wiring.** Schema, manifest and `connect` with multiple credentials,
   `state pull`, adopt hints, rename detection with `--new-identity`, `state
   locks` scoping, and docs. Full flake gate.
7. **Migration.** `state migrate`, `migrateStateTo`, the manifest, and SQL
   fence handling.
8. **Rename and verification.** `state rename`, an ignored MinIO end-to-end
   test, the least-privilege recipe test, and a verification record against
   real R2 and AWS S3 (including the sequential-write 429 question).

The backend stays experimental until phase 8's verification record is in this
document.

## Verification

Not yet run. To be filled in as in the D1 spec: what was probed against real
R2, AWS S3 and MinIO, what the conformance suite covers, and what was not
verified.

## Review record

A five-member review (storage correctness, SRE, security, maintainer, IaC
user) ran five rounds. The final round was unanimous.

| Round | Outcome | Main changes |
| --- | --- | --- |
| 1 | Layout A (one object per tenant) chosen 4–1 over one object per resource. Automatic ownership transfer blocked by three reviewers. | `TenantKey` left unchanged. Segment validation in Rust. Read/write format versions. The migration config inverted to `migrateStateTo`, so no window shows an empty tenant. |
| 2 | No blocks. | Lock markers written first. Zero-padded, create-only history. `put_many`. 404 handled as a vanished object. Manifest. `root` without dots. |
| 3 | Two blocks. | Rename detection limited to identities no longer declared. Restore can re-create a vanished object. |
| 4 | One block. | Destroyed tenants no longer count as renames. `--new-identity` added. |
| 5 | Unanimous. | Notes folded into this spec. |

### Corrections to the starting brief

- AWS supports conditional `DeleteObject` (since 2025-09).
- AWS conditional PUT can also fail with 409 or 404.
- R2 has no bucket versioning or object lock.
- R2 is strongly consistent only through the S3 API, not through cached
  custom domains.
- Refresh uses the unconditional `put` (`engine.rs`).
- `check_unique_name` also runs for `import`.
- `parse_module_path` and `with_environment` enforce none of the CUE grammar.
