// -*- mode: Bluespec; -*-
//
// Cross-process coordination of hook runs in one directory, as the code
// implements it:
// - crates/hooks/src/state/coordinator.rs: begin_run_with_mode,
//   join_supervisor_run, spawn_run_child, record_child, update_run,
//   publish_and_serve, fail_start, request_cancellation,
//   acknowledge_cancellation, recover_drained, fast_status.
// - crates/cuenv/src/hook_supervisor.rs and
//   crates/hooks/src/executor/hook_executor.rs: who calls them, and when.
//
// One directory and one shell session; RUNS runs of the same hook
// configuration, so they share one state file (it is keyed by the instance
// hash) and are told apart by their run identifier. Every transaction holds
// the global state lock, so transactions are serialised, but a transaction
// writes several files one after another and its process can die between
// any two of them: the lock is released and the next transaction sees the
// files as they were left. A worker can die at any point; its child (the
// hook process) lives on until it exits. Recovery runs only on the
// operator's attestation that the run's processes are gone, so it requires
// that here.
//
// Not modelled: broker leases and namespaces beyond the lease file,
// legacy records, payload files, PID reuse.
//
// Check (TLC explores every reachable state of this instance):
//   quint verify hook_runs.qnt --backend=tlc --tlc-config=tlc.json --invariants safety
//
// Mutation switches (constants below); each should make `safety` fail:
// - MARK_CHILD_STARTING = false: the child is spawned before the
//   reservation says so; a worker that dies right after spawning leaves a
//   child nobody knows about, and a new run starts beside it.
// - BUSY_CHECK = false: a run starts while another reserves the directory.
// - CHILD_BEFORE_FINISH = false: a run finishes with its child alive.
// - PUBLISH_CHECKS_CANCEL = false: a run publishes "completed" after its
//   cancellation was requested.
//
// update_run's invalidation of the run's own generation on failure has no
// switch: a run's generation exists only once it published, which ends the
// run, so in this model it never finds one.
//
// This is model checking of a finite instance, not a proof for all sizes.
module hook_runs {

  // ---- parameters ---------------------------------------------------------

  pure val RUNS = Set(1, 2, 3)
  pure val CRASHES = 2

  // Mutation switches. The first value is the code as it is.
  pure val MARK_CHILD_STARTING = true
  pure val BUSY_CHECK = true
  pure val CHILD_BEFORE_FINISH = true
  pure val PUBLISH_CHECKS_CANCEL = true

  // ---- types --------------------------------------------------------------

  // The directory's reservation. run == 0: none.
  // phase: "starting" | "running" | "childStarting" | "cancelling"
  type Res = { owner: int, phase: str, worker: bool, child: bool }
  pure val NO_RES: Res = { owner: 0, phase: "", worker: false, child: false }

  // The shared state file. run == 0: none.
  // status: "running" | "cancelling" | "completed" | "failed" | "cancelled"
  type St = { owner: int, status: str }

  // pc: "idle" | "launched" | "joined" | "child" | "reap" | "finish"
  //   | "failing" | "cancel" | "ack" | "done" | "dead"
  type Proc = { pc: str, failHooks: bool }

  // ---- state --------------------------------------------------------------

  var res: Res
  var st: St
  var marker: int           // the session's marker: a run, or 0
  var gen: int              // the session's completed generation: a run, or 0
  var lease: int -> bool    // broker lease of a published run
  var procs: int -> Proc    // the starter and then the worker of each run
  var childAlive: int -> bool
  var crashes: int
  // ghost
  var hooksDone: int -> bool      // every hook of the run ran and succeeded
  var cancelAsked: int -> bool    // a cancellation of the run was accepted
  var lie: bool                   // a start or status claimed a completion that did not happen

  action init = all {
    res' = NO_RES,
    st' = NO_ST,
    marker' = 0,
    gen' = 0,
    lease' = RUNS.mapBy(_ => false),
    procs' = RUNS.mapBy(_ => { pc: "idle", failHooks: false }),
    childAlive' = RUNS.mapBy(_ => false),
    crashes' = CRASHES,
    hooksDone' = RUNS.mapBy(_ => false),
    cancelAsked' = RUNS.mapBy(_ => false),
    lie' = false,
  }

