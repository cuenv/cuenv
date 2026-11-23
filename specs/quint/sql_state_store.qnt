// -*- mode: Bluespec; -*-
//
// The lock and fenced writes of the SQL state store (Turso and D1), and the
// CLI's release retry, as the code implements them:
// - crates/infrastructure/src/state/sql/implementation.rs: acquire_lock_row
//   (INSERT ... ON CONFLICT DO NOTHING, read back on a zero count or a
//   transient failure), put_if_unchanged (UPDATE guarded by the lock and the
//   expected serial, then `holds` and `is_recorded`), unlock (DELETE by
//   identifier), each statement run through `retrying`.
// - crates/cuenv/src/commands/infrastructure/lock.rs: `release`.
//
// One tenant, one record. Each statement is atomic in the database. The
// network is adversarial, as in object_state_cas.qnt: a statement may be
// executed at any later time or never, its response may be lost, and the
// client may give up waiting at any point (a transient failure). A retry
// sends the same statement again. MAX_RETRIES stands in for the retry
// policy; CLI_ATTEMPTS for RELEASE_ATTEMPTS.
//
// Check (TLC explores every reachable state of this instance):
//   quint verify sql_state_store.qnt --backend=tlc --tlc-config=tlc.json --invariants safety unlockReportsTruth
//
// Mutation switches (constants below); each should make an invariant fail:
// - LOCK_GUARD = false: record writes without `LOCK_HELD`: safety fails.
// - SERIAL_GUARD = false: record writes without the expected serial:
//   safety fails (a delayed retry of an old write lands again).
// - CONFIRM_RETRIED_UNLOCK = false (the code before this model): an unlock
//   whose first DELETE committed but lost its response is retried, the
//   retry deletes nothing, and the run reports the lock as already gone:
//   unlockReportsTruth fails.
//
// This is model checking of a finite instance, not a proof for all sizes.
module sql_state_store {

  // ---- parameters ---------------------------------------------------------

  pure val CLIENTS = Set(1, 2)
  pure val MAX_RETRIES = 1
  pure val CLI_ATTEMPTS = 2
  pure val FORCE_UNLOCKS = 1

  // Mutation switches. The first value is the code as it is.
  pure val LOCK_GUARD = true
  pure val SERIAL_GUARD = true
  pure val CONFIRM_RETRIED_UNLOCK = true

  // ---- types --------------------------------------------------------------

  // The record. serial == 0: absent.
  type Row = { serial: int, content: int }

  // A statement on the wire. sid 1: the send its client waits for.
  type Req = { client: int, sid: int, kind: str, lock: int, expSerial: int, content: int }

  type Client = {
    pc: str, retries: int, cliTry: int,
    waiting: int, resp: str,           // resp: "none" | "n0" | "n1" (rows affected)
    expSerial: int,
    putOut: str, relOut: str,
    // ghost
    puts: int, putFromExpected: bool, relCommitted: bool,
  }

  pure val FRESH: Client = {
    pc: "acquire", retries: 0, cliTry: 1, waiting: 0, resp: "none",
    expSerial: 0, putOut: "", relOut: "",
    puts: 0, putFromExpected: false, relCommitted: false,
  }

  pure def lockOf(c: int): int = c
  pure def contentOf(c: int): int = c * 10

  // ---- state --------------------------------------------------------------

  var lockRow: int            // the lock row's identifier; 0: none
  var rec: Row
  var inflight: Set[Req]
  var clients: int -> Client
  var forced: int             // force-unlocks left
  // ghost
  var fenceOk: bool           // every record write landed under its writer's lock
  var lastPut: int

  action init = all {
    lockRow' = 0,
    rec' = { serial: 0, content: 0 },
    inflight' = Set(),
    clients' = CLIENTS.mapBy(_ => FRESH),
    forced' = FORCE_UNLOCKS,
    fenceOk' = true,
    lastPut' = 0,
  }

  // ---- client helpers -----------------------------------------------------

  action local(c: int, s: Client): bool = all {
    clients' = clients.set(c, s),
    lockRow' = lockRow, rec' = rec, inflight' = inflight, forced' = forced,
    fenceOk' = fenceOk, lastPut' = lastPut,
  }

