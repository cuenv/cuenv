---
id: ADR-0012
title: Per-Project Object-Storage State
status: Proposed
decision_date: 2026-10-29
approvers:
  - Core Maintainers
related_features: []
supersedes: []
superseded_by: []
---

## Context

Infrastructure state lives in one shared SQLite database (Turso or Cloudflare
D1). Every project that uses the database shares its schema version, so
upgrading cuenv in one project migrates the database for all of them. Release
0.61.0 moved the shared SQL schema to version 2. A project still on 0.60.1
that used the same D1 database could then no longer read its state, and
Turso has the same coupling. This is issue #548. Issue #549 deprecates the D1
backend.

The fix is to give each project its own unit of storage with its own format
version, in a store every major cloud offers: an S3-compatible bucket (AWS S3,
Cloudflare R2, MinIO). One bucket can hold every project's state.

The constraints that shape the design:

- **Identity.** A state identity (tenant) is the module path plus the project
  name plus the environment (`TenantKey`). The infrastructure commands that
  write state refuse to run when another instance anywhere in the module
  declares the same project name with an `infrastructure` block.
- **The `StateStore` contract.** It requires the lock check and the write to
  be atomic, and `locks()` to list every lock the store holds.
- **S3-compatible stores.** They offer conditional writes (`If-None-Match: *`
  to create, `If-Match` to replace) but no multi-object transactions.
- **Cloudflare R2 limits.** R2 allows one write per second to a single key,
  supports no conditional `DeleteObject`, and implements no bucket versioning.

A five-person review (storage correctness, operations, security, maintainer,
and user) went through five rounds until all five approved. Their objections
are recorded in the implementation spec.

After the review, the maintainer removed the ownership gate and made project
names unique within the evaluated package for every project. The review had kept the gate as a backstop against
a copied repository that keeps the original module path. That backstop did
not hold: a copy with the same directory layout passes the gate, and anyone
who can make the mistake usually holds the bucket credential anyway.

## Decision

### One object per tenant, under a key derived from the CUE module

```
[<root>/]<module path>/_<package>/<project>/<env | _default>/state.json
```

- **The key carries no directory**, so moving a project's directory keeps its
  state.
- **`_<package>` ends the module path.** CUE module path elements cannot start
  with `_`, so a nested module whose path extends another module's path cannot
  produce the same key.
- **The package is part of the key.** Two packages may use the same project
  name without sharing state.
- **Every key segment is validated in Rust** against an unreserved-character
  grammar before any request. `root` elements may not contain a `.`. The
  request path must equal the expected key byte for byte.
- **`TenantKey` is unchanged.** The object store takes the package from the
  evaluated CUE instance. The SQL schema is not changed. The SQL backends
  lose the ownership gate and record the last writer instead (see Ownership
  and renames).

`state.json` holds the format versions, the tenant identity, the last writer's
instance, the lock, and every managed resource record. Every write is a conditional PUT
on the ETag of the version it was derived from, made only after checking that
the lock in that version is the writer's own. So the lock check and the write
stay atomic, and the `StateStore` contract holds unchanged.

One object per resource was rejected. That layout puts the lock check and the
write in separate requests, which lets a paused writer whose lock was
force-released record changes against another run's view. It also cannot
serve the trait's unconditional `put` safely, and it gives readers torn
snapshots.

### Locks

- **Locks never expire,** as with SQL. A lock is released by its holder or by
  `unlock <id>`. Expiry would add no safety, because every write is already
  fenced by `If-Match`.
- **A held lock is advertised by a marker** at
  `[<root>/]_locks/<module path>/_<package>/<project>/<env>/<lock id>.json`.
  The marker is written before the lock is acquired and deleted after it is
  released, so a held lock is never invisible to `locks()`.
- **Markers are only hints.** The lock itself lives in `state.json`, which is
  the authority.

### R2's write rate

- Writes to one tenant are paced to at least `minWriteInterval` apart. The
  default is one second on R2 and zero elsewhere.
- A 429 response means the write was not applied.
- Refresh results are written in one batch (`put_many`). Every provider change
  is still written as soon as it happens.

### Format versions

- **Every object carries `format.read`, `format.write` and `writtenBy`.**
  A reader refuses a newer `read` version; a writer refuses a newer `write`
  version.
- **Unknown fields are preserved,** so an older writer cannot drop a newer
  field.
- **No format number is ever raised as a side effect of an ordinary command.**
  The first release writes version 1 only. `state upgrade` ships with the
  first bump of either number and is the only way to apply it.
- **The format belongs to the object,** so an upgrade touches only the project
  that ran it.

### History and recovery

- **History snapshots.** When a run releases its lock after changing state, it
  writes a snapshot to `history/<serial>.json`: a create-only write, with the
  lock removed, retained to a configurable count. This is the restore point on
  R2, which has no versioning.
- **cuenv never deletes `state.json`.** Acquiring a lock refuses when
  `state.json` is missing but history exists or a migration manifest lists the
  tenant. In that case the state vanished, and starting again from empty would
  recreate every resource.
- **`state restore`** writes a snapshot back under the lock, with a new serial
  and new record generations. It is the only command that may recreate a
  missing `state.json`.

### Ownership and renames

- **No ownership gate.** A tenant is identified by its key (module path,
  package, project, environment), and project names are unique within a
  module, so a second instance cannot claim the same tenant. The ownership
  check and the requirement to run `state adopt` after moving a directory are
  removed, for every backend.