  pure def terminal(s: str): bool = s == "completed" or s == "failed" or s == "cancelled"
  pure def mine(r: Res, rid: int): bool = r.owner == rid

  // ---- a transaction: a list of file writes, cut short by a crash ---------
  //
  // Each write is one step of a transaction. `upto` is how many of them
  // land; if fewer than all, the process died (one of CRASHES).

  type Files = { res: Res, st: St, marker: int, gen: int, lease: int -> bool }

  pure def files(r: Res, s: St, m: int, g: int, l: int -> bool): Files =
    { res: r, st: s, marker: m, gen: g, lease: l }

  // One file write: kind "res" | "st" | "marker" | "gen" | "lease"
  // | "invalidate" (the generation, if it names `n`) | "dropGen".
  type Write = { k: str, res: Res, st: St, n: int }
  pure val NO_ST: St = { owner: 0, status: "" }

  pure def apply(f: Files, w: Write): Files =
    if (w.k == "res") { ...f, res: w.res }
    else if (w.k == "st") { ...f, st: w.st }
    else if (w.k == "marker") { ...f, marker: w.n }
    else if (w.k == "gen") { ...f, gen: w.n }
    else if (w.k == "lease") { ...f, lease: f.lease.set(w.n, true) }
    else if (w.k == "invalidate") (if (f.gen == w.n) { ...f, gen: 0 } else f)
    else if (w.k == "dropGen") { ...f, gen: 0 }
    else f

  action commit(rid: int, ws: List[Write], after: Proc): bool = {
    val f0 = files(res, st, marker, gen, lease)
    nondet upto = 0.to(ws.length()).oneOf()
    val died = upto < ws.length()
    val f = ws.slice(0, upto).foldl(f0, (acc, w) => apply(acc, w))
    all {
      not(died) or crashes > 0,
      res' = f.res, st' = f.st, marker' = f.marker, gen' = f.gen, lease' = f.lease,
      procs' = procs.set(rid, if (died) { ...procs.get(rid), pc: "dead" } else after),
      crashes' = if (died) crashes - 1 else crashes,
    }
  }

  action keepGhosts = all {
    hooksDone' = hooksDone, cancelAsked' = cancelAsked, lie' = lie,
  }

  action unchangedFiles = all {
    res' = res, st' = st, marker' = marker, gen' = gen, lease' = lease,
  }

  // A check failed: the operation errors and its process exits.
  action refuse(rid: int): bool = all {
    unchangedFiles,
    procs' = procs.set(rid, { ...procs.get(rid), pc: "dead" }),
    childAlive' = childAlive, crashes' = crashes, keepGhosts,
  }

  action setProc(rid: int, p: Proc): bool = all {
    unchangedFiles,
    procs' = procs.set(rid, p),
    childAlive' = childAlive, crashes' = crashes, keepGhosts,
  }

  // ---- writes -------------------------------------------------------------

  pure def wRes(r: Res): Write = { k: "res", res: r, st: NO_ST, n: 0 }
  pure def wSt(x: St): Write = { k: "st", res: NO_RES, st: x, n: 0 }
  pure def wMarker(m: int): Write = { k: "marker", res: NO_RES, st: NO_ST, n: m }
  pure def wGen(g: int): Write = { k: "gen", res: NO_RES, st: NO_ST, n: g }
  pure def wLease(rid: int): Write = { k: "lease", res: NO_RES, st: NO_ST, n: rid }
  // invalidate_generation: remove the generation if it names this run.
  pure def wInvalidate(rid: int): Write = { k: "invalidate", res: NO_RES, st: NO_ST, n: rid }
  // invalidate_generation of the previous run, by begin_run.
  pure val wDropGen: Write = { k: "dropGen", res: NO_RES, st: NO_ST, n: 0 }

  // ---- the starter (the shell or CI) --------------------------------------

