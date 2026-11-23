// -*- mode: Bluespec; -*-
//
// Provider changes whose outcome the state store may not learn: in-flight
// markers, unrecorded-change files and their recovery, as the code
// implements them:
// - crates/infrastructure/src/engine/infrastructure_engine.rs: apply step
//   (marker put -> ApplyResourceChange -> record_change or forget -> marker
//   delete), plan and require_current refusing while a marker exists.
// - crates/infrastructure/src/unrecorded.rs: save on a failed record write,
//   planning refused while this machine holds a file, `state recover`
//   (put_if_unchanged, is_recorded, then marker delete, then file removal).
// - crates/infrastructure/src/in_flight.rs: one marker per address.
//
// One resource address, desired to exist (an "ensure" run) or not (a
// "destroy" run). Two machines share one state backend; each keeps its own
// unrecorded-change directory. A machine runs RUNS times; a run that holds an
// unrecorded file runs `state recover` instead of a plan.
//
// Abstractions:
// - The lock and every store write are atomic and fenced: a write applies
//   only while the writer holds the lock. object_state_cas.qnt checks that
//   the object store gives this.
// - A store write may fail (outage) without applying, or fail after
//   applying (a lost response). A provider call may apply and answer,
//   apply and lose its answer, or not apply. FAULTS bounds these failures.
// - A run may crash at any step (CRASHES), leaving its lock held. An
//   operator may force-unlock once, and may resolve a marker once by hand,
//   as the error message tells them to: inspect the real object, import it
//   if the state lacks it, otherwise remove the marker.
// - Plans refresh: a record whose object is gone is replanned, so a stale
//   record is harmless; a live object the state does not know is not.
// - With LATE_LANDING = false, a provider call that returned an error has
//   either applied or not by the time the run moves on.
//
// Check (TLC explores every reachable state of this instance):
//   quint verify infrastructure_changes.qnt --backend=tlc --tlc-config=tlc.json --invariants safety
//
// Results, 2 machines, 2 runs each, 3 faults, 1 crash: safety holds; ~455K
// distinct states, under a minute. Recovery, the operator's resolution and
// an unsaved change (UnrecordedChangeLost) are all reachable.
//
// Found by this model: `state recover` deleted whatever marker the address
// had. A file saved by run A (create, expected absent), the object imported
// by hand, destroyed, then created again by run B whose answer was lost:
// recovering A's file matched "absent", wrote A's stale record and deleted
// B's marker, so the next plan created the object a second time. Files now
// name their run's lock, and recovery refuses while another run's marker
// exists (OWN_MARKER_ONLY = false restores the bug).
//
// Mutation switches (constants below); each should make `safety` fail:
// - MARKER = "afterCall": write the marker after the provider call. A crash
//   between them leaves a live object nobody knows about.
// - MARKER = "none": no marker. A failed record write leaves the object
//   known only to one machine's file; the other machine creates a second.
// - PLAN_CHECKS_MARKERS = false: plans ignore markers, as above.
// - RECOVER = "markerFirst": recovery deletes the marker before writing the
//   record; a failed record write then strands the object in a file.
// - OWN_MARKER_ONLY = false: the bug above.
// Characterisation, expected to FAIL and not checked in CI:
// - FORCE_LIVE = true: force-unlocking the lock of a run that is still
//   alive. Fencing stops its later store writes, but not its provider call:
//   the operator can resolve the marker while the call is still running.
// - LATE_LANDING = true: a provider operation that lands after its run gave
//   up, and after the operator resolved the marker, is an orphan. Operators
//   must make sure the provider has settled before removing a marker.
//
// This is model checking of a finite instance, not a proof for all sizes.
module infrastructure_changes {

  // ---- parameters ---------------------------------------------------------

  pure val MACHINES = Set(1, 2)
  pure val RUNS = 2
  pure val FAULTS = 3
  pure val CRASHES = 1

