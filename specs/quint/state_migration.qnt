// -*- mode: Bluespec; -*-
//
// `state migrate`: moving a project's state from its legacy backend to the
// object store `migrateStateTo` names, and rolling it back, as the code
// implements it:
// - crates/cuenv/src/commands/infrastructure/migrate.rs: the manifest
//   (`_migration.json`) entries, `forward` and `rollback`.
// - crates/infrastructure/src/state/object/store/legacy_migration.rs: the
//   target's commit receipt, rollback lock, tombstone and release, each one
//   conditional write.
// - The source fence (`migrated-<id>`): the source lock, which every
//   ordinary source write needs, and which copy-back writes require.
//
// One tenant. Clients write through the backend their configuration names:
// the source while it is not fenced, the target unless the manifest lists
// the tenant with another status than migrated, while it is neither
// tombstoned nor locked by a migration. A client write is atomic
// here (acquire, write, release); the lock protocol is object_state_cas.qnt.
//
// Abstractions:
// - The operator runs forward, rollback or rollback with copy-back, in any
//   order and as often as it likes; each run may crash at any step, and any
//   store write may fail without applying or apply and answer with an error
//   (FAULTS). A run that sees an error stops; the next run starts over.
// - Clients write concurrently with the operator (FREEZE = false): the
//   model checks that the stores' locks alone keep them safe, without the
//   maintenance freeze `--confirm-maintenance` asks for.
// - A rerun's lock and writer nonces are fresh, as in the code.
//
// Check (TLC explores every reachable state of this instance):
//   quint verify state_migration.qnt --backend=tlc --tlc-config=tlc.json --invariants safety
//
// Results, 4 operator runs, 3 client writes, 2 faults, 1 crash: safety holds
// with clients writing during the migration (FREEZE = false); ~15K distinct
// states, seconds. A target that took writes, a completed rollback and a
// completed copy-back are all reachable. The model found no bug: an
// interrupted run leaves both stores protected, and reconciling one by hand
// is outside it.
//
// Mutation switches (constants below); each should make `safety` fail:
// - MOVED_CHECK = false: rollback without copy-back ignores writes made to
//   the target since its commit, and loses them.
// - TOMBSTONE = false: rollback does not tombstone the target. Once its
//   rollback lock is released, a target the manifest still lists as
//   migrated (the manifest is written last) accepts writes beside the
//   unfenced source.
// - SNAPSHOT_UNDER_FENCE = false: forward reads the source before fencing
//   it, so a write in between is not copied.
//
// This is model checking of a finite instance, not a proof for all sizes.
module state_migration {

  // ---- parameters ---------------------------------------------------------

  pure val RUNS = 4          // operator commands
  pure val WRITES = 3        // client writes
  pure val FAULTS = 2
  pure val CRASHES = 1

  // Mutation switches. The first value is the code as it is.
  pure val MOVED_CHECK = true
  pure val TOMBSTONE = true
  pure val SNAPSHOT_UNDER_FENCE = true
  pure val FREEZE = false

  // ---- state --------------------------------------------------------------

  type Source = { content: int, fence: bool }

  type Target = {
    exists: bool, content: int, serial: int,
    lock: str, lockRun: int,          // lock: "" | "migrator" | "rollback"
    committed: bool, commitSerial: int,
    tombstone: bool,
    preLock: int,                     // the rollback lock's pre-lock serial
  }

  // The tenant's `_migration.json` entry. status: "" | "pending" |
  // "migrated" | "rolledBack". evidence: the commit serial; 0 none.
  type Manifest = { status: str, evidence: int }

  type Op = {
    pc: str, cmd: str, runId: int,
    existing: int,                    // forward: the commit evidence found
    snap: int,                        // the content read for a copy
  }

  var src: Source
  var tgt: Target
  var man: Manifest
  var op: Op
  var written: int                    // ghost: client writes acknowledged
  var budget: { faults: int, crashes: int, runs: int, writes: int }

  pure val NO_TARGET: Target = {
    exists: false, content: 0, serial: 0, lock: "", lockRun: 0,
    committed: false, commitSerial: 0, tombstone: false, preLock: 0,
  }
  pure val IDLE: Op = { pc: "idle", cmd: "", runId: 0, existing: 0, snap: 0 }

