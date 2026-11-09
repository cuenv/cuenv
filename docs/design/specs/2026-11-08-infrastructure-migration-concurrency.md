# Infrastructure state migration concurrency

Status: **open design**. The current migration command implements the bounded
repairs described below and requires the write freeze in the [infrastructure
guide](../../src/content/docs/how-to/infrastructure.mdx#migrate-between-backends).
The project claims, durable phase journal and automatic recovery in the
candidate protocol are not implemented by the command.

## Problem

The S3 migration manifest is shared by a project's environments. Unconditional
manifest writes can lose entries, including when migrators select different
environments. Sampling a target's serial after releasing its migration lock can
record a later write as the baseline. Checking that baseline before acquiring
the rollback lock can also discard a concurrent target write.

The command now uses conditional manifest updates, records the target baseline
in the same write that releases the migration lock, and checks that baseline
while holding a protected rollback lock. Receipts bind the selected tenants
and attested physical stores. Exact copy-back validates the full snapshot and
keeps both stores protected on an unconfirmed mutation. An interrupted
multi-environment rollback or an uncertain lock release still requires manual
reconciliation. These repairs do not make operations across independent object
keys atomic or prove the operator's physical-store identities.

Migration and rollback must run with all
source and target writers, other migrators, restore and unlock operations, and
older clients stopped through the configuration cutover.

## Design constraints

- Coordinate at the source project as well as the destination. A destination
  claim alone does not prevent the same source environments being migrated to
  different targets. Claims need unique owner identities, conditional updates,
  non-repeating revisions, and owner-matching release; elapsed time alone
  must not permit takeover.
- Bind the journal to the actual source and destination using stable,
  non-secret identities, and to the package, tenant, migration and lock IDs.
  Never treat holder text as proof of ownership or put credentials and state
  values in the journal or diagnostic output.
- Preserve the exact target baseline atomically with the conditional write
  that releases the migration lock. A later read of the current serial is not
  a valid substitute. Rollback must lock an existing target and compare its
  pre-lock version, origin and committed marker while holding that lock.
  Serial exhaustion must fail closed or use a non-repeating version token.
- Record enough durable phases to resume a partially copied SQL source, an
  already-tombstoned target and an uncertain source unlock. Neither a missing
  target nor an absent legacy commit marker proves that rollback is safe.
- Resolve ambiguous object writes by reading back an operation identity;
  never blindly retry a write whose outcome is unknown. Manifest updates must
  preserve unrelated environments and unknown compatible fields.
- A manual recovery operation must identify the exact owner and require the
  operator to stop it and its in-flight requests before releasing a claim.
  Older clients do not honor a new claim, so upgrades and access restrictions
  must be enforced operationally before migration.

A source-project claim can serialize cooperating migrators, but on S3 it cannot
atomically prevent creation of another environment in a different object. An
external write freeze remains necessary unless ordinary source writes also
participate in a common, enforceable fence. Enumeration and a second scan do
not themselves prove that no environment appeared after the scan.

## Candidate protocol for review

Migration and rollback require an explicit maintenance-mode acknowledgement.
It means that the operator has externally stopped every writer, migrator,
reader, restore or unlock operation and its in-flight requests, including old clients,
through configuration cutover. The acknowledgement does not itself enforce a
freeze. Recovery files on every host must be reconciled immediately before
each forward migration, rollback and resume. The command checks the
acknowledgement before resolving secrets, schema upgrades, target setup, or any
other side effect. A rollback or resume requires the same freeze.

1. Acquire a source-project claim, keyed by the physical source identity,
   module and project, and then a destination-project claim. SQL identity
   excludes CUE package; an S3 identity includes its bucket, root and package.
   The claim records a unique operation identifier and a separate owner nonce.
   Acquisition is conditional, release compares the owner nonce, and neither
   elapsed time nor a displayed holder grants ownership. The source and target
   claims must be different physical objects. SQL uses a dedicated
   collision-safe table; S3 uses a project metadata object whose conditional
   updates never delete it. Claim keys live in the backend's own namespace,
   so two endpoint aliases of the same physical store cannot create different
   claims. A source claim records the destination identity and journal locator
   before acquiring the target claim. A busy target releases an untouched
   source claim only after the target outcome is positively known.
2. Record source and destination identities, selected tenants, claim owner,
   source fence identifiers, target lock identifiers, and durable phases in a
   conditional journal before the corresponding irreversible effects. Unknown
   fields and unrelated environments survive every manifest update. Resolve
   ambiguous writes by positively reading back their operation identity;
   an absent or stale read does not prove failure.
3. Fence the source tenant and import it under the journaled target lock.
   Commit the verified target by writing a migration marker, baseline writer
   nonce, and lock release in the same conditional target-document update.
   The baseline nonce equals that document's post-commit writer nonce in the
   same CAS. Store the marker where ordinary compatible writers preserve it.
   The nonce changes on every write, including when a serial has reached its
   limit.
4. To roll back, conditionally lock an existing target and retain its exact
   pre-lock version in the same conditional update that takes the lock. While
   holding that lock, compare its origin and durable
   commit marker with the journal, and compare the pre-lock writer nonce to
   the baseline. Refuse a changed target unless copy-back was selected.
   Copy the locked target snapshot to the fenced source, verify the complete
   source, then write an operation-marked target tombstone before releasing
   the source fence. Keep enough journaled phases to repeat a partial copy,
   recognize the matching tombstone, and resolve an uncertain unlock without
   treating absence as proof of success.
5. A failed or interrupted run retains its claims and any fences whose
   outcome is uncertain. An explicit resume names the operation and owner;
   the operator must first stop the previous owner and its in-flight requests.
   Recovery also revokes the previous operation's tenant lock tokens before
   performing any further effect.
   A missing target, foreign lock, missing marker, or legacy manifest whose
   baseline was sampled after unlock requires manual reconciliation. Restore
   must preserve the marker or refuse to restore a migration-bound state.

The SQL schema upgrade must drain ordinary locks transactionally, preserve only
explicitly inventoried legacy migration fences, and invalidate already-started
old SQL write statements. New lock acquisition, writes, restore, rename, force-unlock
and schema migration check the project claim atomically. A migration operation
can write only under its own journaled fence token and matching claim owner;
ordinary writers cannot use that exemption. Every D1 statement with bound
parameters remains a single statement.

The journal records these phases before advancing to the next side effect:

| Phase | Durable evidence | Permitted next effect |
| --- | --- | --- |
| Claiming | Source claim names operation, owner, destination and journal | Acquire the matching target claim |
| Prepared | Both claims and the selected tenant list are confirmed | Take the journaled source fence |
| Source fenced | Exact source fence token is held | Import under the journaled target lock |
| Target locked | Imported records and the target lock are verified | Commit marker and unlock in one target CAS |
| Committed | Target marker contains the commit writer nonce | Merge the environment's migrated entry into the manifest |
| Forward recorded | Manifest records this tenant and commit marker | Record the next tenant or finalize the forward operation |
| Forward complete | Every selected tenant is recorded and remains fenced | Release project claims; keep the external freeze through cutover |
| Rollback locked | Target lock contains its exact pre-lock writer and marker | Refuse changes or copy a locked snapshot back |
| Source verified | Fenced source matches the locked target | Write the matching operation tombstone |
| Tombstoned | Target tombstone identifies this operation | Release the source fence |
| Source released | Source fence release was positively confirmed | Release the source claim by owner |
| Final release planned | Source claim release is recorded and target release intent is durable | Release the target claim by owner |
| Complete | Both exact claim-release receipts are confirmed | Finish the operation |

Before each side effect, an append-only intent records the tenant, expected
claim and fence revisions, the caller-selected replacement lock token or
write nonce, and the operation it authorizes. The following phase records
only a positively confirmed result. In particular, fence acquisition,
target import, commit, rollback lock, copy-back writes, tombstone, claim
transfer and release must not generate their tokens or nonces after issuing
the corresponding request. A resume checks the exact intent against the live
object, receipt and journal before deciding whether to acknowledge or retry;
missing or overwritten evidence is indeterminate. It never stores provider
state or hashes of secret-derived values in the journal.

Release the source claim first while the target claim remains active, and
append its confirmed receipt and the target-release intent to the journal.
Release the target claim last. Because a journal append requires the target
claim, completion after its release is established from the journaled intent
and the target claim's exact durable release receipt; it does not require a
post-release journal write. Recovery that needs another journal write first
reacquires or transfers the required claims under a fresh owner.

Resume and rollback use the durable tenant list, not a fresh source listing.
Forward completion marks each environment migrated, releases the project
claims, and retains each source tenant fence. A crash between commit, manifest
recording, claim release and cutover resumes from the target marker and
per-tenant journal; it never releases a source fence. The operator keeps the
external freeze in effect until configuration cutover is complete, even after
project claims are released. A later rollback is a new
operation and owner bound to the original migration identifier and marker;
its journal records idempotent per-tenant phases independently of the forward
operation. Rollback completion requires the matching target tombstone and a
positively confirmed source-fence release before project claims are released.
For recovery after cutover, the operator restores a temporary configuration
that identifies the original source and `migrateStateTo` target, module,
package and project, supplies credentials through the existing secret
mechanisms, and passes `--resume <operation-id>` alongside the explicit
maintenance acknowledgement. The command reports the operation identifier
when it leaves recoverable work. Resolved backend identities and journal
ownership must match before any state change. A removed environment is
selected from the journal rather than the current CUE declarations. If the
original identity or backends cannot be reconstructed, recovery fails closed
and requires manual reconciliation. Recovery output identifies the operation
and the next safe action, but never credentials, endpoint URLs or state values.
A read-only claim-inspection command also reports the operation identifier when
a process died before it could print it. If the process died after a project
claim but before the tenant journal was durable, automatic resume stops for
manual reconciliation rather than re-enumerating a possibly changed source.
Resume locates exactly one source claim and target journal by operation
identifier, loads any recorded owner nonce privately, and checks both claims
against the journal before choosing a fresh owner. An active claim must have
the journaled owner and revision. An inactive claim is acceptable only after a
durable phase permits its release and its retained receipt exactly matches the
journaled owner and release revisions. This includes crashes immediately
before, between and after the two claim releases. Recovery acquires each
inactive claim for the same operation under a fresh owner and revision, with
the exact journaled source fences authorized; it transfers an active claim
from the recorded owner. It then rotates every retained fence before making
changes. If a later operation has overwritten a required receipt, a claim is
missing or foreign, or any evidence is ambiguous, recovery stops for manual
reconciliation. A stale owner can never release the fresh owner's claim.
Inspection and diagnostics never print owner or tenant lock tokens.
Before taking over, recovery journals fresh lock tokens, then atomically
transfers each source fence and target lock from the exact old token to the
new token. It reads back the new token after an ambiguous result and never
uses a release-then-acquire gap. A matching tombstone must be readable without
ordinary live-state access. If a source unlock outcome is uncertain and no
atomic operation-bound release receipt exists, automatic recovery stops for
manual reconciliation.

The target commit upgrades its write format so clients that do not preserve
the marker and change the writer nonce cannot write that state. Access by old
writers, old restorers and actors able to delete and recreate migrated target
objects must remain externally restricted while rollback remains available;
version-gating an object cannot protect it after it has vanished. The source
verification phase is journaled before tombstoning without storing hashes of
secret-derived values as offline guessing oracles. A migration tombstone keeps
the locked target records until source unlock is positively resolved, so an
interrupted recovery can compare the actual records again. Copy-back preserves
the target's resource generations, serials, provider state, private bytes,
sensitivity metadata and last-writer metadata, not only `same_content`. An
unrepresentable or exhausted version fails closed rather than being rounded or
reused.
Claims coordinate trusted
writers; they do not authorize against another principal with bucket or
database access.

This is a proposed protocol, not an implemented guarantee. Its recovery
interface, SQL upgrade, backend alias detection, and old-client behavior need
review and tests before implementation. A generic S3 endpoint cannot prove
whether two differently named endpoints address the same physical bucket;
unknown aliases require a trusted identity or conservative refusal. The first
implementation refuses equal source and destination physical identities,
including different prefixes of the same bucket. This restriction avoids
assuming that endpoint-visible prefixes describe physical keys when a gateway
may rewrite them. An operator-supplied identity must be non-secret, stable
across all endpoint aliases, and scoped to the actual bucket or database; the
stored journal binds both identities and the configured namespaces. Distinct
identities are an explicit operator attestation that the physical stores are
different, because generic S3 cannot authenticate this claim. Missing, nil or
inconsistent identities refuse migration, resume or rollback. A future
same-bucket implementation must compare every project, tenant, lock-marker and
journal range using trusted canonical physical-key mappings.

For D1, claim acquisition and claim-guarded writes each use one parameterized
statement. A bound JSON fence list may be inspected with `json_each` after real
D1 and SQLite-backed tests verify the required functions. Schema version 2
remains the production schema until a complete version-3 store and an explicit
database-wide maintenance bootstrap are reviewed together. The bootstrap
requires every client and in-flight request on the database to be stopped, and
an operator-reviewed exact inventory of any retained legacy fences; a token's
`migrated-` prefix alone does not establish its provenance. It stages a
non-overwritable intent with a fresh operation and random inventory revision.
The protected database retains the exact inventory; no token-derived digest is
published as a guessing oracle.

The D1 upgrade transaction is unbound and compares the complete stored intent
against every current lock in both directions. It rejects unapproved or changed
locks, preserves approved legacy rows as explicitly marked legacy fences,
renames the old lock identifier column so already-started legacy write
statements fail, records version 3 and an operation-bound completion receipt,
and consumes the intent atomically. A matching receipt is accepted only with
the version-3 schema; ambiguous or conflicting evidence stops for manual
reconciliation. Ordinary version-3 writes and unlocks must refuse marked
legacy fences, while unrelated projects remain usable. A legacy rollback is
not safe merely because a fence was preserved: it requires verified target
snapshot, copy and recovery evidence before any target tombstone or source
release. Older clients remain stopped because their rollback can change the
target before its old-column source unlock fails. No such bootstrap or legacy
reconciliation is currently exposed by the production command.
Migration-only writes use an explicit claim-owner-plus-fence capability in the
same statement. Each migration fence records the claim-owner nonce that owns
it; a write requires that nonce to match the active claim. Fence takeover
atomically changes both the lock token and its bound owner, preventing either
owner from writing in the interval between claim and fence transfer. The
ordinary state-write API refuses an active project claim.
The object store must provide strong per-key conditional writes and
read-after-write consistency; the existing sequential probe does not prove
concurrent compare-and-swap correctness.

## Verification needed

Exercise the protocol with deterministic request barriers around target-lock
acquisition and the target commit/manifest update boundary. Cover concurrent
same-tenant, different-environment and different-target migrations; writes
racing `--all` enumeration; lost responses and stale ETags; partial SQL
copy-back; each tombstone and unlock crash point; missing or foreign state;
legacy documents; serial exhaustion; and recovery after a stopped owner.
Verify that restore and ordinary state writes preserve or safely reject any
required provenance marker. Include real older-client compatibility tests and
the full Nix flake gate before claiming the protocol is safe.