  // Mutation switches. The first value is the code as it is.
  pure val MARKER = "beforeCall"       // "beforeCall" | "afterCall" | "none"
  pure val PLAN_CHECKS_MARKERS = true
  pure val RECOVER = "recordFirst"     // "recordFirst" | "markerFirst"
  pure val LATE_LANDING = false
  // Recovery leaves a marker another run wrote alone, and refuses a file
  // while one exists. false: it deletes whatever marker the address has.
  pure val OWN_MARKER_ONLY = true
  pure val FORCE_LIVE = false

  // ---- types --------------------------------------------------------------

  // The stored record of the address. obj == 0: no record.
  type Record = { obj: int, gen: int, serial: int }

  // A saved unrecorded change: the record and the version it replaces.
  // `lock`: the lock of the run that saved it.
  type File = { obj: int, gen: int, expGen: int, expSerial: int, lock: int }

  type Run = {
    pc: str, round: int, kind: str,
    obj: int,                  // the object this run's create makes
    expGen: int, expSerial: int, newGen: int,
    fileLock: int,             // `state recover`: the lock that saved the file
  }

  // A provider operation still on its way (LATE_LANDING only).
  type Late = { create: bool, obj: int }

  pure val NO_RECORD: Record = { obj: 0, gen: 0, serial: 0 }
  pure val OPERATOR_LOCK = 999
  pure val OPERATOR_GEN = 999

  pure def lockOf(m: int, r: Run): int = m * 10 + r.round
  // A fresh object id and a fresh generation per run.
  pure def objOf(m: int, r: Run): int = m * 10 + r.round
  pure def genOf(m: int, r: Run): int = 100 + m * 10 + r.round

  // ConditionalPut::is_recorded.
  pure def isRecorded(f: File, s: Record): bool = and {
    s.obj != 0,
    s.gen == f.gen,
    s.obj == f.obj,
    if (f.expGen == 0) s.serial == 1 else s.serial == f.expSerial + 1,
  }

  // RecordVersion::of(stored) == expected.
  pure def unchanged(s: Record, expGen: int, expSerial: int): bool =
    if (expGen == 0) s.obj == 0 else (s.gen == expGen and s.serial == expSerial)

  // ---- state --------------------------------------------------------------

  var world: Set[int]          // live provider objects for the address
  var late: Set[Late]
  var holder: int              // the lock in the store; 0: free
  var record: Record
  var marker: int              // the lock that wrote the marker; 0: none
  var files: int -> Set[File]  // machine -> its unrecorded-change files
  var runs: int -> Run
  var budget: { faults: int, crashes: int, unlocks: int, resolves: int }

