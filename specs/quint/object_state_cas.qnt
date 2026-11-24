// -*- mode: Bluespec; -*-
//
// The compare-and-swap protocol of the S3 state store
// (crates/infrastructure/src/state/object/store/object_state_store.rs) and
// the CLI's release retry (crates/cuenv/src/commands/infrastructure/lock.rs),
// as the code implements them, not as the design spec describes them.
//
// One tenant, one record address. `state.json` is a single object written
// only with conditional PUTs (`If-Match: <etag>` / `If-None-Match: *`).
//
// Abstractions:
// - The ETag is modelled by `ver`. Every write bumps `serial` and sets a
//   fresh `writer`, so bodies never repeat and an ETag never recurs
//   (spec: "no ABA, even where the ETag is a content MD5"). Taken as given.
// - The network is adversarial: a PUT may be processed at any later time,
//   or never; a response may be lost after the store committed; the client
//   may time out at any point (`S3Failure::Ambiguous`). A 412 reaches the
//   client only while it still waits for that request.
// - After a timeout the store reads the document back. An acquire, record
//   write or release counts as done only if the stored `writer` is the nonce
//   of the PUT it sent; otherwise the call fails ("could not be confirmed")
//   rather than retrying blind. A create resends the same body (same
//   writer) while the object is still absent.
// - Each client runs acquire_lock -> list -> put_if_unchanged -> unlock.
//   If the record write errored, the engine saved an unrecorded-change file
//   (engine.rs `record_change`); the client then runs again and replays the
//   same ConditionalPut (same expected version, same generation).
// - unlock() is retried by the CLI (`release`, RELEASE_ATTEMPTS). A retry
//   that finds the lock gone reports Uncertain: the failed attempt's PUT may
//   still land.
// - An operator may `force_unlock` whichever lock is held, once.
// - MAX_ATTEMPTS stands in for MUTATION_ATTEMPTS (8 in the code) and
//   CLI_ATTEMPTS for RELEASE_ATTEMPTS (3). 3 and 2 are the smallest values
//   that reach every branch of the retry loops.
//
// Check (TLC explores every reachable state of this instance):
//   quint verify object_state_cas.qnt --backend=tlc --tlc-config=tlc.json --invariants safety unlockReportsTruth
//
// Results, 2 clients, 1 record: safety and unlockReportsTruth hold; ~1.1M
// distinct states, under a minute. Released, Uncertain and a replayed write
// that lands are all reachable, so the invariants are not vacuous.
//
// Mutation switches (constants below); each should make an invariant fail:
// - RELEASE = "trusting" (the CLI before Release::Uncertain: a retry that
//   finds the lock gone reports "no longer held"): unlockReportsTruth fails.
//   The first attempt's release landed after the store gave up on it.
// - READBACK_WRITER = false (confirm a timed-out release by `lastReleased`
//   and an empty lock alone): unlockReportsTruth fails. An operator's
//   force-unlock of this lock looks the same as this run's own release.
// - errorMeansNotWritten: fails by design (see below). Not checked in CI.
// Mutants caught by `safety`: no lock check before a record write; a record
// PUT without If-Match. Not caught: dropping the serial clause from
// is_recorded (each client writes once here, so gen + content suffice).
//
// This is model checking of a finite instance, not a proof for all sizes.
module object_state_cas {

  // ---- parameters ---------------------------------------------------------

  pure val CLIENTS = Set(1, 2)
  pure val MAX_ATTEMPTS = 3
  pure val CLI_ATTEMPTS = 2   // unlock() calls by the CLI's release()
  pure val MAX_ROUNDS = 2     // first run + one replay of an unrecorded change
  pure val MAX_TRIES = 1      // acquire attempts (a `Locked` run gives up)
  pure val ABSENT = -1        // the condition `If-None-Match: *`
  pure val OPERATOR = -1

  // Mutation switches. The first value is the code as it is.
  // What the CLI reports when a retried unlock() finds the lock gone:
  // "checked": Uncertain. "trusting": NoLongerHeld.
  pure val RELEASE = "checked"
  // Whether a timed-out release is confirmed by its own writer nonce.
  pure val READBACK_WRITER = true


