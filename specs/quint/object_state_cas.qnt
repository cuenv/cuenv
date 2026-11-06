// -*- mode: Bluespec; -*-
//
// The lock and compare-and-swap protocol of the S3 state store.
//
// The client side of this model is the code: each client state below is a
// state of the protocols in crates/infrastructure/src/state/object/protocol.rs
// (`Acquire`, `Mutate`, `Release`), and each named action is one step those
// protocols take. The `mbt` tests in that crate replay traces of this model
// against the protocols and compare the two after every step, so the model
// and the code cannot drift apart unnoticed.
//
// One tenant, one record address. `state.json` is a single object written
// only with conditional PUTs (`If-Match: <etag>` / `If-None-Match: *`).
//
// Abstractions:
// - The ETag is modelled by `ver`, the document serial. Every write bumps the
//   serial and sets a fresh `writer`, so bodies never repeat and an ETag
//   never recurs. Taken as given.
// - The network is adversarial: a PUT may be processed at any later time, or
//   never; a response may be lost after the store committed; the client may
//   time out at any point (`S3Failure::Ambiguous`). A 412 reaches the client
//   only while it still waits for that request. 429s, 404s and permanent
//   failures are not modelled.
// - Waits (backoff, the vanished-state check before a create) take no step.
// - Each client runs acquire_lock -> list -> put_if_unchanged -> unlock, as
//   the engine does. If the record write errored, the engine saved an
//   unrecorded-change file (engine.rs `record_change`); the client then runs
//   again and replays the same ConditionalPut (same expected version, same
//   generation).
// - An operator may `force_unlock` whichever lock is held, once.
// - MAX_ATTEMPTS stands in for MUTATION_ATTEMPTS (8 in the code). 3 is the
//   smallest value that reaches every branch of the retry loops.
//
// Check (TLC explores every reachable state of this instance):
//   quint verify object_state_cas.qnt --backend=tlc --tlc-config=tlc.json --invariants safety unlockReportsTruth
//
// - safety and unlockReportsTruth hold.
// - unlockReportsTruth fails with RELEASE = "notHeld" (the code at 0.68.0):
//   unlock() reported "no longer held" after its own release landed late.
// - unlockReportsTruth fails with RELEASE = "anyGone" (the first fix): after
//   a force-unlock and another run's acquire, unlock() reported "released"
//   and returned that run's locked document for a snapshot.
// - errorMeansNotWritten fails, by design (see below). Not checked in CI.
//
// This is model checking of a finite instance, not a proof for all sizes.
module object_state_cas {

  // ---- parameters ---------------------------------------------------------

  pure val CLIENTS = Set(1, 2)
  pure val MAX_ATTEMPTS = 3
  pure val MAX_ROUNDS = 2     // first run + one replay of an unrecorded change
  pure val MAX_TRIES = 1      // acquire attempts (a `Locked` run gives up)
  pure val ABSENT = -1        // the condition `If-None-Match: *`
  pure val OPERATOR = -1

  // What Release concludes when, after sending its own PUT, a re-read no
  // longer shows its lock:
  // "checked": the code. "released" only if the document shows this lock
  //   released (`lock` empty, `lastReleased == id`): its own release landed
  //   late, or an operator force-unlocked it. Otherwise "gone": the lock is
  //   not held, but this call cannot tell whose write removed it, and the
  //   document it read is not one it may snapshot.
  // "anyGone": the first fix (PR 576 before review). "released", returning
  //   whatever document it read, even one another run has locked since.
  // "notHeld": the code at 0.68.0, which reported "no longer held".
  pure val RELEASE = "checked"

  // ---- types --------------------------------------------------------------

  // `state.json`. `gen == 0` means the record is absent.
  type Doc = {
    exists: bool, ver: int, lock: int, lastReleased: int, writer: int,
    gen: int, serial: int, content: int,
  }

  // A conditional PUT on the wire.
  type Req = { client: int, round: int, holder: int, nonce: int, kind: str, cond: int, body: Doc }

  // A client: the engine's progress, and the state of the protocol it runs.
  //
  // pc, protocol state in protocol.rs:
  //   idle, list, released, done      the engine, between protocols
  //   acq_read                        Acquire  Reading
  //   acq_create                      Acquire  Creating
  //   acq_create_refused              Acquire  CreateRefused
  //   acq_create_readback             Acquire  CreateReadingBack
  //   acq_write                       Acquire  Writing
  //   acq_readback                    Acquire  ReadingBack
  //   put_read, put_write, put_readback   Mutate   Reading, Writing, ReadingBack
  //   rel_read, rel_write, rel_readback   Release  Reading, Writing, ReadingBack
  type Client = {
    pc: str, attempt: int, createAttempt: int, waiting: int, resp: str,
    round: int, tries: int, replay: bool,
    expGen: int, expSerial: int, payGen: int,
    acqOut: str, putOut: str, relOut: str,
    relLock: int,           // the lock in the document Release returned
    // ghost: what the store committed on this client's behalf
    puts: int, putFromExpected: bool, putRound: int, relRound: int,
  }