  // Send a statement and wait. A retry sends the same statement again.
  // The client waits only for its latest send (sid 1); an earlier one it
  // no longer waits for becomes sid 0, so identical stale copies of one
  // statement are one element of the set. Two stale copies of the same
  // statement can do no more than one: each is guarded by the lock row or
  // the serial, and a client never takes its lock back after losing it.
  action send(c: int, s: Client, kind: str, waitPc: str): bool = all {
    inflight' = inflight
      .map(r => if (r.client == c) { ...r, sid: 0 } else r)
      .union(Set({
        client: c, sid: 1, kind: kind, lock: lockOf(c),
        expSerial: s.expSerial, content: contentOf(c),
      })),
    clients' = clients.set(c, { ...s, pc: waitPc, waiting: 1, resp: "none" }),
    lockRow' = lockRow, rec' = rec, forced' = forced, fenceOk' = fenceOk, lastPut' = lastPut,
  }

  pure def relDone(s: Client, out: str): Client =
    { ...s, pc: "done", waiting: 0, resp: "none", relOut: out }

  // unlock() returned Ok(false): NoLongerHeld on the CLI's first call,
  // Uncertain on a retry.
  pure def relNone(s: Client): Client =
    relDone(s, if (s.cliTry > 1) "uncertain" else "notHeld")

  // unlock() failed: the CLI retries, or gives up.
  pure def relFailed(s: Client): Client =
    if (s.cliTry >= CLI_ATTEMPTS) relDone(s, "error")
    else { ...s, pc: "rel_send", cliTry: s.cliTry + 1, retries: 0, waiting: 0, resp: "none" }

  pure def toRelease(s: Client, out: str): Client =
    { ...s, pc: "rel_send", retries: 0, waiting: 0, resp: "none", putOut: out }

  // ConditionalPut::is_recorded for the stored record.
  pure def isRecorded(c: int, s: Client, r: Row): bool =
    r.serial == s.expSerial + 1 and r.content == contentOf(c)

  // ---- one client step ----------------------------------------------------

  action clientStep(c: int): bool = {
    val s = clients.get(c)
    val me = lockOf(c)
    any {
      // acquire_lock_row
      all { s.pc == "acquire", send(c, s, "insertLock", "acq_wait") },
      all { s.pc == "acq_wait", s.resp == "n1", local(c, { ...s, pc: "list", waiting: 0 }) },
      // Zero rows, or a transient failure: read the lock back.
      all { s.pc == "acq_wait", s.resp == "n0", local(c, { ...s, pc: "acq_read", waiting: 0 }) },
      all { s.pc == "acq_wait", local(c, { ...s, pc: "acq_read", waiting: 0 }) },
      all { s.pc == "acq_read",
        local(c,
          if (lockRow == me) { ...s, pc: "list" }
          else if (lockRow != 0) { ...s, pc: "done", relOut: "locked" }
          else if (s.retries < MAX_RETRIES) { ...s, pc: "acquire", retries: s.retries + 1 }
          else { ...s, pc: "done", relOut: "acquireFailed" }) },

      // list, then put_if_unchanged
      all { s.pc == "list", local(c, { ...s, pc: "put_send", retries: 0, expSerial: rec.serial }) },
      all { s.pc == "put_send", send(c, s, "update", "put_wait") },
      all { s.pc == "put_wait", s.resp == "n1", local(c, toRelease(s, "ok")) },
      all { s.pc == "put_wait", s.resp == "n0", local(c, { ...s, pc: "put_check", waiting: 0 }) },
      // A transient failure: `retrying` sends the statement again, or
      // gives up with an error.
      all { s.pc == "put_wait",
        local(c, if (s.retries < MAX_RETRIES) { ...s, pc: "put_send", retries: s.retries + 1, waiting: 0 }
                 else toRelease(s, "error")) },
      all { s.pc == "put_check",
        local(c, if (lockRow != me) toRelease(s, "lockLost")
                 else if (isRecorded(c, s, rec)) toRelease(s, "ok")
                 else toRelease(s, "stateChanged")) },

      // unlock (DELETE by identifier), inside the CLI's release loop.
      all { s.pc == "rel_send", send(c, s, "deleteLock", "rel_wait") },
      all { s.pc == "rel_wait", s.resp == "n1", local(c, relDone(s, "released")) },
      all { s.pc == "rel_wait", s.resp == "n0",
        local(c, if (s.retries > 0 and CONFIRM_RETRIED_UNLOCK) relFailed(s) else relNone(s)) },
      all { s.pc == "rel_wait",
        local(c, if (s.retries < MAX_RETRIES) { ...s, pc: "rel_send", retries: s.retries + 1, waiting: 0 }
                 else relFailed(s)) },
    }
  }

