// -*- mode: Bluespec; -*-
//
// The task action cache over a content-addressed store, as cuenv implements
// it: LocalCas + RemoteCas behind LayeredCas, LocalActionCache +
// RemoteActionCache behind LayeredActionCache, driven by
// task-exec `execute_task` (lookup -> missing_blobs -> materialize_hit ->
// commit_verified, or execute -> record_resolved -> update).
//
// One action digest, two machines sharing one remote (REAPI) cache. Each
// result names two output files. A machine runs the task RUNS times.
//
// Abstractions:
// - A store maps a digest to ABSENT, OK (bytes match the digest) or CORRUPT
//   (bytes under that name do not hash to it).
// - Every remote operation may fail (the server is unreachable); LayeredCas
//   and LayeredActionCache turn that into a miss or a dropped upload.
// - The adversary has FAULTS faults to spend, at any time: evict or corrupt
//   a remote blob, evict the remote action-cache entry, evict or corrupt a
//   local blob, or make one local CAS write fail.
// - Action-cache writers are trusted (Bazel's model): an entry only ever
//   names a result some machine produced. A malicious writer is out of scope.
// - Workspace commit (renames out of staging) does not fail.
//
// Check (TLC explores every reachable state of this instance; ~2 min):
//   quint verify task_cache.qnt --backend=tlc --tlc-config=tlc.json --invariants safety fixes
//
// The model lets machines upload and publish. The CLI keeps the remote
// read-only for now (remote_cache.rs: `let upload = false`), so in production
// today only the read side applies; the upload and publish paths are checked
// ahead of being enabled.
//
// Results, 2 machines, 2 runs each, 2 faults:
// - safety and fixes: hold; ~1.5M distinct states.
// - With TRACK_CORRUPT = false (the code at 0.68.0), `fixes` fails: a corrupt
//   remote blob is never replaced, because FindMissingBlobs reports it present
//   and the upload is skipped.
// - With CHECK_PROMOTION = false (the code at 0.68.0), `fixes` fails: a remote
//   hit whose blob could not be stored locally is promoted anyway.
// - With VERIFY_READS = false or STAGE_OUTPUTS = false, `safety` fails.
//   Skipping missing_blobs entirely does not: staging plus verified reads
//   carry safety, and missing_blobs only avoids wasted fetches.
// - neverHit and neverCrossMachineHit fail, so hits (including from the other
//   machine's remote entry) are reachable: the model is not vacuous.
//
// Model checking of a finite instance, not a proof for all sizes.
module task_cache {

  // ---- parameters ---------------------------------------------------------

  pure val MACHINES = Set(1, 2)
  pure val RESULTS = Set(1, 2)
  pure val ACTION_BLOB = 99            // the Action message's own blob
  pure val BLOBS = Set(11, 12, 21, 22, ACTION_BLOB)
  pure val RUNS = 2
  pure val FAULTS = 2

  // false: the task is not deterministic; machine m's run produces result m.
  pure val DETERMINISTIC = false

  // Mutation switches. All true is the code as it is.
  pure val VERIFY_READS = true    // RemoteCas/LocalCas check the digest of what they read
  pure val STAGE_OUTPUTS = true   // materialize into staging, commit only when complete
  pure val TRACK_CORRUPT = true   // CorruptionAwareCas wraps the remote CAS
  pure val CHECK_PROMOTION = true // LayeredActionCache::commit_verified checks the local CAS

  // ---- values -------------------------------------------------------------

  pure val ABSENT = 0
  pure val OK = 1
  pure val CORRUPT = 2
  pure val OLD = 0                // what the workspace held before any run
  pure val BAD_BYTES = -1         // bytes that do not match the digest that named them

  pure def fileA(r: int): int = r * 10 + 1
  pure def fileB(r: int): int = r * 10 + 2
  pure def resultOf(m: int): int = if (DETERMINISTIC) 1 else m

  // ---- state --------------------------------------------------------------

  type Stores = {
    local: int -> (int -> int),   // machine -> digest -> blob state
    localAC: int -> int,          // machine -> result (0: none)
    remote: int -> int,           // digest -> blob state
    remoteAC: int,                // result (0: none)
    faults: int,
  }

  type Machine = {
    pc: str, round: int, cand: int,
    stagedA: int, stagedB: int,
    wsA: int, wsB: int,           // the two declared outputs in the workspace
    outcome: str,                 // "hit" | "executed" | ""
    knownBad: Set[int],           // remote reads that failed verification in this process (used only with TRACK_CORRUPT)
  }