  // begin_run_with_mode, then launch the supervisor (or fail_start).
  action begin(rid: int): bool = {
    val p = procs.get(rid)
    nondet force = Set(true, false).oneOf()
    nondet failHooks = Set(true, false).oneOf()
    val busy = res.owner != 0
    val previous = gen != 0
    val consistent = not(previous) or (st.owner == gen and st.status == "completed")
    all {
      p.pc == "idle",
      // Runs start in order: a run's identifier is fresh.
      RUNS.forall(o => o < rid implies procs.get(o).pc != "idle"),
      if (busy and BUSY_CHECK) refuse(rid)
      else if (not(consistent)) refuse(rid)
      else if (previous and not(force)) all {
        // IfMissing and the same configuration: AlreadyPresent.
        lie' = (lie or not(hooksDone.get(gen))),
        unchangedFiles,
        procs' = procs.set(rid, { ...p, pc: "done" }),
        childAlive' = childAlive, crashes' = crashes,
        hooksDone' = hooksDone, cancelAsked' = cancelAsked,
      }
      else all {
        commit(rid, [
          wDropGen,
          wRes({ owner: rid, phase: "starting", worker: false, child: false }),
          wMarker(rid),
          wSt({ owner: rid, status: "running" }),
        ].slice(if (previous) 0 else 1, 4), { pc: "launched", failHooks: failHooks }),
        childAlive' = childAlive, keepGhosts,
      },
    }
  }

  // The supervisor could not be started: fail_start.
  action failStart(rid: int): bool = all {
    procs.get(rid).pc == "launched",
    if (not(mine(res, rid))) setProc(rid, { ...procs.get(rid), pc: "done" })
    else if (res.phase == "cancelling") ackCancel(rid)
    else if (res.phase != "starting" or res.worker or res.child or st.owner != rid) refuse(rid)
    else all {
      commit(rid, [wSt({ owner: rid, status: "failed" }), wRes(NO_RES)],
             { ...procs.get(rid), pc: "done" }),
      childAlive' = childAlive, keepGhosts,
    },
  }

  // ---- the supervisor (worker) --------------------------------------------

  // join_supervisor_run
  action join(rid: int): bool = all {
    procs.get(rid).pc == "launched",
    if (not(mine(res, rid))) setProc(rid, { ...procs.get(rid), pc: "done" })
    else if (res.phase == "cancelling") setProc(rid, { ...procs.get(rid), pc: "ack" })
    else if (res.phase != "starting" or st.owner != rid) refuse(rid)
    else all {
      commit(rid, [wRes({ ...res, phase: "running", worker: true })],
             { ...procs.get(rid), pc: "joined" }),
      childAlive' = childAlive, keepGhosts,
    },
  }

  // spawn_run_child: mark ChildStarting, spawn, record the child.
  action spawnChild(rid: int): bool = {
    val p = procs.get(rid)
    nondet upto = 0.to(3).oneOf()  // 0: nothing; 1: marked; 2: spawned; 3: recorded
    val died = upto < 3
    val marked = { ...res, phase: "childStarting" }
    val recorded = { ...res, phase: "running", child: true }
    all {
      p.pc == "joined",
      if (not(mine(res, rid))) setProc(rid, { ...p, pc: "done" })
      else if (res.phase == "cancelling") setProc(rid, { ...p, pc: "cancel" })
      else if (res.phase != "running" or not(res.worker) or res.child or st.owner != rid) refuse(rid)
      else all {
        not(died) or crashes > 0,
        res' =
          if (MARK_CHILD_STARTING)
            (if (upto == 0) res else if (upto < 3) marked else recorded)
          else (if (upto < 3) res else recorded),
        st' = st, marker' = marker, gen' = gen, lease' = lease,
        childAlive' = childAlive.set(rid, upto >= 2),
        procs' = procs.set(rid, if (died) { ...p, pc: "dead" } else { ...p, pc: "child" }),
        crashes' = if (died) crashes - 1 else crashes,
        keepGhosts,
      },
    }
  }