  // A client's next state and the PUT it sends, if any.
  type Move = { next: Client, sent: Set[Req] }

  pure val NO_DOC: Doc = {
    exists: false, ver: 0, lock: 0, lastReleased: 0, writer: 0,
    gen: 0, serial: 0, content: 0,
  }

  pure val FRESH: Client = {
    pc: "idle", attempt: 0, createAttempt: 0, waiting: 0, resp: "none",
    round: 1, tries: 0, replay: false,
    expGen: 0, expSerial: 0, payGen: 0,
    acqOut: "", putOut: "", relOut: "", relLock: 0,
    puts: 0, putFromExpected: false, putRound: 0, relRound: 0,
  }

  pure val READING = Set(
    "acq_read", "acq_create_refused", "acq_create_readback", "acq_readback",
    "put_read", "put_readback", "rel_read", "rel_readback",
  )
  pure val WAITING = Set("acq_create", "acq_write", "put_write", "rel_write")

  // StateLock::generate(): a fresh identifier per lock attempt.
  pure def lockOf(c: int, s: Client): int = c * 100 + s.round * 10 + s.tries
  // The record content client c writes (distinct per client).
  pure def contentOf(c: int): int = c * 1000
  // write_generation(Absent): a fresh uuid when the engine builds the record.
  pure def insertionOf(c: int, s: Client): int = c * 10000 + s.round
  // The `writer` nonce of a write (unique per client, round, try, phase and
  // attempt). A create keeps its nonce across its retries, as the code does.
  pure def nonceOf(c: int, s: Client, phase: int): int =
    c * 100000 + s.round * 10000 + s.tries * 1000 + phase * 100 + s.attempt

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

  // ---- client moves (pure) ------------------------------------------------

  pure def stay(s: Client): Move = { next: s, sent: Set() }

  pure def send(c: int, s: Client, phase: int, kind: str, cond: int, body: Doc, pc: str): Move = {
    val nonce = nonceOf(c, s, phase)
    {
      next: { ...s, pc: pc, waiting: nonce, resp: "none" },
      sent: Set({
        client: c, round: s.round, holder: lockOf(c, s), nonce: nonce,
        kind: kind, cond: cond, body: { ...body, writer: nonce },
      }),
    }
  }

  pure def idle(s: Client, pc: str): Client = { ...s, pc: pc, waiting: 0, resp: "none" }

  pure def acquired(s: Client): Client =
    { ...idle(s, if (s.replay) "put_read" else "list"), attempt: 0, createAttempt: 0, acqOut: "acquired" }

  pure def acquireFailed(s: Client, out: str): Client = { ...idle(s, "done"), acqOut: out }

  // On running out of attempts the code has already counted the last one.
  pure def retryAcquire(s: Client): Client =
    if (s.attempt + 1 >= MAX_ATTEMPTS) acquireFailed({ ...s, attempt: s.attempt + 1 }, "uncertain")
    else { ...idle(s, "acq_read"), attempt: s.attempt + 1 }

  pure def putDone(s: Client, out: str): Client = { ...idle(s, "rel_read"), attempt: 0, putOut: out }

  pure def retryPut(s: Client): Client =
    if (s.attempt + 1 >= MAX_ATTEMPTS) putDone(s, "exhausted")
    else { ...idle(s, "put_read"), attempt: s.attempt + 1 }

  pure def releaseDone(s: Client, out: str): Client = { ...idle(s, "released"), relOut: out, relLock: 0 }

  // A read after this call sent a release (`attempt > 0`) finds its lock
  // gone.
  pure def releaseGone(s: Client, d: Doc, me: int): Client =
    if (s.attempt == 0 or RELEASE == "notHeld") releaseDone(s, "notHeld")
    else if (RELEASE == "anyGone") { ...releaseDone(s, "released"), relLock: d.lock }
    else if (d.exists and d.lock == 0 and d.lastReleased == me) releaseDone(s, "released")
    else releaseDone(s, "gone")