  type Ghost = {
    produced: Set[int],           // results some execution produced
    promotedIncomplete: bool,     // commit_verified wrote a local entry whose blobs are not local
    uploadLeftCorrupt: bool,      // an upload of a blob known corrupt left it corrupt on an honest server
    publishedKnownBad: bool,      // a result was published naming a blob its publisher knew was corrupt
  }

  var st: Stores
  var machines: int -> Machine
  var g: Ghost

  pure val EMPTY_STORE: int -> int = BLOBS.mapBy(_ => ABSENT)

  action init = all {
    st' = {
      local: MACHINES.mapBy(_ => EMPTY_STORE),
      localAC: MACHINES.mapBy(_ => 0),
      remote: EMPTY_STORE,
      remoteAC: 0,
      faults: FAULTS,
    },
    machines' = MACHINES.mapBy(_ => {
      pc: "start", round: 1, cand: 0, stagedA: OLD, stagedB: OLD,
      wsA: OLD, wsB: OLD, outcome: "", knownBad: Set(),
    }),
    g' = { produced: Set(), promotedIncomplete: false, uploadLeftCorrupt: false, publishedKnownBad: false },
  }

  // ---- store operations ---------------------------------------------------

  // LocalCas::put_*: installs verified bytes; a corrupt blob is repaired.
  pure def localPut(s: Stores, m: int, d: int): Stores =
    { ...s, local: s.local.set(m, s.local.get(m).set(d, OK)) }

  // RemoteCas::put / put_file: skip the upload when FindMissingBlobs says
  // the server holds the digest, whatever bytes it holds.
  pure def remotePut(s: Stores, reach: bool, d: int): Stores =
    if (reach and s.remote.get(d) == ABSENT) { ...s, remote: s.remote.set(d, OK) } else s

  // CorruptionAwareCas::put_*: a digest known corrupt is re-uploaded with
  // replace_* (no FindMissingBlobs). A server may honour the overwrite or
  // ignore it (REAPI lets a server skip writes for digests it holds).
  pure def remoteReplace(s: Stores, reach: bool, honours: bool, d: int): Stores =
    if (reach and (honours or s.remote.get(d) == ABSENT)) { ...s, remote: s.remote.set(d, OK) } else s

  // RemoteCas::contains is FindMissingBlobs (existence); CorruptionAwareCas
  // reports a digest it saw corrupt as absent.
  pure def remoteHas(s: Stores, m: Machine, reach: bool, d: int): bool =
    reach and s.remote.get(d) != ABSENT and not(TRACK_CORRUPT and m.knownBad.contains(d))

  // LayeredCas::contains (used by missing_blobs on a hit). LocalCas::contains
  // verifies content.
  pure def present(s: Stores, m: int, mm: Machine, reach: bool, d: int): bool =
    s.local.get(m).get(d) == OK or remoteHas(s, mm, reach, d)

  // ---- one machine step ---------------------------------------------------

  action step_m(m: int, reach: bool, localWriteFails: bool, honours: bool): bool = {
    val s = machines.get(m)
    val loc = st.local.get(m)
    val dA = fileA(s.cand)
    val dB = fileB(s.cand)
    any {
      // build_action: the Action blob is written through LayeredCas.
      all { s.pc == "start",
        st' = remotePut(localPut(st, m, ACTION_BLOB), reach, ACTION_BLOB),
        machines' = machines.set(m, { ...s, pc: "lookup", cand: 0, stagedA: OLD, stagedB: OLD, outcome: "",
                                      knownBad: Set() }),
        g' = g },

      // LayeredActionCache::lookup: local first, then remote (unreachable = miss).
      all { s.pc == "lookup",
        val c = if (st.localAC.get(m) != 0) st.localAC.get(m)
                else if (reach) st.remoteAC else 0
        all {
          st' = st,
          machines' = machines.set(m, { ...s, cand: c, pc: if (c != 0) "check" else "exec" }),
          g' = g,
        } },

      // cache::lookup -> missing_blobs over LayeredCas.
      all { s.pc == "check",
        st' = st,
        machines' = machines.set(m, { ...s,
          pc: if (present(st, m, s, reach, dA) and present(st, m, s, reach, dB)) "fetchA" else "exec" }),
        g' = g },

      // materialize_outputs: LayeredCas::get_to_file per output.
      all { s.pc == "fetchA" or s.pc == "fetchB",
        val d = if (s.pc == "fetchA") dA else dB
        val nextPc = if (s.pc == "fetchA") "fetchB" else "commit"
        // Where the bytes come from, if anywhere:
        //   local OK -> local; else remote reachable and not ABSENT -> remote
        //   (verified when VERIFY_READS); otherwise the read fails.
        val fromLocal = loc.get(d) == OK
        val remoteServes = and { not(fromLocal), remoteHas(st, s, reach, d),
                                 st.remote.get(d) == OK or not(VERIFY_READS) }
        // A read that fails verification is remembered by CorruptionAwareCas.
        val sawCorrupt = and { not(fromLocal), remoteHas(st, s, reach, d),
                               st.remote.get(d) == CORRUPT, VERIFY_READS }
        val s0 = if (sawCorrupt) { ...s, knownBad: s.knownBad.union(Set(d)) } else s
        val bytes = if (fromLocal or (remoteServes and st.remote.get(d) == OK)) d else BAD_BYTES
        val failsLocalCopy = remoteServes and localWriteFails and st.faults > 0
        val keepsCopy = remoteServes and not(failsLocalCopy) and st.remote.get(d) == OK
        val s2 =
          if (not(fromLocal) and not(remoteServes)) { ...s0, pc: "exec", stagedA: OLD, stagedB: OLD }
          else if (not(STAGE_OUTPUTS))
            // Mutant: write straight into the workspace.
            if (s.pc == "fetchA") { ...s0, pc: nextPc, wsA: bytes } else { ...s0, pc: nextPc, wsB: bytes }
          else if (s.pc == "fetchA") { ...s0, pc: nextPc, stagedA: bytes }
          else { ...s0, pc: nextPc, stagedB: bytes }
        all {
          st' = if (failsLocalCopy) { ...st, faults: st.faults - 1 }
                else if (keepsCopy) localPut(st, m, d)
                else st,
          machines' = machines.set(m, s2),
          g' = g,
        } },

      // commit_staged_outputs, then ActionCache::commit_verified (local
      // only), which promotes only when the local CAS holds every blob.
      all { s.pc == "commit",
        val blobsLocal = loc.get(dA) == OK and loc.get(dB) == OK
        val promote = blobsLocal or not(CHECK_PROMOTION)
        all {
          st' = if (promote) { ...st, localAC: st.localAC.set(m, s.cand) } else st,
          machines' = machines.set(m,
            if (STAGE_OUTPUTS) { ...s, pc: "finish", wsA: s.stagedA, wsB: s.stagedB, outcome: "hit" }
            else { ...s, pc: "finish", outcome: "hit" }),
          g' = { ...g, promotedIncomplete: g.promotedIncomplete or (promote and not(blobsLocal)) },
        } },

      // Miss: run the task. Its outputs land in the workspace.
      all { s.pc == "exec",
        val r = resultOf(m)
        all {
          st' = st,
          machines' = machines.set(m, { ...s, pc: "putA", cand: r, wsA: fileA(r), wsB: fileB(r),
                                        stagedA: OLD, stagedB: OLD }),
          g' = { ...g, produced: g.produced.union(Set(r)) },
        } },

      // record_resolved: LayeredCas::put_file per output (local, then push).
      // A digest this process saw corrupt is replaced, then read back;
      // only a verified read-back clears it.
      all { s.pc == "putA" or s.pc == "putB",
        val d = if (s.pc == "putA") dA else dB
        val replacing = TRACK_CORRUPT and s.knownBad.contains(d)
        val st1 = localPut(st, m, d)
        val st2 = if (replacing) remoteReplace(st1, reach, honours, d) else remotePut(st1, reach, d)
        val repaired = replacing and reach and st2.remote.get(d) == OK
        all {
          st' = st2,
          machines' = machines.set(m, { ...s, pc: if (s.pc == "putA") "putB" else "update",
                                        knownBad: if (repaired) s.knownBad.exclude(Set(d)) else s.knownBad }),
          g' = { ...g, uploadLeftCorrupt: g.uploadLeftCorrupt or
                   (reach and honours and s.knownBad.contains(d) and st2.remote.get(d) == CORRUPT) },
        } },

      // LayeredActionCache::update: local entry, then the publish check
      // (Action blob and every output in the remote CAS), then publish.
      all { s.pc == "update",
        st' = { ...st, localAC: st.localAC.set(m, s.cand) },
        machines' = machines.set(m, { ...s,
          pc: if (and { remoteHas(st, s, reach, ACTION_BLOB),
                        remoteHas(st, s, reach, dA), remoteHas(st, s, reach, dB) })
              "publish" else "executed" }),
        g' = g },
      all { s.pc == "publish",
        st' = if (reach) { ...st, remoteAC: s.cand } else st,
        machines' = machines.set(m, { ...s, pc: "executed" }),
        g' = { ...g, publishedKnownBad: g.publishedKnownBad or
                 (reach and (s.knownBad.contains(dA) or s.knownBad.contains(dB))) } },
      all { s.pc == "executed",
        st' = st,
        machines' = machines.set(m, { ...s, pc: "finish", outcome: "executed" }),
        g' = g },

      // The next run of the same task, with the workspace as it was left.
      all { s.pc == "finish",
        st' = st,
        machines' = machines.set(m,
          if (s.round < RUNS) { ...s, pc: "start", round: s.round + 1 } else { ...s, pc: "done" }),
        g' = g },
    }
  }

  // ---- the adversary ------------------------------------------------------

  action fault = all {
    st.faults > 0,
    nondet d = BLOBS.oneOf()
    nondet m = MACHINES.oneOf()
    nondet kind = Set("evictRemote", "corruptRemote", "evictRemoteAC", "evictLocal", "corruptLocal").oneOf()
    val f = st.faults - 1
    all {
      st' =
        if (kind == "evictRemote") { ...st, faults: f, remote: st.remote.set(d, ABSENT) }
        else if (kind == "corruptRemote")
          { ...st, faults: f, remote: st.remote.set(d, if (st.remote.get(d) == OK) CORRUPT else st.remote.get(d)) }
        else if (kind == "evictRemoteAC") { ...st, faults: f, remoteAC: 0 }
        else if (kind == "evictLocal")
          { ...st, faults: f, local: st.local.set(m, st.local.get(m).set(d, ABSENT)) }
        else
          { ...st, faults: f, local: st.local.set(m, st.local.get(m).set(d,
              if (st.local.get(m).get(d) == OK) CORRUPT else st.local.get(m).get(d))) },
      machines' = machines,
      g' = g,
    }
  }

  action quiescent = all {
    MACHINES.forall(m => machines.get(m).pc == "done"),
    st' = st, machines' = machines, g' = g,
  }

  action step = any {
    nondet m = MACHINES.oneOf()
    nondet reach = Set(true, false).oneOf()
    nondet localWriteFails = Set(true, false).oneOf()
    nondet honours = Set(true, false).oneOf()
    step_m(m, reach, localWriteFails, honours),
    fault,
    quiescent,
  }

  // ---- properties ---------------------------------------------------------

  pure def isResult(a: int, b: int): bool = RESULTS.exists(r => a == fileA(r) and b == fileB(r))

  // No torn restore: the declared outputs are either what was there before,
  // or exactly the outputs of one result. Never a mix of two results, or of
  // a result and the old files.
  val noTornWorkspace = MACHINES.forall(m => {
    val s = machines.get(m)
    (s.wsA == OLD and s.wsB == OLD) or isResult(s.wsA, s.wsB)
  })

  // Integrity: no workspace file ever holds bytes that do not match the
  // digest the cache named them by.
  val noBadBytes = MACHINES.forall(m =>
    machines.get(m).wsA != BAD_BYTES and machines.get(m).wsB != BAD_BYTES)

  // Hit soundness: a hit restores exactly a result that some execution of
  // this action produced.
  val hitIsProduced = MACHINES.forall(m => {
    val s = machines.get(m)
    s.outcome == "hit" implies and {
      g.produced.contains(s.cand), s.wsA == fileA(s.cand), s.wsB == fileB(s.cand),
    }
  })

  val safety = and { noTornWorkspace, noBadBytes, hitIsProduced }

  // ---- properties of the fixes --------------------------------------------

  // A local action-cache entry promoted from a remote hit names only blobs
  // the local CAS holds. (Violated before commit_verified checked the local
  // CAS: a blob served after its local copy failed to write was promoted.)
  val promotedComplete = not(g.promotedIncomplete)

  // Re-executing a task whose output read back corrupt repairs the remote
  // blob whenever the server honours the overwrite. (Violated before
  // CorruptionAwareCas: FindMissingBlobs said "present" and the upload was
  // skipped, so the blob stayed corrupt for good.)
  val uploadsRepairRemote = not(g.uploadLeftCorrupt)

  // A machine never publishes a result naming a blob it saw corrupt and
  // could not repair.
  val noPublishOfKnownCorrupt = not(g.publishedKnownBad)

  val fixes = and { promotedComplete, uploadsRepairRemote, noPublishOfKnownCorrupt }

  // ---- witnesses (each expected to be VIOLATED) ----------------------------

  // Non-vacuity: a hit is reachable at all.
  val neverHit = MACHINES.forall(m => machines.get(m).outcome != "hit")

  // Non-vacuity: a hit served from the other machine's remote entry.
  val neverCrossMachineHit = MACHINES.forall(m => {
    val s = machines.get(m)
    not(s.outcome == "hit" and s.cand != resultOf(m))
  })
}