  // ---- types --------------------------------------------------------------

  // `state.json`. `gen == 0` means the record is absent.
  type Doc = {
    exists: bool, ver: int, lock: int, lastReleased: int, writer: int,
    gen: int, serial: int, content: int,
  }

  // A conditional PUT on the wire.
  type Req = { client: int, round: int, holder: int, nonce: int, kind: str, cond: int, body: Doc }

  type Client = {
    pc: str, attempt: int, cliTry: int, waiting: int, resp: str,
    round: int, tries: int, replay: bool,
    expGen: int, expSerial: int, payGen: int,
    putOut: str, relOut: str,
    relLock: int,           // the lock in the document release() returned
    // ghost: what the store committed on this client's behalf
    puts: int, putFromExpected: bool, putRound: int, relRound: int,
  }

  pure val NO_DOC: Doc = {
    exists: false, ver: 0, lock: 0, lastReleased: 0, writer: 0,
    gen: 0, serial: 0, content: 0,
  }

  pure val FRESH: Client = {
    pc: "idle", attempt: 0, cliTry: 1, waiting: 0, resp: "none",
    round: 1, tries: 0, replay: false,
    expGen: 0, expSerial: 0, payGen: 0,
    putOut: "", relOut: "",
    relLock: 0,
    puts: 0, putFromExpected: false, putRound: 0, relRound: 0,
  }

  // StateLock::generate(): a fresh identifier per lock attempt.
  pure def lockOf(c: int, s: Client): int = c * 100 + s.round * 10 + s.tries
  // The record content client c writes (distinct per client).
  pure def contentOf(c: int): int = c * 1000
  // write_generation(Absent): a fresh uuid when the engine builds the record.
  pure def insertionOf(c: int, s: Client): int = c * 10000 + s.round
  // A fresh `writer` nonce per send (unique per client, round, phase, CLI
  // attempt and store attempt). A create's resend keeps its nonce.
  pure def nonceOf(c: int, s: Client, phase: int): int =
    c * 100000 + s.round * 10000 + s.tries * 1000 + phase * 100 + s.cliTry * 10 + s.attempt

  // ConditionalPut::is_recorded.
  pure def isRecorded(c: int, s: Client, d: Doc): bool = and {
    s.payGen != 0,
    d.gen == s.payGen,
    d.content == contentOf(c),
    if (s.expGen == 0) d.serial == 1
    else and { d.gen == s.expGen, d.serial == s.expSerial + 1 },
  }

  // A request that can no longer match (the ETag moved on, or the object
  // exists for an `If-None-Match: *`) and that no client waits for has no
  // further effect. Dropping it keeps the state space finite and small.
  pure def live(r: Req, d: Doc, cs: int -> Client): bool = or {
    if (r.cond == ABSENT) not(d.exists) else d.ver == r.cond,
    cs.get(r.client).waiting == r.nonce,
  }
  pure def gc(rs: Set[Req], d: Doc, cs: int -> Client): Set[Req] = rs.filter(r => live(r, d, cs))

  // ---- state --------------------------------------------------------------

  var doc: Doc
  var inflight: Set[Req]
  var clients: int -> Client
  var forcedLock: int     // the lock an operator force-unlocked; 0 before
  // ghost
  var fenceOk: bool       // every committed record write replaced a version holding its lock
  var lastPut: int        // content of the last committed record write
  var writers: Set[int]   // clients with a committed record write

  action init = all {
    doc' = NO_DOC,
    inflight' = Set(),
    clients' = CLIENTS.mapBy(_ => FRESH),
    forcedLock' = 0,
    fenceOk' = true,
    lastPut' = 0,
    writers' = Set(),
  }

  // ---- client helpers -----------------------------------------------------

  action local(c: int, s: Client): bool = all {
    clients' = clients.set(c, s),
    doc' = doc, inflight' = gc(inflight, doc, clients.set(c, s)), forcedLock' = forcedLock,
    fenceOk' = fenceOk, lastPut' = lastPut, writers' = writers,
  }