  pure def retryRelease(s: Client): Client =
    if (s.attempt + 1 >= MAX_ATTEMPTS) releaseDone({ ...s, attempt: s.attempt + 1 }, "error")
    else { ...idle(s, "rel_read"), attempt: s.attempt + 1 }

  // Acquire::create: `If-None-Match: *`, keeping the nonce across retries.
  pure def create(c: int, s: Client, createAttempt: int): Move =
    if (createAttempt >= MAX_ATTEMPTS) stay(acquireFailed(s, "error"))
    else send(c, { ...s, createAttempt: createAttempt }, 1, "create", ABSENT,
              { ...NO_DOC, exists: true, ver: 1, lock: lockOf(c, s) }, "acq_create")

  // A read: the protocol's next step given the document.
  pure def onRead(c: int, s: Client, d: Doc): Move = {
    val me = lockOf(c, s)
    if (s.pc == "acq_read")
      if (not(d.exists)) create(c, s, 0)
      else if (d.lock == me) stay(acquired(s))
      else if (d.lock != 0)
        stay(if (s.tries + 1 < MAX_TRIES) { ...idle(s, "idle"), tries: s.tries + 1, acqOut: "locked" }
             else acquireFailed(s, "locked"))
      else send(c, s, 1, "acquire", d.ver, { ...d, ver: d.ver + 1, lock: me }, "acq_write")
    else if (s.pc == "acq_create_refused")
      stay(if (d.exists and d.writer == nonceOf(c, s, 1)) acquired(s) else retryAcquire(s))
    else if (s.pc == "acq_create_readback")
      if (not(d.exists)) create(c, s, s.createAttempt + 1)
      else stay(if (d.writer == nonceOf(c, s, 1)) acquired(s) else retryAcquire(s))
    else if (s.pc == "acq_readback")
      stay(if (d.exists and d.lock == me) acquired(s) else retryAcquire(s))
    else if (s.pc == "put_read")
      if (not(d.exists)) stay(putDone(s, "vanished"))
      else if (d.lock != me) stay(putDone(s, "lockLost"))
      else if (d.gen != s.expGen or d.serial != s.expSerial)
        stay(putDone(s, if (isRecorded(c, s, d)) "ok" else "stateChanged"))
      else send(c, s, 2, "put", d.ver, {
        ...d, ver: d.ver + 1,
        gen: if (d.gen == 0) s.payGen else d.gen,
        serial: d.serial + 1,
        content: contentOf(c),
      }, "put_write")
    else if (s.pc == "put_readback")
      if (not(d.exists)) stay(putDone(s, "vanished"))
      else stay(if (d.writer == nonceOf(c, s, 2)) putDone(s, "ok") else retryPut(s))
    else if (s.pc == "rel_read")
      if (not(d.exists) or d.lock != me) stay(releaseGone(s, d, me))
      else send(c, s, 3, "release", d.ver, { ...d, ver: d.ver + 1, lock: 0, lastReleased: me }, "rel_write")
    else // rel_readback
      stay(if (d.exists and d.lastReleased == me and d.lock == 0) releaseDone(s, "released")
           else retryRelease(s))
  }

  // The response to the PUT the client waits for: "ok" or "412".
  pure def onResponse(s: Client): Client =
    if (s.pc == "acq_create") (if (s.resp == "ok") acquired(s) else idle(s, "acq_create_refused"))
    else if (s.pc == "acq_write") (if (s.resp == "ok") acquired(s) else retryAcquire(s))
    else if (s.pc == "put_write") (if (s.resp == "ok") putDone(s, "ok") else retryPut(s))
    else (if (s.resp == "ok") releaseDone(s, "released") else retryRelease(s))

  // The PUT the client waits for is not resolved (S3Failure::Ambiguous).
  pure def onTimeout(s: Client): Client =
    if (s.pc == "acq_create") idle(s, "acq_create_readback")
    else if (s.pc == "acq_write") idle(s, "acq_readback")
    else if (s.pc == "put_write") idle(s, "put_readback")
    else idle(s, "rel_readback")

  // ---- actions --------------------------------------------------------------

  action move(c: int, m: Move): bool = {
    val cs = clients.set(c, m.next)
    all {
      clients' = cs,
      inflight' = gc(inflight.union(m.sent), doc, cs),
      doc' = doc, forcedLock' = forcedLock,
      fenceOk' = fenceOk, lastPut' = lastPut, writers' = writers,
    }
  }

  // The engine starts acquire_lock.
  action begin = {
    nondet c = CLIENTS.oneOf()
    val s = clients.get(c)
    all { s.pc == "idle", move(c, stay({ ...idle(s, "acq_read"), attempt: 0, createAttempt: 0 })) }
  }