- **Writer instance, information only.** Lock acquisition by an evaluated
  command records the writer's instance (`<directory>:<package>`) as
  `lastWriter`. Plan shows it and notes when it differs from the current
  directory, and rename detection uses it. It never blocks a run.
- **Skipped directories.** State commands refuse a target inside a directory
  the module walk skips (`_x`, `.x`, `testdata`). Today only the ownership
  gate stopped `state remove` run in a stale copy such as `_old/web` from
  deleting live records.
- **Rename detection.** A run that finds its own tenant empty looks for a
  tenant in the same module whose last writer was in its directory, that is
  no longer declared, and that still holds records or a lock. If it finds one, it refuses as
  a probable rename and offers three ways forward:
  - rename the state;
  - retire the old identity;
  - continue with `--new-identity`. That flag skips only this check.
- **`state rename`** moves a tenant to a new key and leaves a tombstone behind.

### Migration from SQL

- **`infrastructure.migrateStateTo` names the target** while `state` still
  names the SQL source.
- **`state migrate` works one tenant at a time,** or `--all` from the source's
  records. It fences the source with a lock that is never released and that
  new builds' SQL schema migrations ignore. It then copies, reads back,
  compares, and commits, and records progress in a `_migration.json` manifest.
- **Plan and apply never see an empty target mid-migration.** They refuse a
  tenant the manifest lists as not yet migrated.
- **Migrating a source whose schema is older than the build's** requires
  `--upgrade-source`, because the upgrade affects every project on that
  database.

### Project names

Project names are unique. A project is a `#Project` instance in the package
cuenv evaluates. Instances in other CUE packages are not projects. A duplicate
name, compared case-insensitively, is an error in every command that
evaluates the whole module (`plan`, `apply`, `destroy`, `import`,
`cuenv sync`, `cuenv ci`). The shell hook path never runs the check.

The SQL backends keep their existing cross-package check unchanged, because
their key has no package.

The module path is the identity. Keeping it unique, for example by changing
`module:` when a repository is copied, is the user's responsibility.

### Transport

- **reqwest with the shared `state/http.rs` rules,** and SigV4 header signing
  written on the existing `hmac` and `sha2` crates.
- **No presigned URLs, and no ambient AWS credential chain.** Credentials come
  only from CUE, through `#InfrastructureStateToken`.
- **A capability probe** confirms the store honours conditional writes,
  returns strong ETags, and does not serve the bucket to unsigned requests. It
  needs no bucket-level permissions.

## Consequences

### Positive

- Upgrading cuenv in one project can no longer break another project's state.
- Moving a project's directory keeps its state.
- The lock-fencing guarantee is the same as SQL's, on every S3-compatible
  store.
- One implementation covers AWS S3, R2 and MinIO.
- The key layout allows prefix-scoped credentials per module.

### Negative

- Every write rewrites the tenant object. A size cap (16 MiB) stops an apply
  before any provider call when an object nears it.
- On R2, a tenant records at most one change per second.
- Anyone who can read the bucket can read every project's plaintext provider
  secrets, and anyone who can write it has apply authority over every project.
  The documentation recommends one bucket per trust boundary and ships
  least-privilege recipes.
- History snapshots keep rotated secrets until they are pruned. `destroy`
  leaves its snapshots behind.
- cuenv builds older than this release do not honour the migration fence.
  Pinned machines must upgrade before a project migrates.
- Renaming a module path cannot be detected. Renaming a project and moving
  its directory in the same change cannot be detected either.
- A copied repository that keeps the original module path, used with the same
  bucket credential, shares the original's state. The plan shows the state
  location and every destroy before anything runs. Nothing else guards
  against it.
- Modules that reuse a project name fail until one is renamed.
- Two nested modules that declare the same module path share state, and
  nothing detects it.
- Builds from 0.61 and earlier still enforce the old gate against the SQL
  owner row, which new builds overwrite with the last writer. A pinned older
  build may therefore refuse with `OwnedByAnotherInstance` and suggest
  `state adopt`. The release note says so.

### Neutral

- Turso stays supported. D1 is deprecated by #549.
- `state adopt` is no longer required. It is kept for one release as a no-op
  that prints a deprecation notice, then removed. `state-adopt` stays an
  accepted, ignored value of `#InfrastructureAction` permanently, so existing
  configurations keep evaluating.
- The SQL schema is unchanged, apart from preserving migration fence rows.
  The SQL owner table now records the last writer.

## Validation

- A generic `StateStore` conformance suite runs against every backend,
  including a stateful in-process S3 stand-in. The stand-in injects 409, 412,
  429, missing objects and dropped responses.
- Specific tests cover:
  - fencing after a forced unlock;
  - format preservation across versions;
  - rename detection (adding an environment, a second package in one
    directory, a destroyed environment, an actual rename);
  - the vanished-state guard;
  - restore;
  - fence survival across SQL migrations.
- An ignored end-to-end test runs the conformance suite against any
  S3-compatible server, such as MinIO. A verification record against real R2
  and AWS S3 is added to the spec before the backend leaves experimental
  status; the spec's Verification section says what has run so far.

Implementation spec:
`docs/design/specs/2026-10-29-infrastructure-object-storage-state.md`.