  // ---- the database and the network ---------------------------------------

  action deliverOne(r: Req): bool = {
    val s = clients.get(r.client)
    val applies =
      if (r.kind == "insertLock") lockRow == 0
      else if (r.kind == "deleteLock") lockRow == r.lock
      else and {
        not(LOCK_GUARD) or lockRow == r.lock,
        not(SERIAL_GUARD) or rec.serial == r.expSerial,
      }
    val isPut = applies and r.kind == "update"
    val s1 = if (r.sid == 1 and s.waiting == 1) { ...s, resp: if (applies) "n1" else "n0" } else s
    val s2 = if (isPut) { ...s1, puts: s1.puts + 1,
                          putFromExpected: s1.putFromExpected or rec.serial == s1.expSerial }
             else if (applies and r.kind == "deleteLock") { ...s1, relCommitted: true }
             else s1
    all {
      lockRow' = if (not(applies)) lockRow
                 else if (r.kind == "insertLock") r.lock
                 else if (r.kind == "deleteLock") 0
                 else lockRow,
      rec' = if (isPut) { serial: rec.serial + 1, content: r.content } else rec,
      inflight' = inflight.exclude(Set(r)),
      clients' = clients.set(r.client, s2),
      forced' = forced,
      fenceOk' = (fenceOk and (not(isPut) or lockRow == r.lock)),
      lastPut' = if (isPut) r.content else lastPut,
    }
  }

  // A statement that is never executed needs no action: it stays in flight.

  // `unlock --force` of whatever lock is held.
  action forceUnlock = all {
    forced > 0, lockRow != 0,
    lockRow' = 0, forced' = forced - 1,
    rec' = rec, inflight' = inflight, clients' = clients,
    fenceOk' = fenceOk, lastPut' = lastPut,
  }

  action quiescent = all {
    CLIENTS.forall(c => clients.get(c).pc == "done"),
    lockRow' = lockRow, rec' = rec, inflight' = inflight, clients' = clients,
    forced' = forced, fenceOk' = fenceOk, lastPut' = lastPut,
  }

  action step = any {
    nondet c = CLIENTS.oneOf()
    clientStep(c),
    all { inflight != Set(), nondet r = inflight.oneOf() deliverOne(r) },
    forceUnlock,
    quiescent,
  }

  // ---- properties ---------------------------------------------------------

  // Fencing, no lost update, Ok means written, at most once.
  val safety = and {
    fenceOk,
    rec.content == lastPut,
    CLIENTS.forall(c => clients.get(c).putOut == "ok" implies clients.get(c).putFromExpected),
    CLIENTS.forall(c => clients.get(c).puts <= 1),
  }

  // The CLI reports the release truthfully: "notHeld" (NoLongerHeld) only
  // if no DELETE of this run's lock committed; "released" only if one did.
  val unlockReportsTruth = CLIENTS.forall(c => {
    val s = clients.get(c)
    and {
      s.relOut == "notHeld" implies not(s.relCommitted),
      s.relOut == "released" implies s.relCommitted,
    }
  })

  // Reachability, expected to FAIL.
  val neverUncertain = CLIENTS.forall(c => clients.get(c).relOut != "uncertain")
  val neverBothWrote = not(CLIENTS.forall(c => clients.get(c).putOut == "ok"))
}