  action init = all {
    src' = { content: 0, fence: false },
    tgt' = NO_TARGET,
    man' = { status: "", evidence: 0 },
    op' = IDLE,
    written' = 0,
    budget' = { faults: FAULTS, crashes: CRASHES, runs: RUNS, writes: WRITES },
  }

  // ---- who accepts writes -------------------------------------------------

  val srcAccepts = not(src.fence)
  // `refuse_unmigrated`: S3 state refuses a tenant the manifest lists with
  // any status but migrated. Creating fresh target state out of band (a
  // client pointed at the target before any migration) is out of scope:
  // forward then refuses a target of another origin.
  val tgtAccepts = and {
    man.status != "pending", man.status != "rolledBack",
    tgt.exists, not(tgt.tombstone), tgt.lock == "",
  }

  // ---- outcomes -----------------------------------------------------------

  // "ok", "err" (not applied, error) or "lost" (applied, error).
  pure val OUTCOMES = Set("ok", "err", "lost")
  pure def cost(outcome: str): int = if (outcome == "ok") 0 else 1
  pure def applies(outcome: str): bool = outcome != "err"

  pure def stop(o: Op): Op = IDLE

  // A conditional store write: whether `cond` held and `outcome` applied it.
  pure def lands(cond: bool, outcome: str): bool = cond and applies(outcome)
  // Whether the run goes on: it applied and the run saw it.
  pure def goesOn(cond: bool, outcome: str): bool = cond and outcome == "ok"

  // ---- clients ------------------------------------------------------------

  action clientWrite = all {
    budget.writes > 0,
    not(FREEZE) or op.pc == "idle",
    any {
      all { srcAccepts,
            src' = { ...src, content: written + 1 }, tgt' = tgt },
      all { tgtAccepts,
            tgt' = { ...tgt, content: written + 1, serial: tgt.serial + 1 }, src' = src },
    },
    written' = written + 1,
    budget' = { ...budget, writes: budget.writes - 1 },
    man' = man, op' = op,
  }

  // ---- the operator -------------------------------------------------------

  action opMove(o: Op, outcome: str): bool = all {
    op' = o,
    budget' = { ...budget, faults: budget.faults - cost(outcome) },
    written' = written,
  }