  action init = all {
    world' = Set(),
    late' = Set(),
    holder' = 0,
    record' = NO_RECORD,
    marker' = 0,
    files' = MACHINES.mapBy(_ => Set()),
    runs' = MACHINES.mapBy(_ => { pc: "idle", round: 1, kind: "", obj: 0,
                                  expGen: 0, expSerial: 0, newGen: 0, fileLock: 0 }),
    budget' = { faults: FAULTS, crashes: CRASHES, unlocks: 1, resolves: 1 },
  }

  // ---- outcomes -----------------------------------------------------------

  // What a store write or provider call did: "ok", "err" (not applied) or
  // "lost" (applied, but the caller saw an error). Failures spend a fault.
  pure val OUTCOMES = Set("ok", "err", "lost")
  pure def cost(outcome: str): int = if (outcome == "ok") 0 else 1
  pure def applies(outcome: str): bool = outcome != "err"

  // ---- one machine step ---------------------------------------------------

  action move(m: int, r: Run, outcome: str): bool = all {
    runs' = runs.set(m, r),
    budget' = { ...budget, faults: budget.faults - cost(outcome) },
  }

  pure def finish(r: Run): Run = { ...r, pc: "release" }

  action keep(unused: bool): bool = all {
    world' = world, late' = late, holder' = holder, record' = record,
    marker' = marker, files' = files,
  }

  action machineStep(m: int, outcome: str, kind: str): bool = {
    val r = runs.get(m)
    val mine = lockOf(m, r)
    val fenced = holder == mine
    val writes = fenced and applies(outcome)
    all {
      budget.faults >= cost(outcome),
      any {
        // Start a run, or stop after RUNS.
        all { r.pc == "idle", r.round > RUNS, outcome == "ok", kind == "ensure",
          keep(true), move(m, { ...r, pc: "done" }, outcome) },
        all { r.pc == "idle", r.round <= RUNS, outcome == "ok",
          keep(true), move(m, { ...r, pc: "acquire", kind: kind }, outcome) },

        // acquire_lock (atomic; a held lock is `Locked` and the run stops)
        all { r.pc == "acquire", outcome == "ok", kind == "",
          if (holder == 0)
            all { holder' = mine, world' = world, late' = late, record' = record,
                  marker' = marker, files' = files,
                  move(m, { ...r, pc: "check" }, outcome) }
          else
            all { keep(true), move(m, { ...r, pc: "idle", round: r.round + 1 }, outcome) } },

        // A machine with unrecorded files runs `state recover`; otherwise
        // plan: refuse while a change is in flight, then refresh and diff.
        all { r.pc == "check", outcome == "ok", kind == "", keep(true),
          if (files.get(m) != Set())
            move(m, { ...r, pc: if (RECOVER == "recordFirst") "recRecord" else "recMarker" }, outcome)
          else if (PLAN_CHECKS_MARKERS and marker != 0)
            move(m, finish(r), outcome)
          else if (r.kind == "ensure" and (record.obj == 0 or not(world.contains(record.obj))))
            move(m, { ...r, pc: if (MARKER == "beforeCall") "markerPut" else "call",
                      obj: objOf(m, r),
                      expGen: record.gen, expSerial: record.serial,
                      newGen: if (record.obj == 0) genOf(m, r) else record.gen }, outcome)
          else if (r.kind == "destroy" and record.obj != 0 and world.contains(record.obj))
            move(m, { ...r, pc: if (MARKER == "beforeCall") "markerPut" else "call",
                      obj: record.obj }, outcome)
          else if (r.kind == "destroy" and record.obj != 0)
            move(m, { ...r, pc: "forget" }, outcome)
          else
            move(m, finish(r), outcome) },

        // store.put(marker); an error ends the run.
        all { r.pc == "markerPut",
          world' = world, late' = late, holder' = holder, record' = record, files' = files,
          marker' = if (writes) mine else marker,
          move(m, if (fenced and outcome == "ok")
                    { ...r, pc: if (MARKER == "afterCall") (if (r.kind == "ensure") "record" else "forget")
                                else "call" }
                  else finish(r), outcome) },

        // ApplyResourceChange. An error (lost answer or not applied) ends the
        // run with the marker in place.
        all { r.pc == "call", kind == "",
          val create = r.kind == "ensure"
          val landsNow = applies(outcome)
          all {
            world' = if (not(landsNow)) world
                     else if (create) world.union(Set(r.obj)) else world.exclude(Set(r.obj)),
            late' = late, holder' = holder, record' = record, marker' = marker, files' = files,
            move(m, if (outcome == "ok")
                      { ...r, pc: if (MARKER == "afterCall") "markerPut"
                                  else if (create) "record" else "forget" }
                    else finish(r), outcome),
          } },
        // The same call, answered with an error, landing later.
        all { r.pc == "call", LATE_LANDING, kind == "late", outcome == "err",
          world' = world, holder' = holder, record' = record, marker' = marker, files' = files,
          late' = late.union(Set({ create: r.kind == "ensure", obj: r.obj })),
          move(m, finish(r), outcome) },

        // record_change: put_if_unchanged, else save the change to a file.
        // kind == "unsaved": saving the file failed too (UnrecordedChangeLost).
        all { r.pc == "record", kind == "" or kind == "unsaved",
          val ok = unchanged(record, r.expGen, r.expSerial)
          val f = { obj: r.obj, gen: r.newGen, expGen: r.expGen, expSerial: r.expSerial, lock: mine }
          val saved = not(fenced and ok and outcome == "ok") and kind == ""
          all {
            kind == "" or budget.faults >= cost(outcome) + 1,
            record' = if (writes and ok) { obj: r.obj, gen: r.newGen, serial: record.serial + 1 }
                      else record,
            files' = if (saved) files.set(m, files.get(m).union(Set(f))) else files,
            world' = world, late' = late, holder' = holder, marker' = marker,
            runs' = runs.set(m,
              if (fenced and ok and outcome == "ok")
                { ...r, pc: "markerDel" }
              else finish(r)),
            budget' = { ...budget, faults: budget.faults - cost(outcome) - (if (kind == "unsaved") 1 else 0) },
          } },

        // forget: the provider deleted the object; delete the record.
        all { r.pc == "forget", kind == "",
          world' = world, late' = late, holder' = holder, marker' = marker, files' = files,
          record' = if (writes) NO_RECORD else record,
          move(m, if (fenced and outcome == "ok")
                    { ...r, pc: "markerDel" }
                  else finish(r), outcome) },

        // The outcome is recorded: delete the marker.
        all { r.pc == "markerDel", kind == "",
          world' = world, late' = late, holder' = holder, record' = record, files' = files,
          marker' = if (writes) 0 else marker,
          move(m, finish(r), outcome) },

        // `state recover`, one file: put_if_unchanged; a changed record that
        // already holds this change counts as recorded; any other is refused
        // and the file stays.
        all { r.pc == "recRecord", kind == "",
          nondet f = files.get(m).oneOf()
          val ok = unchanged(record, f.expGen, f.expSerial)
          val already = isRecorded(f, record)
          val foreign = OWN_MARKER_ONLY and marker != 0 and marker != f.lock
          all {
            world' = world, late' = late, holder' = holder, marker' = marker, files' = files,
            record' = if (writes and ok and not(foreign)) { obj: f.obj, gen: f.gen, serial: record.serial + 1 }
                      else record,
            move(m, if (fenced and outcome == "ok" and (ok or already) and not(foreign))
                      { ...r, pc: if (RECOVER == "recordFirst") "recMarker" else "recRemove",
                        obj: f.obj, fileLock: f.lock }
                    else finish(r), outcome),
          } },
        all { r.pc == "recMarker", kind == "",
          world' = world, late' = late, holder' = holder, record' = record, files' = files,
          marker' = if (writes and (marker == r.fileLock or not(OWN_MARKER_ONLY) or RECOVER == "markerFirst")) 0 else marker,
          move(m, if (fenced and outcome == "ok")
                    { ...r, pc: if (RECOVER == "recordFirst") "recRemove" else "recRecord" }
                  else finish(r), outcome) },
        all { r.pc == "recRemove", outcome == "ok", kind == "",
          world' = world, late' = late, holder' = holder, record' = record, marker' = marker,
          files' = files.set(m, files.get(m).filter(f => f.obj != r.obj)),
          move(m, finish(r), outcome) },

        // unlock; a failed release leaves the lock held.
        all { r.pc == "release", kind == "",
          world' = world, late' = late, record' = record, marker' = marker, files' = files,
          holder' = if (fenced and applies(outcome)) 0 else holder,
          move(m, { ...r, pc: "idle", round: r.round + 1 }, outcome) },
      },
    }
  }

  // ---- crashes, the operator and the provider -----------------------------

  // The process dies: no file is saved, the lock stays held.
  action crash(m: int): bool = {
    val r = runs.get(m)
    all {
      budget.crashes > 0,
      not(Set("idle", "done").contains(r.pc)),
      runs' = runs.set(m, { ...r, pc: "idle", round: r.round + 1 }),
      budget' = { ...budget, crashes: budget.crashes - 1 },
      world' = world, late' = late, holder' = holder, record' = record,
      marker' = marker, files' = files,
    }
  }

  // `state unlock --force` on a lock whose run has ended (crashed, or
  // failed to release it). With FORCE_LIVE, on any lock.
  action forceUnlock = all {
    budget.unlocks > 0, holder != 0,
    FORCE_LIVE or not(MACHINES.exists(m => and {
      lockOf(m, runs.get(m)) == holder, not(Set("idle", "done").contains(runs.get(m).pc)),
    })),
    holder' = 0,
    budget' = { ...budget, unlocks: budget.unlocks - 1 },
    world' = world, late' = late, record' = record, marker' = marker,
    files' = files, runs' = runs,
  }

  // The operator resolves the change in flight by hand, under the lock:
  // imports a live object the state lacks, else removes the marker.
  action resolve = all {
    budget.resolves > 0, marker != 0, holder == 0,
    val unknown = world.filter(x => x != record.obj)
    all {
      record' = if (unknown != Set())
                  { obj: unknown.fold(0, (a, x) => if (x > a) x else a),
                    gen: OPERATOR_GEN, serial: record.serial + 1 }
                else record,
      marker' = 0,
      budget' = { ...budget, resolves: budget.resolves - 1 },
      world' = world, late' = late, holder' = holder, files' = files, runs' = runs,
    } }

  // A provider operation that answered with an error lands after all.
  action land = all {
    late != Set(),
    nondet op = late.oneOf()
    all {
      world' = if (op.create) world.union(Set(op.obj)) else world.exclude(Set(op.obj)),
      late' = late.exclude(Set(op)),
    },
    holder' = holder, record' = record, marker' = marker, files' = files,
    runs' = runs, budget' = budget,
  }

  action quiescent = all {
    MACHINES.forall(m => runs.get(m).pc == "done"),
    world' = world, late' = late, holder' = holder, record' = record,
    marker' = marker, files' = files, runs' = runs, budget' = budget,
  }

  action step = any {
    nondet m = MACHINES.oneOf()
    nondet outcome = OUTCOMES.oneOf()
    nondet kind = Set("", "late", "unsaved", "ensure", "destroy").oneOf()
    any {
      all { runs.get(m).pc == "idle", Set("ensure", "destroy").contains(kind),
            machineStep(m, outcome, kind) },
      all { runs.get(m).pc != "idle", machineStep(m, outcome, kind) },
      crash(m),
    },
    forceUnlock,
    resolve,
    land,
    quiescent,
  }

  // ---- properties ---------------------------------------------------------

  // A live object the state may not know about is accounted for: it is the
  // stored record, or a marker still blocks every plan, or the run that
  // created it is still on its way to recording it (or, with MARKER =
  // "afterCall", to writing its marker).
  val noOrphan = world.forall(x => or {
    record.obj == x,
    marker != 0,
    MACHINES.exists(m => and {
      runs.get(m).kind == "ensure", runs.get(m).obj == x,
      Set("record", "markerPut").contains(runs.get(m).pc),
    }),
  })

  // The address never has two live objects: no change is repeated against
  // a state that does not know about the first.
  val noDuplicate = world.size() <= 1

  val safety = and { noOrphan, noDuplicate }

  // Reachability, expected to FAIL: shows the model reaches the paths the
  // invariants are about.
  val neverRecovered = MACHINES.forall(m => runs.get(m).pc != "recRemove")
  val neverResolved = budget.resolves > 0
  val neverUnsaved = not(MACHINES.exists(m => runs.get(m).pc == "release" and runs.get(m).kind == "ensure"
                                             and marker != 0 and world.contains(runs.get(m).obj)
                                             and files.get(m) == Set()))
}
