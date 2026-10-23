# Cloudflare D1 as an Infrastructure State Backend

Status: **implemented**. `cuenv infrastructure` remains experimental. This
document records how the D1 backend is built, what was verified against D1,
and what was not.

## Goal

Let a project keep its infrastructure state in a Cloudflare D1 database
instead of Turso, with the same guarantees: lock-fenced writes, compare and
swap, ownership, fenced migrations, recovery files bound to the backend, and
nothing secret in errors.

```cue
infrastructure: state: d1: {
	accountId:  "0123456789abcdef0123456789abcdef"
	databaseId: "0b2c3d4e-5f60-4718-8293-a4b5c6d7e8f9"
	authenticationTokenEnvironmentVariable: "CLOUDFLARE_D1_API_TOKEN" // default
}
```

## Design

D1 and Turso are both SQLite reached over HTTP, so the backends share one
implementation instead of two copies of the locking logic.

- `state/sql.rs` holds everything that decides what is stored and how a write
  is fenced: the schema and its migrations, every statement, row decoding,
  retries, `acquire_lock`'s lost-response recovery, compare and swap and the
  migration fence. `SqlStateStore<T: SqlTransport>` implements the state store
  operations; `TursoStateStore` and `D1StateStore` are newtypes over it.
- `SqlTransport` is all a backend supplies: execute statements in one request,
  run statements as one transaction, the secret to redact, how a rejected
  statement is described, and a `Dialect` for the parameters it cannot bind as
  themselves.
- `state/turso.rs` is the Hrana transport (unchanged wire format;
  transactions are a `BEGIN IMMEDIATE` … `COMMIT` / `ROLLBACK` conditional
  batch). `state/d1.rs` is the Cloudflare REST transport (`POST
  /accounts/{account}/d1/database/{database}/raw`). `state/http.rs` holds the
  HTTP rules both follow: no redirects, bounded bodies, timeouts, transient
  classification, `Retry-After`.

### What D1's API requires

| D1 behaviour | Consequence |
| --- | --- |
| `params` is an array of strings | Integers are sent in decimal; INTEGER column affinity stores and compares them as numbers. Every bound integer is compared with, or stored into, a column — never compared with a literal, where a text value always sorts above a number. |
| No blob or null parameters | `Dialect` spells blob parameters `NULLIF(unhex(?), x'')` (hex text; empty is null) and optional text `NULLIF(?, '')` (identity JSON is never empty). `SqlValue` has no bindable null: only an empty `Blob` and an absent `OptionalText` stand for it, so a stray null cannot be sent. |
| `params` with several statements is refused (7400) | A request that binds anything holds one statement. Multi-statement requests (the migration transaction, the schema version check, the migration announcement) bind nothing; their integers are computed locally and inlined. |
| The statements of one request run as a transaction | A migration is one request: the fence, the DDL and the version bookkeeping commit together or not at all. D1 runs one request at a time per database, which gives the fence what `BEGIN IMMEDIATE` gives it on Turso. D1 has no `BEGIN`. |
| A rejected statement is HTTP 400, code 7500, message ending `: SQLITE_ERROR` | The body is parsed before the status is used. The lock fence's `integer overflow` and `already exists` reach the migration logic; D1 does not say which statement failed, so the fence is matched by message, and only in a migration that has a fence. |
| Numbers are JavaScript numbers | An integer above 2^53 (the fence returns `9223372036854775807` as `9223372036854776000` when no lock is held) is decoded as a float and never as an integer; no column cuenv reads comes near that. |
| Blobs come back as arrays of byte values | Decoded into bytes; anything else in such an array is unreadable, and the record is reported as undecodable by address. |
| `success: false` can come with HTTP 200 | Checked at the envelope and per result. |
| Retryable errors are named by message (Network connection lost, object reset, transient remote issue) | Retried with backoff whatever the HTTP status; every statement is safe to repeat. An overloaded database is not retried: Cloudflare asks for fewer requests. 429 honours `Retry-After` up to 30 seconds (for Turso too, since the HTTP rules are shared). |
| 2 MB per string, blob or row | Named in the error; the change is saved as an unrecorded file like any failed write. |

### Configuration

- `#InfrastructureState` is `turso?` or `d1?`, with a CUE `error()` check in
  `#Infrastructure` that exactly one is set, decided from field names so a
  backend with its own error (an invalid URL) reports only that. The manifest
  type is an enum with `deny_unknown_fields` on each variant.
- `accountId` must be 32 lowercase hexadecimal digits and `databaseId` a
  lowercase UUID, checked in CUE and in Rust before any secret is resolved. The
  identifiers go into the request path, so they are never anything else.
- The token is required. Its default variable is `CLOUDFLARE_D1_API_TOKEN`,
  not `CLOUDFLARE_API_TOKEN`: the state token's variable is withheld from every
  provider, and the Cloudflare Terraform provider reads `CLOUDFLARE_API_TOKEN`.
- Recovery files bind to a hash of the account and database IDs.
- Errors never name the account or database: transport errors drop the URL
  (`reqwest::Error::without_url`), and Cloudflare's own messages, which can
  quote the request path, have both identifiers replaced. A request that
  carried state never has the server's message quoted.

## Verification

- Probed on a real, throwaway D1 database through the REST query endpoint:
  `WITHOUT ROWID`, `sqlite_master`, `unhex`/`hex`, numbered parameters, the
  lock-fenced upsert (changes 0 on a foreign lock, 1 on insert and update),
  string parameters stored as integers, `NULLIF` nulls, blobs read as byte
  arrays, the fence's `integer overflow` error with code 7500, a failing
  multi-statement request rolling back entirely, and the refusal of parameters
  with several statements. `sqlite_version()` is not authorized (cuenv does not
  use it).
- `state/d1/tests.rs` runs the store against a stand-in for the `/raw`
  endpoint backed by real in-memory SQLite that binds text only, refuses
  parameters with several statements, runs a request as one transaction and
  answers errors as D1 does: round trips, lock fencing, compare and swap,
  ownership, the migration fence, the pending-migration hold-off, newer schemas,
  and error classification, retries and redaction against canned responses.
- The Turso behaviour tests run against the shared implementation; the tests
  of how a Hrana batch result is read were rewritten for the new seam
  (`transaction_result`, then `migration_outcome`).

Not verified against real D1: the `/raw` endpoint specifically (the probes
used the query endpoint, which takes the same body), including that a failing
multi-statement request rolls back there too (Cloudflare documents rollback for
`batch()`; the probe showed it for a multi-statement query; should it not hold,
a migration that fails part way leaves tables without a version row, and the
next run refuses with a schema conflict instead of writing), whether a D1 Read
token may call `/raw` for reads, and the ignored `lifecycle_against_d1` test, which
needs a database and token. REST queries were served by the primary in the
probes; cuenv does not use the Sessions API, so it does not read from
replicas.