  action unchangedStores = all { src' = src, tgt' = tgt, man' = man }

  action operatorStep(outcome: str, cmd: str): bool = {
    val o = op
    val mine = tgt.lock != "" and tgt.lockRun == o.runId
    all {
      budget.faults >= cost(outcome),
      any {
        // Start a command.
        all { o.pc == "idle", budget.runs > 0, outcome == "ok",
          unchangedStores, written' = written,
          op' = { ...IDLE, pc: if (cmd == "forward") "fwStart" else "rbStart", cmd: cmd,
                  runId: RUNS - budget.runs + 1 },
          budget' = { ...budget, runs: budget.runs - 1 } },

        // ---- forward ----
        // migrate(): a rolled-back tenant, or a tombstoned target the
        // manifest does not list as rolled back, is refused; otherwise the
        // entry becomes pending unless it is migrated.
        all { o.pc == "fwStart", cmd == "",
          if (man.status == "rolledBack" or tgt.tombstone)
            all { unchangedStores, opMove(stop(o), outcome) }
          else all {
            src' = src, tgt' = tgt,
            man' = if (man.status != "migrated" and applies(outcome))
                     { status: "pending", evidence: 0 } else man,
            opMove(if (man.status == "migrated" or outcome == "ok")
                     { ...o, pc: if (SNAPSHOT_UNDER_FENCE) "fwTarget" else "fwEarlySnap" }
                   else stop(o), outcome) } },
        // The mutant: read the source before fencing it.
        all { o.pc == "fwEarlySnap", cmd == "", outcome == "ok", unchangedStores,
          opMove({ ...o, pc: "fwTarget", snap: src.content }, outcome) },
        // An existing target must be ours, unlocked and committed, with
        // the receipt the manifest recorded.
        all { o.pc == "fwTarget", cmd == "", outcome == "ok", unchangedStores,
          opMove(
            if (not(tgt.exists)) { ...o, pc: "fwFence", existing: 0 }
            else if (tgt.lock != "" or not(tgt.committed)) stop(o)
            else if (man.evidence != 0 and man.evidence != tgt.commitSerial) stop(o)
            else { ...o, pc: "fwFence", existing: tgt.commitSerial }, outcome) },
        // take_fence: the source lock, or the fence an earlier run took.
        all { o.pc == "fwFence", cmd == "",
          tgt' = tgt, man' = man,
          src' = { ...src, fence: src.fence or applies(outcome) },
          opMove(if (src.fence or outcome == "ok")
                   { ...o, pc: if (SNAPSHOT_UNDER_FENCE) "fwSnap" else "fwCopy" }
                 else stop(o), outcome) },
        // read_legacy_migration_snapshot, under the fence.
        all { o.pc == "fwSnap", cmd == "", outcome == "ok", unchangedStores,
          opMove({ ...o, pc: "fwCopy", snap: src.content }, outcome) },
        // An existing target is verified against the source; otherwise it
        // is created holding the copy and this run's migrator lock.
        all { o.pc == "fwCopy", cmd == "",
          if (o.existing != 0)
            all { unchangedStores,
                  opMove(if (outcome == "ok" and tgt.content == o.snap)
                           { ...o, pc: "fwManifest" } else stop(o), outcome) }
          else all {
            src' = src, man' = man,
            tgt' = if (lands(not(tgt.exists), outcome))
                     { ...NO_TARGET, exists: true, content: o.snap, serial: 1,
                       lock: "migrator", lockRun: o.runId }
                   else tgt,
            opMove(if (goesOn(not(tgt.exists), outcome)) { ...o, pc: "fwVerify" } else stop(o),
                   outcome) } },
        all { o.pc == "fwVerify", cmd == "", outcome == "ok", unchangedStores,
          opMove(if (tgt.exists and tgt.content == o.snap) { ...o, pc: "fwCommit" } else stop(o),
                 outcome) },
        // commit_legacy_migration: receipt and lock release in one write.
        all { o.pc == "fwCommit", cmd == "",
          val cond = tgt.exists and tgt.lock == "migrator" and tgt.lockRun == o.runId
          all {
            src' = src, man' = man,
            tgt' = if (lands(cond, outcome))
                     { ...tgt, lock: "", lockRun: 0, committed: true,
                       serial: tgt.serial + 1, commitSerial: tgt.serial + 1 }
                   else tgt,
            opMove(if (goesOn(cond, outcome)) { ...o, pc: "fwManifest", existing: tgt.serial + 1 }
                   else stop(o), outcome),
          } },
        all { o.pc == "fwManifest", cmd == "",
          src' = src, tgt' = tgt,
          man' = if (applies(outcome)) { status: "migrated", evidence: o.existing } else man,
          opMove(stop(o), outcome) },

        // ---- rollback ----
        // Needs a migrated entry with its receipt and the source fence.
        all { o.pc == "rbStart", cmd == "", outcome == "ok", unchangedStores,
          opMove(if (man.status == "migrated" and man.evidence != 0 and src.fence)
                   { ...o, pc: "rbLock" } else stop(o), outcome) },
        // lock_legacy_rollback: on the committed, unlocked target whose
        // receipt the manifest holds; records the pre-lock serial.
        all { o.pc == "rbLock", cmd == "",
          val cond = and { tgt.exists, tgt.committed, not(tgt.tombstone), tgt.lock == "",
                           tgt.commitSerial == man.evidence }
          all {
            src' = src, man' = man,
            tgt' = if (lands(cond, outcome))
                     { ...tgt, lock: "rollback", lockRun: o.runId, preLock: tgt.serial,
                       serial: tgt.serial + 1 }
                   else tgt,
            opMove(if (goesOn(cond, outcome)) { ...o, pc: "rbMoved" } else stop(o), outcome),
          } },
        // Changed since the commit: plain rollback releases its lock and
        // refuses; copy-back copies the target to the source first.
        all { o.pc == "rbMoved", cmd == "", outcome == "ok", unchangedStores,
          val moved = MOVED_CHECK and tgt.preLock != man.evidence
          opMove(if (not(moved)) { ...o, pc: "rbTombstone" }
                 else if (o.cmd == "copyBack") { ...o, pc: "rbCopy" }
                 else { ...o, pc: "rbRefuse" }, outcome) },
        all { o.pc == "rbRefuse", cmd == "",
          src' = src, man' = man,
          tgt' = if (lands(mine, outcome)) { ...tgt, lock: "", lockRun: 0, serial: tgt.serial + 1 }
                 else tgt,
          opMove(stop(o), outcome) },
        // legacy_rollback_snapshot, then the fenced source writes, then a
        // read-back compared with the snapshot.
        all { o.pc == "rbCopy", cmd == "",
          tgt' = tgt, man' = man,
          src' = if (lands(mine and src.fence, outcome)) { ...src, content: tgt.content } else src,
          opMove(if (goesOn(mine and src.fence, outcome)) { ...o, pc: "rbCopyVerify", snap: tgt.content }
                 else stop(o), outcome) },
        all { o.pc == "rbCopyVerify", cmd == "", outcome == "ok", unchangedStores,
          opMove(if (src.content == o.snap)
                   { ...o, pc: "rbTombstone" }
                 else stop(o), outcome) },
        all { o.pc == "rbTombstone", cmd == "",
          src' = src, man' = man,
          tgt' = if (lands(mine, outcome) and TOMBSTONE)
                   { ...tgt, tombstone: true, serial: tgt.serial + 1 } else tgt,
          opMove(if (goesOn(mine, outcome)) { ...o, pc: "rbUnfence" } else stop(o), outcome) },
        // unlock(fence): the source accepts writes again.
        all { o.pc == "rbUnfence", cmd == "",
          tgt' = tgt, man' = man,
          src' = if (lands(src.fence, outcome)) { ...src, fence: false } else src,
          opMove(if (goesOn(src.fence, outcome))
                   { ...o, pc: "rbRelease" }
                 else stop(o), outcome) },
        all { o.pc == "rbRelease", cmd == "",
          src' = src, man' = man,
          tgt' = if (lands(mine, outcome)) { ...tgt, lock: "", lockRun: 0, serial: tgt.serial + 1 }
                 else tgt,
          opMove(if (goesOn(mine, outcome)) { ...o, pc: "rbManifest" } else stop(o), outcome) },
        all { o.pc == "rbManifest", cmd == "",
          src' = src, tgt' = tgt,
          man' = if (applies(outcome)) { ...man, status: "rolledBack" } else man,
          opMove(stop(o), outcome) },
      },
    }
  }

  action crash = all {
    budget.crashes > 0, op.pc != "idle",
    op' = IDLE,
    budget' = { ...budget, crashes: budget.crashes - 1 },
    src' = src, tgt' = tgt, man' = man, written' = written,
  }

  action quiescent = all {
    op.pc == "idle", budget.runs == 0, budget.writes == 0,
    src' = src, tgt' = tgt, man' = man, op' = op, written' = written, budget' = budget,
  }

  action step = any {
    clientWrite,
    nondet outcome = OUTCOMES.oneOf()
    nondet cmd = Set("", "forward", "rollback", "copyBack").oneOf()
    all { (op.pc == "idle") == (cmd != ""), operatorStep(outcome, cmd) },
    crash,
    quiescent,
  }

  // ---- properties ---------------------------------------------------------

  // Never two stores accepting writes: no split brain.
  val singleWriter = not(srcAccepts and tgtAccepts)

  // No acknowledged write is lost: a store that accepts writes holds the
  // last one (both start empty, at 0).
  val noLostWrite = and {
    srcAccepts implies src.content == written,
    tgtAccepts implies tgt.content == written,
  }

  val safety = and { singleWriter, noLostWrite }

  // Reachability, expected to FAIL: the model reaches a migrated target that
  // took writes, a completed rollback and a completed copy-back.
  val neverMigratedWrite = not(tgtAccepts and written > 0 and tgt.serial > tgt.commitSerial)
  val neverRolledBack = man.status != "rolledBack"
  val neverCopiedBack = not(man.status == "rolledBack" and src.content > 0 and tgt.serial > tgt.commitSerial + 3)
}