  // Send a conditional PUT with a fresh `writer` nonce, then wait.
  action send(c: int, s: Client, phase: int, kind: str, cond: int, body: Doc, waitPc: str): bool = {
    val nonce = nonceOf(c, s, phase)
    all {
      inflight' = inflight.union(Set({
        client: c, round: s.round, holder: lockOf(c, s), nonce: nonce,
        kind: kind, cond: cond, body: { ...body, writer: nonce },
      })),
      clients' = clients.set(c, { ...s, pc: waitPc, waiting: nonce, resp: "none" }),
      doc' = doc, forcedLock' = forcedLock,
      fenceOk' = fenceOk, lastPut' = lastPut, writers' = writers,
    }
  }

  pure def acquired(s: Client): Client =
    { ...s, pc: if (s.replay) "put_read" else "list", attempt: 0, waiting: 0, resp: "none" }

  pure def putDone(s: Client, out: str): Client =
    { ...s, pc: "rel_read", attempt: 0, waiting: 0, resp: "none", putOut: out }

  pure def relDone(s: Client, out: str, lock: int): Client =
    { ...s, pc: "released", waiting: 0, resp: "none", relOut: out, relLock: lock }

  // unlock() returned Ok(false): the CLI reports NoLongerHeld on its first
  // call, Uncertain on a retry.
  pure def relNone(s: Client): Client =
    relDone(s, if (s.cliTry > 1 and RELEASE == "checked") "uncertain" else "notHeld", 0)

  // unlock() failed: the CLI retries, or gives up after its last attempt.
  pure def relFailed(s: Client): Client =
    if (s.cliTry >= CLI_ATTEMPTS) relDone(s, "error", 0)
    else { ...s, pc: "rel_read", cliTry: s.cliTry + 1, attempt: 0, waiting: 0, resp: "none" }

  pure def retry(s: Client, readPc: str): Client =
    { ...s, pc: readPc, attempt: s.attempt + 1, waiting: 0, resp: "none" }

  pure def timedOut(s: Client, readbackPc: str): Client =
    { ...s, pc: readbackPc, waiting: 0, resp: "none" }

  // ---- one client step ----------------------------------------------------

  action clientStep(c: int): bool = {
    val s = clients.get(c)
    val me = lockOf(c, s)
    any {
      all { s.pc == "idle", local(c, { ...s, pc: "acq_read", attempt: 0 }) },

      // acquire_in_document
      all { s.pc == "acq_read", s.attempt >= MAX_ATTEMPTS, local(c, { ...s, pc: "done" }) },
      all { s.pc == "acq_read", s.attempt < MAX_ATTEMPTS,
        if (not(doc.exists))
          send(c, s, 1, "create", ABSENT, { ...NO_DOC, exists: true, ver: 1, lock: me }, "create_wait")
        else if (doc.lock == me)
          local(c, acquired(s))
        else if (doc.lock != 0)
          local(c, if (s.tries + 1 < MAX_TRIES) { ...s, pc: "idle", tries: s.tries + 1 }
                   else { ...s, pc: "done" })
        else
          send(c, s, 1, "acquire", doc.ver, { ...doc, ver: doc.ver + 1, lock: me }, "acq_wait")
      },
      // create(): Ok(true) on success; on 412, Ok(writer is ours) and the
      // acquire loop reads again.
      all { s.pc == "create_wait", s.resp == "ok", local(c, acquired(s)) },
      all { s.pc == "create_wait", s.resp == "412",
        local(c, if (doc.exists and doc.writer == nonceOf(c, s, 1)) acquired(s) else retry(s, "acq_read")) },
      all { s.pc == "create_wait", local(c, timedOut(s, "create_readback")) },
      // A lost create response: decided by the writer if the object exists,
      // else the same PUT is sent again.
      all { s.pc == "create_readback", doc.exists,
        local(c, if (doc.writer == nonceOf(c, s, 1)) acquired(s) else retry(s, "acq_read")) },
      all { s.pc == "create_readback", not(doc.exists),
        send(c, s, 1, "create", ABSENT, { ...NO_DOC, exists: true, ver: 1, lock: me }, "create_wait") },
      all { s.pc == "acq_wait", s.resp == "ok", local(c, acquired(s)) },
      all { s.pc == "acq_wait", s.resp == "412", local(c, retry(s, "acq_read")) },
      all { s.pc == "acq_wait", local(c, timedOut(s, "acq_readback")) },
      // Confirmed by its own writer and lock, or the acquisition fails.
      all { s.pc == "acq_readback",
        local(c, if (doc.exists and doc.writer == nonceOf(c, s, 1) and doc.lock == me) acquired(s)
                 else { ...s, pc: "done" }) },

      // StateStore::list, then the engine builds the ConditionalPut
      all { s.pc == "list",
        local(c, { ...s, pc: "put_read", attempt: 0,
                   expGen: doc.gen, expSerial: doc.serial,
                   payGen: if (doc.gen != 0) doc.gen else insertionOf(c, s) }) },

      // mutate(put_if_unchanged)
      all { s.pc == "put_read", s.attempt >= MAX_ATTEMPTS, local(c, putDone(s, "exhausted")) },
      all { s.pc == "put_read", s.attempt < MAX_ATTEMPTS,
        if (not(doc.exists) or doc.lock != me)
          local(c, putDone(s, "lockLost"))
        else if (doc.gen != s.expGen or doc.serial != s.expSerial)
          local(c, putDone(s, if (isRecorded(c, s, doc)) "ok" else "stateChanged"))
        else
          send(c, s, 2, "put", doc.ver, {
            ...doc, ver: doc.ver + 1,
            gen: if (doc.gen == 0) s.payGen else doc.gen,
            serial: doc.serial + 1,
            content: contentOf(c),
          }, "put_wait")
      },
      all { s.pc == "put_wait", s.resp == "ok", local(c, putDone(s, "ok")) },
      all { s.pc == "put_wait", s.resp == "412", local(c, retry(s, "put_read")) },
      all { s.pc == "put_wait", local(c, timedOut(s, "put_readback")) },
      all { s.pc == "put_readback",
        local(c, if (doc.exists and doc.writer == nonceOf(c, s, 2)) putDone(s, "ok")
                 else putDone(s, "unconfirmed")) },

      // unlock() -> release(own lock), inside the CLI's retry loop.
      all { s.pc == "rel_read", s.attempt >= MAX_ATTEMPTS, local(c, relFailed(s)) },
      all { s.pc == "rel_read", s.attempt < MAX_ATTEMPTS,
        if (not(doc.exists) or doc.lock != me)
          local(c, relNone(s))
        else
          send(c, s, 3, "release", doc.ver, { ...doc, ver: doc.ver + 1, lock: 0, lastReleased: me }, "rel_wait")
      },
      all { s.pc == "rel_wait", s.resp == "ok", local(c, relDone(s, "released", 0)) },
      all { s.pc == "rel_wait", s.resp == "412", local(c, retry(s, "rel_read")) },
      all { s.pc == "rel_wait", local(c, timedOut(s, "rel_readback")) },
      // Confirmed by its own writer, or unlock() fails.
      all { s.pc == "rel_readback",
        local(c, if (and { doc.exists, doc.lastReleased == me, doc.lock == 0,
                           doc.writer == nonceOf(c, s, 3) or not(READBACK_WRITER) })
                   relDone(s, "released", 0)
                 else relFailed(s)) },

      // After unlock: replay an unrecorded change in a new run, or stop.
      all { s.pc == "released",
        local(c,
          if (s.putOut != "ok" and s.round < MAX_ROUNDS)
            { ...FRESH, round: s.round + 1, replay: true,
              expGen: s.expGen, expSerial: s.expSerial, payGen: s.payGen,
              puts: s.puts, putFromExpected: s.putFromExpected,
              putRound: s.putRound, relRound: s.relRound }
          else { ...s, pc: "done" }) },
    }
  }

  // ---- the object store and the network -----------------------------------

  action deliverOne(r: Req): bool = {
    val matches = if (r.cond == ABSENT) not(doc.exists) else (doc.exists and doc.ver == r.cond)
    val s = clients.get(r.client)
    val isPut = matches and r.kind == "put"
    val isRelease = matches and r.kind == "release"
    val s1 = if (s.waiting == r.nonce) { ...s, resp: if (matches) "ok" else "412" } else s
    val s2 = if (isPut) { ...s1, puts: s1.puts + 1, putRound: r.round,
                          putFromExpected: s1.putFromExpected or
                            (doc.gen == s1.expGen and doc.serial == s1.expSerial) }
             else if (isRelease) { ...s1, relRound: r.round }
             else s1
    all {
      doc' = if (matches) r.body else doc,
      clients' = clients.set(r.client, s2),
      inflight' = gc(inflight.exclude(Set(r)), if (matches) r.body else doc, clients.set(r.client, s2)),
      forcedLock' = forcedLock,
      fenceOk' = (fenceOk and (not(isPut) or doc.lock == r.holder)),
      lastPut' = if (isPut) r.body.content else lastPut,
      writers' = if (isPut) writers.union(Set(r.client)) else writers,
    }
  }

  // A request that is never processed needs no action of its own: it stays
  // in flight until it can no longer match, and is then garbage-collected.

  // `state unlock --force <id>` on whatever lock is held, once.
  action forceUnlock = all {
    forcedLock == 0,
    doc.exists,
    doc.lock != 0,
    doc' = { ...doc, ver: doc.ver + 1, lock: 0, lastReleased: doc.lock, writer: OPERATOR },
    forcedLock' = doc.lock,
    inflight' = gc(inflight, { ...doc, ver: doc.ver + 1, lock: 0, lastReleased: doc.lock, writer: OPERATOR }, clients),
    clients' = clients,
    fenceOk' = fenceOk, lastPut' = lastPut, writers' = writers,
  }

  action quiescent = all {
    CLIENTS.forall(c => clients.get(c).pc == "done"),
    doc' = doc, inflight' = inflight, clients' = clients, forcedLock' = forcedLock,
    fenceOk' = fenceOk, lastPut' = lastPut, writers' = writers,
  }

  action step = any {
    nondet c = CLIENTS.oneOf()
    clientStep(c),
    all { inflight != Set(), nondet r = inflight.oneOf() deliverOne(r) },
    forceUnlock,
    quiescent,
  }

  // ---- properties ---------------------------------------------------------

  // Fencing: a record write lands only on a version that holds the
  // writer's own lock.
  val fencing = fenceOk

  // No lost update: the stored record is exactly the last committed write.
  val noLostUpdate = doc.content == lastPut

  // Ok means written: put_if_unchanged returns Ok only if a write by this
  // client replaced exactly the expected version.
  val okMeansWritten = CLIENTS.forall(c => {
    val s = clients.get(c)
    s.putOut == "ok" implies s.putFromExpected
  })

  // StateChanged means a real conflict: another client wrote the record.
  val conflictIsReal = CLIENTS.forall(c =>
    clients.get(c).putOut == "stateChanged" implies writers.exclude(Set(c)) != Set())

  // Exactly once: one logical write per client lands at most once across
  // retries, delayed requests and the replay.
  val atMostOnce = CLIENTS.forall(c => clients.get(c).puts <= 1)

  val safety = and { fencing, noLostUpdate, okMeansWritten, conflictIsReal, atMostOnce }

  // The CLI reports the release truthfully:
  // - "notHeld" (NoLongerHeld) only if this run's own release never
  //   committed. FAILS with RELEASE = "trusting".
  // - "released" only if this run's own release committed: not an
  //   operator's force-unlock of the same lock. FAILS with
  //   READBACK_WRITER = false.
  // - the document it returns, which unlock() snapshots into history, holds
  //   no lock.
  // "uncertain" and "error" claim nothing.
  val unlockReportsTruth = CLIENTS.forall(c => {
    val s = clients.get(c)
    and {
      s.relOut == "notHeld" implies s.relRound != s.round,
      s.relOut == "released" implies s.relRound == s.round,
      s.relOut == "released" implies s.relLock == 0,
    }
  })

  // Characterisation, expected to FAIL: an error from put_if_unchanged does
  // not mean "not written". This is why unrecorded-change files exist, and
  // why the replay must recognise its own earlier write (is_recorded).
  val errorMeansNotWritten = CLIENTS.forall(c => {
    val s = clients.get(c)
    (s.putOut == "lockLost" or s.putOut == "exhausted" or s.putOut == "unconfirmed") implies s.putRound != s.round
  })
}