  // A protocol reads `state.json`.
  action read = {
    nondet c = CLIENTS.oneOf()
    val s = clients.get(c)
    all { READING.contains(s.pc), move(c, onRead(c, s, doc)) }
  }

  // A protocol receives the response to its PUT.
  action respond = {
    nondet c = CLIENTS.oneOf()
    val s = clients.get(c)
    all { WAITING.contains(s.pc), s.resp != "none", move(c, stay(onResponse(s))) }
  }

  // A protocol gives up waiting for its PUT; the request stays in flight.
  action timeout = {
    nondet c = CLIENTS.oneOf()
    val s = clients.get(c)
    all { WAITING.contains(s.pc), move(c, stay(onTimeout(s))) }
  }

  // StateStore::list, then the engine builds the ConditionalPut.
  action list = {
    nondet c = CLIENTS.oneOf()
    val s = clients.get(c)
    all {
      s.pc == "list",
      move(c, stay({ ...idle(s, "put_read"), attempt: 0,
                     expGen: doc.gen, expSerial: doc.serial,
                     payGen: if (doc.gen != 0) doc.gen else insertionOf(c, s) })),
    }
  }

  // After unlock: replay an unrecorded change in a new run, or stop.
  action finish = {
    nondet c = CLIENTS.oneOf()
    val s = clients.get(c)
    all {
      s.pc == "released",
      move(c, stay(
        if (s.putOut != "ok" and s.round < MAX_ROUNDS)
          { ...FRESH, round: s.round + 1, replay: true,
            expGen: s.expGen, expSerial: s.expSerial, payGen: s.payGen,
            puts: s.puts, putFromExpected: s.putFromExpected,
            putRound: s.putRound, relRound: s.relRound }
        else idle(s, "done"))),
    }
  }

  // The store processes a request.
  action deliver = all {
    inflight != Set(),
    {
      nondet r = inflight.oneOf()
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
      val d = if (matches) r.body else doc
      val cs = clients.set(r.client, s2)
      all {
        doc' = d,
        clients' = cs,
        inflight' = gc(inflight.exclude(Set(r)), d, cs),
        forcedLock' = forcedLock,
        fenceOk' = (fenceOk and (not(isPut) or doc.lock == r.holder)),
        lastPut' = if (isPut) r.body.content else lastPut,
        writers' = if (isPut) writers.union(Set(r.client)) else writers,
      }
    },
  }

  // `state unlock --force <id>` on whatever lock is held, once.
  action forceUnlock = all {
    forcedLock == 0,
    doc.exists,
    doc.lock != 0,
    {
      val d = { ...doc, ver: doc.ver + 1, lock: 0, lastReleased: doc.lock, writer: OPERATOR }
      all {
        doc' = d,
        forcedLock' = doc.lock,
        inflight' = gc(inflight, d, clients),
        clients' = clients,
        fenceOk' = fenceOk, lastPut' = lastPut, writers' = writers,
      }
    },
  }

  action quiescent = all {
    CLIENTS.forall(c => clients.get(c).pc == "done"),
    doc' = doc, inflight' = inflight, clients' = clients, forcedLock' = forcedLock,
    fenceOk' = fenceOk, lastPut' = lastPut, writers' = writers,
  }

  action step = any { begin, read, respond, timeout, list, finish, deliver, forceUnlock, quiescent }

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

  // unlock() reports the release truthfully:
  // - "notHeld" only if this run's own release did not commit.
  //   FAILS with RELEASE = "notHeld".
  // - "released" only if this run's lock really was released, by its own
  //   release or by an operator's force-unlock.
  // - the document it returns, which unlock() snapshots into history, holds
  //   no lock. FAILS with RELEASE = "anyGone": a force-unlock, another
  //   run's acquire, then this run's retry returns that run's locked
  //   document.
  val unlockReportsTruth = CLIENTS.forall(c => {
    val s = clients.get(c)
    and {
      s.relOut == "notHeld" implies s.relRound != s.round,
      s.relOut == "released" implies (s.relRound == s.round or forcedLock == lockOf(c, s)),
      s.relOut == "released" implies s.relLock == 0,
    }
  })

  // Characterisation, expected to FAIL: an error from put_if_unchanged does
  // not mean "not written". This is why unrecorded-change files exist, and
  // why the replay must recognise its own earlier write (is_recorded).
  val errorMeansNotWritten = CLIENTS.forall(c => {
    val s = clients.get(c)
    (s.putOut == "lockLost" or s.putOut == "exhausted") implies s.putRound != s.round
  })
}