  // The hook process exits on its own.
  action childExits(rid: int): bool = all {
    childAlive.get(rid),
    childAlive' = childAlive.set(rid, false),
    unchangedFiles, procs' = procs, crashes' = crashes,
    hooksDone' = hooksDone.set(rid,
      procs.get(rid).pc == "child" and not(procs.get(rid).failHooks)),
    cancelAsked' = cancelAsked, lie' = lie,
  }

  // The worker waits for its child; on cancellation it kills it first.
  action waitChild(rid: int): bool = all {
    procs.get(rid).pc == "child",
    not(childAlive.get(rid)),
    setProc(rid, { ...procs.get(rid), pc: "reap" }),
  }

  // Waiting for, stopping or recording the child fails (an I/O error): the
  // worker records a failure without having reaped it.
  action abandonChild(rid: int): bool = all {
    procs.get(rid).pc == "child",
    setProc(rid, { ...procs.get(rid), pc: "failing" }),
  }

  // The worker sees a cancellation (run_status) and kills its child.
  action seeCancel(rid: int): bool = all {
    procs.get(rid).pc == "child",
    mine(res, rid), res.phase == "cancelling",
    childAlive' = childAlive.set(rid, false),
    unchangedFiles, procs' = procs.set(rid, { ...procs.get(rid), pc: "reap" }),
    crashes' = crashes, keepGhosts,
  }

  // record_child(Reaped)
  action reap(rid: int): bool = all {
    procs.get(rid).pc == "reap",
    if (not(mine(res, rid))) setProc(rid, { ...procs.get(rid), pc: "done" })
    else if (not(res.child)) refuse(rid)
    else all {
      commit(rid, [wRes({ ...res, child: false })],
             { ...procs.get(rid),
               pc: if (res.phase == "cancelling") "ack"
                   else if (procs.get(rid).failHooks) "failing" else "finish" }),
      childAlive' = childAlive, keepGhosts,
    },
  }

  // publish_and_serve
  action publish(rid: int): bool = all {
    procs.get(rid).pc == "finish",
    if (not(mine(res, rid))) setProc(rid, { ...procs.get(rid), pc: "done" })
    else if (res.phase == "cancelling" and PUBLISH_CHECKS_CANCEL) setProc(rid, { ...procs.get(rid), pc: "ack" })
    else if (not(res.phase == "running" or res.phase == "cancelling") or not(res.worker)
             or (res.child and CHILD_BEFORE_FINISH)
             or st.owner != rid or terminal(st.status)) refuse(rid)
    else all {
      commit(rid, [wLease(rid), wGen(rid), wSt({ owner: rid, status: "completed" }), wRes(NO_RES)],
             { ...procs.get(rid), pc: "done" }),
      childAlive' = childAlive, keepGhosts,
    },
  }

  // update_run with a failed terminal state (record_failure).
  action finishFailed(rid: int): bool = all {
    procs.get(rid).pc == "failing",
    if (not(mine(res, rid))) setProc(rid, { ...procs.get(rid), pc: "done" })
    else if (res.phase == "cancelling") setProc(rid, { ...procs.get(rid), pc: "ack" })
    else if (res.phase != "running" or not(res.worker) or (res.child and CHILD_BEFORE_FINISH)
             or st.owner != rid or terminal(st.status)) refuse(rid)
    else all {
      commit(rid,
        [wInvalidate(rid), wSt({ owner: rid, status: "failed" }), wRes(NO_RES)],
        { ...procs.get(rid), pc: "done" }),
      childAlive' = childAlive, keepGhosts,
    },
  }

  // acknowledge_cancellation
  action ackCancel(rid: int): bool = all {
    if (not(mine(res, rid))) setProc(rid, { ...procs.get(rid), pc: "done" })
    else if (res.phase != "cancelling" or res.child or st.owner != rid) refuse(rid)
    else all {
      commit(rid, [wSt({ owner: rid, status: "cancelled" }), wRes(NO_RES)],
             { ...procs.get(rid), pc: "done" }),
      childAlive' = childAlive, keepGhosts,
    },
  }

  action ack(rid: int): bool = all { procs.get(rid).pc == "ack", ackCancel(rid) }

  // A worker or starter dies outside a transaction.
  action die(rid: int): bool = all {
    crashes > 0,
    not(procs.get(rid).pc == "done" or procs.get(rid).pc == "dead" or procs.get(rid).pc == "idle"),
    unchangedFiles,
    procs' = procs.set(rid, { ...procs.get(rid), pc: "dead" }),
    childAlive' = childAlive, crashes' = crashes - 1, keepGhosts,
  }

  // ---- other processes ----------------------------------------------------

  // cancel_execution: request_cancellation of the reserved run.
  action requestCancel = {
    val rid = res.owner
    all {
      rid != 0,
      res.phase != "childStarting",
      st.owner == rid, not(terminal(st.status)),
      res' = { ...res, phase: "cancelling" },
      st' = { owner: rid, status: "cancelling" },
      marker' = marker, gen' = gen, lease' = lease,
      procs' = procs, childAlive' = childAlive, crashes' = crashes,
      hooksDone' = hooksDone, cancelAsked' = cancelAsked.set(rid, true), lie' = lie,
    }
  }

  // recover_drained, after the operator drained the run's processes.
  action recover = {
    val rid = res.owner
    all {
      rid != 0,
      not(childAlive.get(rid)),
      procs.get(rid).pc == "dead" or procs.get(rid).pc == "done",
      res' = NO_RES,
      st' = if (st.owner == rid and not(terminal(st.status))) { owner: rid, status: "cancelled" } else st,
      marker' = if (marker == rid) 0 else marker,
      gen' = if (gen == rid) 0 else gen,
      lease' = lease,
      procs' = procs, childAlive' = childAlive, crashes' = crashes, keepGhosts,
    }
  }

  // fast_status: what the shell is told.
  action status = all {
    marker != 0,
    lie' = (lie or (st.owner == marker and st.status == "completed" and not(hooksDone.get(marker)))),
    unchangedFiles, procs' = procs, childAlive' = childAlive, crashes' = crashes,
    hooksDone' = hooksDone, cancelAsked' = cancelAsked,
  }

  action stutter = all {
    unchangedFiles, procs' = procs, childAlive' = childAlive, crashes' = crashes, keepGhosts,
  }

  action runStep = {
    nondet rid = RUNS.oneOf()
    any {
      begin(rid), failStart(rid), join(rid), spawnChild(rid), childExits(rid),
      waitChild(rid), abandonChild(rid), seeCancel(rid), reap(rid), publish(rid), finishFailed(rid),
      ack(rid), die(rid),
    }
  }

  action step = any {
    runStep,
    requestCancel,
    recover,
    status,
    stutter,
  }

  // ---- properties ---------------------------------------------------------

  // Hooks of two runs never run at once in the directory.
  val oneChild = RUNS.forall(a => RUNS.forall(b =>
    (childAlive.get(a) and childAlive.get(b)) implies a == b))

  // A live hook process belongs to the reserved run, which records it (or
  // records that one may be starting), so cancellation and recovery see it.
  val childKnown = RUNS.forall(r => childAlive.get(r) implies and {
    res.owner == r,
    res.child or res.phase == "childStarting",
  })

  // A finished state never has a hook process still running.
  val finishedMeansStopped = RUNS.forall(r =>
    (st.owner == r and terminal(st.status)) implies not(childAlive.get(r)))

  // A run whose cancellation was accepted never ends as completed.
  val cancelHonoured = RUNS.forall(r =>
    (st.owner == r and st.status == "completed") implies not(cancelAsked.get(r)))

  val safety = and {
    oneChild, childKnown, finishedMeansStopped, cancelHonoured, not(lie),
  }

  // Reachability, expected to FAIL.
  val neverPublished = gen == 0
  val neverCancelled = st.status != "cancelled"
  val neverSecondRunCompleted = not(st.owner == 2 and st.status == "completed")
}
