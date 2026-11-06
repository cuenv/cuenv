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
// Check (TLC explores every reachable state of this instance; ~3 min):
//   quint verify task_cache.qnt --backend=tlc --tlc-config=tlc.json --invariants safety fixes
//
// The machine steps are the calls cuenv makes, one call per step, so the
// model-based test in crates/cas-remote/tests/reapi_roundtrip/mbt.rs can
// replay traces against the real stores. `update` is one step because
// LayeredActionCache::update checks and publishes in one call.
//
// The model lets machines upload and publish. The CLI keeps the remote
// read-only for now (remote_cache.rs: `let upload = false`), so in production
// today only the read side applies; the upload and publish paths are checked
// ahead of being enabled.
//
// Results, 2 machines, 2 runs each, 2 faults:
// - safety and fixes: hold; ~1.2M distinct states.
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

  // A machine's step, as a function of the stores, its state and the
  // environment's choices for this step: whether the remote answers, whether
  // a local CAS write fails, whether the server honours an overwrite.
  type Next = { st: Stores, s: Machine, g: Ghost }

  pure def machineNext(st: Stores, g: Ghost, m: int, s: Machine,
                       reach: bool, localWriteFails: bool, honours: bool): Next = {
    val loc = st.local.get(m)
    val dA = fileA(s.cand)
    val dB = fileB(s.cand)
    if (s.pc == "start")
      // build_action: the Action blob is written through LayeredCas.
      { st: remotePut(localPut(st, m, ACTION_BLOB), reach, ACTION_BLOB),
        s: { ...s, pc: "lookup", cand: 0, stagedA: OLD, stagedB: OLD, outcome: "", knownBad: Set() },
        g: g }
    else if (s.pc == "lookup") {
      // LayeredActionCache::lookup: local first, then remote (unreachable = miss).
      val c = if (st.localAC.get(m) != 0) st.localAC.get(m) else if (reach) st.remoteAC else 0
      { st: st, s: { ...s, cand: c, pc: if (c != 0) "check" else "exec" }, g: g }
    }
    else if (s.pc == "check")
      // cache::lookup -> missing_blobs over LayeredCas.
      { st: st,
        s: { ...s, pc: if (present(st, m, s, reach, dA) and present(st, m, s, reach, dB)) "fetchA" else "exec" },
        g: g }
    else if (s.pc == "fetchA" or s.pc == "fetchB") {
      // materialize_outputs: LayeredCas::get_to_file per output.
      val d = if (s.pc == "fetchA") dA else dB
      val nextPc = if (s.pc == "fetchA") "fetchB" else "commit"
      // Where the bytes come from, if anywhere: local OK -> local; else the
      // remote, if it has them (verified when VERIFY_READS); else the read
      // fails.
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
      { st: if (failsLocalCopy) { ...st, faults: st.faults - 1 }
            else if (keepsCopy) localPut(st, m, d)
            else st,
        s: s2, g: g }
    }
    else if (s.pc == "commit") {
      // commit_staged_outputs, then ActionCache::commit_verified (local
      // only), which promotes only when the local CAS holds every blob.
      val blobsLocal = loc.get(dA) == OK and loc.get(dB) == OK
      val promote = blobsLocal or not(CHECK_PROMOTION)
      { st: if (promote) { ...st, localAC: st.localAC.set(m, s.cand) } else st,
        s: if (STAGE_OUTPUTS) { ...s, pc: "finish", wsA: s.stagedA, wsB: s.stagedB, outcome: "hit" }
           else { ...s, pc: "finish", outcome: "hit" },
        g: { ...g, promotedIncomplete: g.promotedIncomplete or (promote and not(blobsLocal)) } }
    }
    else if (s.pc == "exec") {
      // Miss: run the task. Its outputs land in the workspace.
      val r = resultOf(m)
      { st: st,
        s: { ...s, pc: "putA", cand: r, wsA: fileA(r), wsB: fileB(r), stagedA: OLD, stagedB: OLD },
        g: { ...g, produced: g.produced.union(Set(r)) } }
    }
    else if (s.pc == "putA" or s.pc == "putB") {
      // record_resolved: LayeredCas::put_file per output (local, then push).
      // A digest this process saw corrupt is replaced, then read back; only
      // a verified read-back clears it.
      val d = if (s.pc == "putA") dA else dB
      val replacing = TRACK_CORRUPT and s.knownBad.contains(d)
      val st1 = localPut(st, m, d)
      val st2 = if (replacing) remoteReplace(st1, reach, honours, d) else remotePut(st1, reach, d)
      val repaired = replacing and reach and st2.remote.get(d) == OK
      { st: st2,
        s: { ...s, pc: if (s.pc == "putA") "putB" else "update",
             knownBad: if (repaired) s.knownBad.exclude(Set(d)) else s.knownBad },
        g: { ...g, uploadLeftCorrupt: g.uploadLeftCorrupt or
               (reach and honours and s.knownBad.contains(d) and st2.remote.get(d) == CORRUPT) } }
    }
    else if (s.pc == "update") {
      // LayeredActionCache::update: the local entry, then, in the same call,
      // the publish check (the Action blob and every output in the remote
      // CAS) and the publish.
      val st1 = { ...st, localAC: st.localAC.set(m, s.cand) }
      val publish = and { remoteHas(st, s, reach, ACTION_BLOB),
                          remoteHas(st, s, reach, dA), remoteHas(st, s, reach, dB) }
      { st: if (publish) { ...st1, remoteAC: s.cand } else st1,
        s: { ...s, pc: "finish", outcome: "executed" },
        g: { ...g, publishedKnownBad: g.publishedKnownBad or
               (publish and (s.knownBad.contains(dA) or s.knownBad.contains(dB))) } }
    }
    else // finish: the next run of the same task, with the workspace as it was left.
      { st: st,
        s: if (s.round < RUNS) { ...s, pc: "start", round: s.round + 1 } else { ...s, pc: "done" },
        g: g }
  }

  // One step of one machine.
  action machine = {
    nondet m = MACHINES.oneOf()
    nondet reach = Set(true, false).oneOf()
    nondet failing = Set(true, false).oneOf()   // the next local CAS write fails
    nondet honours = Set(true, false).oneOf()
    val s = machines.get(m)
    val n = machineNext(st, g, m, s, reach, failing, honours)
    all {
      s.pc != "done",
      st' = n.st,
      machines' = machines.set(m, n.s),
      g' = n.g,
    }
  }

  // ---- the adversary ------------------------------------------------------

  pure def applyFault(st: Stores, d: int, m: int, kind: str): Stores = {
    val f = st.faults - 1
    if (kind == "evictRemote") { ...st, faults: f, remote: st.remote.set(d, ABSENT) }
    else if (kind == "corruptRemote")
      { ...st, faults: f, remote: st.remote.set(d, if (st.remote.get(d) == OK) CORRUPT else st.remote.get(d)) }
    else if (kind == "evictRemoteAC") { ...st, faults: f, remoteAC: 0 }
    else if (kind == "evictLocal")
      { ...st, faults: f, local: st.local.set(m, st.local.get(m).set(d, ABSENT)) }
    else
      { ...st, faults: f, local: st.local.set(m, st.local.get(m).set(d,
          if (st.local.get(m).get(d) == OK) CORRUPT else st.local.get(m).get(d))) }
  }

  action fault = all {
    st.faults > 0,
    nondet d = BLOBS.oneOf()
    nondet m = MACHINES.oneOf()
    nondet kind = Set("evictRemote", "corruptRemote", "evictRemoteAC", "evictLocal", "corruptLocal").oneOf()
    all { st' = applyFault(st, d, m, kind), machines' = machines, g' = g },
  }

  action quiescent = all {
    MACHINES.forall(m => machines.get(m).pc == "done"),
    st' = st, machines' = machines, g' = g,
  }

  action step = any { machine, fault, quiescent }

  // ---- a narrower environment, for focused runs ---------------------------
  //
  // Random runs of `step` rarely line up a corrupt remote output with a
  // machine that must read it. `corruptionStep` keeps the remote reachable
  // and lets the adversary only corrupt remote outputs and evict local
  // ones, which is what drives a machine to read a corrupt blob, re-upload
  // it and decide whether to publish. Same model, same properties.

  pure val OUTPUTS = Set(11, 12, 21, 22)

  action reachableMachine = {
    nondet m = MACHINES.oneOf()
    nondet failing = Set(true, false).oneOf()
    nondet honours = Set(true, false).oneOf()
    val s = machines.get(m)
    val n = machineNext(st, g, m, s, true, failing, honours)
    all {
      s.pc != "done",
      st' = n.st,
      machines' = machines.set(m, n.s),
      g' = n.g,
    }
  }

  action damage = all {
    st.faults > 0,
    nondet d = OUTPUTS.oneOf()
    nondet m = MACHINES.oneOf()
    nondet kind = Set("corruptRemote", "evictLocal").oneOf()
    all {
      // Only damage that lands: a present remote output, a present local one.
      if (kind == "corruptRemote") st.remote.get(d) == OK else st.local.get(m).get(d) == OK,
      st' = applyFault(st, d, m, kind), machines' = machines, g' = g,
    }
  }

  action corruptionStep = any { reachableMachine, damage, quiescent }

  // `localFailureStep` spends the whole fault budget on local CAS writes
  // that fail while a fetched blob is being kept: the path where a remote
  // hit is served but must not be promoted.
  action localFailureStep = any { reachableMachine, quiescent }

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
