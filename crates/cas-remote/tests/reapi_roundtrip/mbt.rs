//! Model-based test: the layered task cache against
//! `specs/quint/task_cache.qnt`.
//!
//! `quint run --mbt` generates random traces of the model. Each trace is
//! replayed against the real stack each machine builds for a run:
//! [`LayeredCas`] over a [`LocalCas`] and a [`CorruptionAwareCas`] around a
//! [`RemoteCas`], and [`LayeredActionCache`] over a [`LocalActionCache`] and
//! a [`RemoteActionCache`], all talking gRPC to the in-process REAPI server
//! of this test crate. The model's environment is applied for real: the
//! remote is switched off when the model says it is unreachable, the server
//! skips overwrites when the model says it does, blobs are evicted or
//! corrupted on disk and on the server, and a local CAS write fails when the
//! model says so. After every step the local and remote stores, the action
//! caches, the workspace and what each machine knows are compared with the
//! model's.
//!
//! Needs `quint` on `PATH`. The `cuenv-quint-mbt` flake check runs it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use cuenv_cas::{
    ActionCache, ActionResult, Cas, Digest, LocalActionCache, LocalCas, OutputFile, missing_blobs,
};
use cuenv_cas_remote::{
    CorruptionAwareCas, LayeredActionCache, LayeredCas, RemoteActionCache, RemoteCas, RemoteClient,
    RemoteConfig,
};
use quint_connect::{Driver, State, Step, quint_run, switch};
use serde::Deserialize;
use tempfile::TempDir;

use super::{Store, TestServer, lock, sha256};

// The model's constants (task_cache.qnt).
const MACHINES: [i64; 2] = [1, 2];
const BLOBS: [i64; 5] = [11, 12, 21, 22, ACTION_BLOB];
const ACTION_BLOB: i64 = 99;
const RUNS: i64 = 2;
const FAULTS: i64 = 2;
const ABSENT: i64 = 0;
const OK: i64 = 1;
const CORRUPT: i64 = 2;
const OLD: i64 = 0;
const BAD_BYTES: i64 = -1;

fn file_a(result: i64) -> i64 {
    result * 10 + 1
}

fn file_b(result: i64) -> i64 {
    result * 10 + 2
}

fn bytes_of(blob: i64) -> Vec<u8> {
    format!("cuenv-mbt-blob-{blob:02}").into_bytes()
}

/// Bytes of the same length as [`bytes_of`] that do not match its digest.
fn corrupt_bytes_of(blob: i64) -> Vec<u8> {
    format!("cuenv-mbt-BLOB-{blob:02}").into_bytes()
}

fn digest_of(blob: i64) -> Digest {
    Digest::of_bytes(&bytes_of(blob))
}

fn server_key(digest: &Digest) -> String {
    format!("{}/{}", digest.hash, digest.size_bytes)
}

/// The blob these bytes are, as the model numbers them.
fn blob_of_bytes(bytes: &[u8]) -> i64 {
    BLOBS
        .iter()
        .copied()
        .find(|blob| bytes == bytes_of(*blob).as_slice())
        .unwrap_or(BAD_BYTES)
}

/// A stored blob's state, as the model sees it.
fn blob_state(stored: Option<&[u8]>, blob: i64) -> i64 {
    match stored {
        None => ABSENT,
        Some(bytes) if bytes == bytes_of(blob).as_slice() => OK,
        Some(_) => CORRUPT,
    }
}

fn result_of(result: i64) -> ActionResult {
    ActionResult {
        exit_code: 0,
        output_files: vec![
            OutputFile {
                path: "a".to_string(),
                digest: digest_of(file_a(result)),
                is_executable: false,
            },
            OutputFile {
                path: "b".to_string(),
                digest: digest_of(file_b(result)),
                is_executable: false,
            },
        ],
        ..ActionResult::default()
    }
}

/// The result an action result names, by its first output.
fn result_id(first_output_hash: &str) -> i64 {
    [1, 2]
        .into_iter()
        .find(|result| digest_of(file_a(*result)).hash == first_output_hash)
        .unwrap_or(-1)
}

fn action_digest() -> Digest {
    digest_of(ACTION_BLOB)
}

// -- the environment's switches ------------------------------------------------

fn unreachable_error() -> cuenv_cas::Error {
    cuenv_cas::Error::serialization("the remote is unreachable")
}

/// A remote store that answers only while `reachable` is set.
struct SwitchedCas {
    inner: Arc<dyn Cas>,
    reachable: Arc<AtomicBool>,
}

impl SwitchedCas {
    fn check(&self) -> cuenv_cas::Result<()> {
        if self.reachable.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(unreachable_error())
        }
    }
}

#[async_trait]
impl Cas for SwitchedCas {
    async fn contains(&self, digest: &Digest) -> cuenv_cas::Result<bool> {
        self.check()?;
        self.inner.contains(digest).await
    }
    async fn get(&self, digest: &Digest) -> cuenv_cas::Result<Vec<u8>> {
        self.check()?;
        self.inner.get(digest).await
    }
    async fn get_to_file(&self, digest: &Digest, destination: &Path) -> cuenv_cas::Result<()> {
        self.check()?;
        self.inner.get_to_file(digest, destination).await
    }
    async fn put_bytes(&self, bytes: &[u8]) -> cuenv_cas::Result<Digest> {
        self.check()?;
        self.inner.put_bytes(bytes).await
    }
    async fn put_file(&self, source: &Path) -> cuenv_cas::Result<Digest> {
        self.check()?;
        self.inner.put_file(source).await
    }
    async fn replace_bytes(&self, bytes: &[u8]) -> cuenv_cas::Result<Digest> {
        self.check()?;
        self.inner.replace_bytes(bytes).await
    }
    async fn replace_file(&self, source: &Path) -> cuenv_cas::Result<Digest> {
        self.check()?;
        self.inner.replace_file(source).await
    }
}

/// A remote action cache that answers only while `reachable` is set.
struct SwitchedActionCache {
    inner: RemoteActionCache,
    reachable: Arc<AtomicBool>,
}

#[async_trait]
impl ActionCache for SwitchedActionCache {
    async fn lookup(&self, action_digest: &Digest) -> cuenv_cas::Result<Option<ActionResult>> {
        if !self.reachable.load(Ordering::SeqCst) {
            return Err(unreachable_error());
        }
        self.inner.lookup(action_digest).await
    }
    async fn update(&self, action_digest: &Digest, result: &ActionResult) -> cuenv_cas::Result<()> {
        if !self.reachable.load(Ordering::SeqCst) {
            return Err(unreachable_error());
        }
        self.inner.update(action_digest, result).await
    }
}

/// A local store whose next write fails when armed.
struct FlakyCas {
    inner: Arc<LocalCas>,
    armed: AtomicBool,
    fired: AtomicBool,
}

impl FlakyCas {
    fn fail_next_write(&self) -> cuenv_cas::Result<()> {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.fired.store(true, Ordering::SeqCst);
            return Err(cuenv_cas::Error::serialization("the disk is full"));
        }
        Ok(())
    }
}

#[async_trait]
impl Cas for FlakyCas {
    async fn contains(&self, digest: &Digest) -> cuenv_cas::Result<bool> {
        self.inner.contains(digest).await
    }
    async fn get(&self, digest: &Digest) -> cuenv_cas::Result<Vec<u8>> {
        self.inner.get(digest).await
    }
    async fn get_to_file(&self, digest: &Digest, destination: &Path) -> cuenv_cas::Result<()> {
        self.inner.get_to_file(digest, destination).await
    }
    async fn put_bytes(&self, bytes: &[u8]) -> cuenv_cas::Result<Digest> {
        self.fail_next_write()?;
        self.inner.put_bytes(bytes).await
    }
    async fn put_file(&self, source: &Path) -> cuenv_cas::Result<Digest> {
        self.fail_next_write()?;
        self.inner.put_file(source).await
    }
}

// -- the model's state, as compared -------------------------------------------

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct StoresView {
    local: BTreeMap<i64, BTreeMap<i64, i64>>,
    #[serde(rename = "localAC")]
    local_ac: BTreeMap<i64, i64>,
    remote: BTreeMap<i64, i64>,
    #[serde(rename = "remoteAC")]
    remote_ac: i64,
    faults: i64,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MachineView {
    pc: String,
    round: i64,
    cand: i64,
    ws_a: i64,
    ws_b: i64,
    outcome: String,
    known_bad: BTreeSet<i64>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct Observed {
    st: StoresView,
    machines: BTreeMap<i64, MachineView>,
}

impl State<CacheDriver> for Observed {
    fn from_driver(driver: &CacheDriver) -> quint_connect::Result<Self> {
        let world = driver.world()?;
        let blobs = lock(&world.server.store.blobs).clone();
        let remote_ac = lock(&world.server.store.results)
            .get(&server_key(&action_digest()))
            .and_then(|result| result.output_files.first())
            .and_then(|output| output.digest.as_ref())
            .map_or(0, |digest| result_id(&digest.hash));
        let mut local = BTreeMap::new();
        let mut local_ac = BTreeMap::new();
        let mut machines = BTreeMap::new();
        for (id, machine) in &world.machines {
            let mut states = BTreeMap::new();
            for blob in BLOBS {
                let path = machine.local.blob_path(&digest_of(blob))?;
                let stored = std::fs::read(path).ok();
                states.insert(blob, blob_state(stored.as_deref(), blob));
            }
            local.insert(*id, states);
            let entry = driver
                .runtime
                .block_on(machine.local_ac.lookup(&action_digest()))?;
            local_ac.insert(
                *id,
                entry
                    .as_ref()
                    .and_then(|result| result.output_files.first())
                    .map_or(0, |output| result_id(&output.digest.hash)),
            );
            machines.insert(*id, machine.view());
        }
        Ok(Self {
            st: StoresView {
                local,
                local_ac,
                remote: BLOBS
                    .iter()
                    .map(|blob| {
                        let stored = blobs.get(&server_key(&digest_of(*blob)));
                        (*blob, blob_state(stored.map(Vec::as_slice), *blob))
                    })
                    .collect(),
                remote_ac,
                faults: world.faults,
            },
            machines,
        })
    }
}

// -- the driver ---------------------------------------------------------------

/// What one machine builds for one run (one process).
struct Stack {
    cas: LayeredCas,
    action_cache: LayeredActionCache,
    corruption: Arc<CorruptionAwareCas>,
}

struct Machine {
    directory: TempDir,
    local: Arc<LocalCas>,
    flaky: Arc<FlakyCas>,
    local_ac: Arc<LocalActionCache>,
    remote: Arc<dyn Cas>,
    client: RemoteClient,
    stack: Option<Stack>,
    pc: String,
    round: i64,
    cand: i64,
    staged: [i64; 2],
    outcome: String,
}

impl Machine {
    fn workspace(&self, name: &str) -> PathBuf {
        self.directory.path().join("workspace").join(name)
    }

    fn staging(&self, name: &str) -> PathBuf {
        self.directory.path().join("staging").join(name)
    }

    fn workspace_blob(&self, name: &str) -> i64 {
        std::fs::read(self.workspace(name)).map_or(OLD, |bytes| blob_of_bytes(&bytes))
    }

    fn view(&self) -> MachineView {
        MachineView {
            pc: self.pc.clone(),
            round: self.round,
            cand: self.cand,
            ws_a: self.workspace_blob("a"),
            ws_b: self.workspace_blob("b"),
            outcome: self.outcome.clone(),
            known_bad: self.stack.as_ref().map_or_else(BTreeSet::new, |stack| {
                BLOBS
                    .iter()
                    .copied()
                    .filter(|blob| stack.corruption.is_known_corrupt(&digest_of(*blob)))
                    .collect()
            }),
        }
    }

    fn stack(&self) -> anyhow::Result<&Stack> {
        self.stack
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("machine has no stack in {}", self.pc))
    }
}

/// One trace's server, machines and fault budget.
struct World {
    server: TestServer,
    reachable: Arc<AtomicBool>,
    machines: BTreeMap<i64, Machine>,
    faults: i64,
}

struct CacheDriver {
    world: Option<World>,
    runtime: tokio::runtime::Runtime,
}

impl CacheDriver {
    fn new() -> anyhow::Result<Self> {
        Ok(Self {
            world: None,
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?,
        })
    }

    fn world(&self) -> anyhow::Result<&World> {
        self.world
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no world before init"))
    }

    fn world_mut(&mut self) -> anyhow::Result<&mut World> {
        self.world
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no world before init"))
    }

    fn init(&mut self) -> anyhow::Result<()> {
        self.world = None;
        let world = self.runtime.block_on(async {
            let server = TestServer::start(Store::default(), sha256()).await;
            let reachable = Arc::new(AtomicBool::new(true));
            let mut machines = BTreeMap::new();
            for id in MACHINES {
                let directory = TempDir::new()?;
                std::fs::create_dir_all(directory.path().join("workspace"))?;
                std::fs::create_dir_all(directory.path().join("staging"))?;
                let local = Arc::new(LocalCas::open(directory.path().join("cas"))?);
                let local_ac = Arc::new(LocalActionCache::open(directory.path().join("ac"))?);
                let client = server.client(RemoteConfig::default().writable()).await;
                let remote: Arc<dyn Cas> = Arc::new(RemoteCas::connect(client.clone()).await?);
                machines.insert(
                    id,
                    Machine {
                        flaky: Arc::new(FlakyCas {
                            inner: local.clone(),
                            armed: AtomicBool::new(false),
                            fired: AtomicBool::new(false),
                        }),
                        directory,
                        local,
                        local_ac,
                        remote,
                        client,
                        stack: None,
                        pc: "start".to_string(),
                        round: 1,
                        cand: 0,
                        staged: [OLD, OLD],
                        outcome: String::new(),
                    },
                );
            }
            anyhow::Ok(World {
                server,
                reachable,
                machines,
                faults: FAULTS,
            })
        })?;
        self.world = Some(world);
        Ok(())
    }

    /// One step of machine `id`, with the environment's choices.
    fn machine(
        &mut self,
        id: i64,
        reach: bool,
        local_write_fails: bool,
        honours: bool,
    ) -> anyhow::Result<()> {
        let runtime = &self.runtime;
        let world = self
            .world
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no world before init"))?;
        world.reachable.store(reach, Ordering::SeqCst);
        world
            .server
            .store
            .ignore_overwrites
            .store(!honours, Ordering::SeqCst);
        let faults = world.faults;
        let reachable = world.reachable.clone();
        let machine = world
            .machines
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("no machine {id}"))?;
        let action = action_digest();
        match machine.pc.as_str() {
            "start" => {
                // A new process: a new stack, nothing known to be corrupt.
                let corruption = Arc::new(CorruptionAwareCas::new(Arc::new(SwitchedCas {
                    inner: machine.remote.clone(),
                    reachable: reachable.clone(),
                })));
                let stack = Stack {
                    cas: LayeredCas::new(machine.flaky.clone(), corruption.clone()).with_push(),
                    action_cache: LayeredActionCache::new(
                        machine.local_ac.clone(),
                        Arc::new(SwitchedActionCache {
                            inner: RemoteActionCache::new(machine.client.clone()),
                            reachable,
                        }),
                    )
                    .with_remote_cas(corruption.clone())
                    .with_local_cas(machine.local.clone())
                    .with_push(),
                    corruption,
                };
                runtime.block_on(stack.cas.put_bytes(&bytes_of(ACTION_BLOB)))?;
                machine.stack = Some(stack);
                machine.pc = "lookup".to_string();
                machine.cand = 0;
                machine.staged = [OLD, OLD];
                machine.outcome = String::new();
            }
            "lookup" => {
                let found = runtime.block_on(machine.stack()?.action_cache.lookup(&action))?;
                machine.cand = found
                    .as_ref()
                    .and_then(|result| result.output_files.first())
                    .map_or(0, |output| result_id(&output.digest.hash));
                machine.pc = if machine.cand == 0 { "exec" } else { "check" }.to_string();
            }
            "check" => {
                let missing = runtime.block_on(missing_blobs(
                    &machine.stack()?.cas,
                    &result_of(machine.cand),
                ));
                machine.pc = match missing {
                    Ok(missing) if missing.is_empty() => "fetchA",
                    _ => "exec",
                }
                .to_string();
            }
            "fetchA" | "fetchB" => {
                let first = machine.pc == "fetchA";
                let (blob, name) = if first {
                    (file_a(machine.cand), "a")
                } else {
                    (file_b(machine.cand), "b")
                };
                let staged = machine.staging(name);
                let _ = std::fs::remove_file(&staged);
                machine
                    .flaky
                    .armed
                    .store(local_write_fails && faults > 0, Ordering::SeqCst);
                machine.flaky.fired.store(false, Ordering::SeqCst);
                let fetched =
                    runtime.block_on(machine.stack()?.cas.get_to_file(&digest_of(blob), &staged));
                machine.flaky.armed.store(false, Ordering::SeqCst);
                let fired = machine.flaky.fired.swap(false, Ordering::SeqCst);
                if fetched.is_ok() {
                    let bytes = std::fs::read(&staged)?;
                    machine.staged[usize::from(!first)] = blob_of_bytes(&bytes);
                    machine.pc = if first { "fetchB" } else { "commit" }.to_string();
                } else {
                    machine.staged = [OLD, OLD];
                    machine.pc = "exec".to_string();
                }
                if fired {
                    world.faults -= 1;
                }
            }
            "commit" => {
                // commit_staged_outputs: every staged output replaces its
                // workspace file.
                for name in ["a", "b"] {
                    std::fs::copy(machine.staging(name), machine.workspace(name))?;
                }
                runtime.block_on(
                    machine
                        .stack()?
                        .action_cache
                        .commit_verified(&action, &result_of(machine.cand)),
                )?;
                machine.pc = "finish".to_string();
                machine.outcome = "hit".to_string();
            }
            "exec" => {
                // The task runs; machine `id` produces result `id`.
                std::fs::write(machine.workspace("a"), bytes_of(file_a(id)))?;
                std::fs::write(machine.workspace("b"), bytes_of(file_b(id)))?;
                machine.cand = id;
                machine.staged = [OLD, OLD];
                machine.pc = "putA".to_string();
            }
            "putA" | "putB" => {
                let first = machine.pc == "putA";
                let source = machine.workspace(if first { "a" } else { "b" });
                runtime.block_on(machine.stack()?.cas.put_file(&source))?;
                machine.pc = if first { "putB" } else { "update" }.to_string();
            }
            "update" => {
                runtime.block_on(
                    machine
                        .stack()?
                        .action_cache
                        .update(&action, &result_of(machine.cand)),
                )?;
                machine.pc = "finish".to_string();
                machine.outcome = "executed".to_string();
            }
            "finish" => {
                if machine.round < RUNS {
                    machine.round += 1;
                    machine.pc = "start".to_string();
                } else {
                    machine.pc = "done".to_string();
                }
            }
            other => anyhow::bail!("machine {id} cannot step from {other}"),
        }
        Ok(())
    }

    fn fault(&mut self, blob: i64, id: i64, kind: &str) -> anyhow::Result<()> {
        let world = self.world_mut()?;
        world.faults -= 1;
        let digest = digest_of(blob);
        match kind {
            "evictRemote" => {
                lock(&world.server.store.blobs).remove(&server_key(&digest));
            }
            "corruptRemote" => {
                let mut blobs = lock(&world.server.store.blobs);
                if let Some(stored) = blobs.get_mut(&server_key(&digest))
                    && *stored == bytes_of(blob)
                {
                    *stored = corrupt_bytes_of(blob);
                }
            }
            "evictRemoteAC" => {
                lock(&world.server.store.results).remove(&server_key(&action_digest()));
            }
            "evictLocal" | "corruptLocal" => {
                let machine = world
                    .machines
                    .get(&id)
                    .ok_or_else(|| anyhow::anyhow!("no machine {id}"))?;
                let path = machine.local.blob_path(&digest)?;
                if kind == "evictLocal" {
                    let _ = std::fs::remove_file(&path);
                } else if std::fs::read(&path).is_ok_and(|stored| stored == bytes_of(blob)) {
                    let mut permissions = std::fs::metadata(&path)?.permissions();
                    #[expect(
                        clippy::permissions_set_readonly_false,
                        reason = "the test corrupts a blob the store made read-only"
                    )]
                    permissions.set_readonly(false);
                    std::fs::set_permissions(&path, permissions)?;
                    std::fs::write(&path, corrupt_bytes_of(blob))?;
                }
            }
            other => anyhow::bail!("unknown fault {other}"),
        }
        Ok(())
    }
}

impl Driver for CacheDriver {
    type State = Observed;

    fn step(&mut self, step: &Step) -> quint_connect::Result {
        switch!(step {
            init => self.init()?,
            machine(m: i64, reach: bool, failing: bool, honours: bool) => {
                self.machine(m, reach, failing, honours)?;
            },
            fault(d: i64, m: i64, kind: String) => self.fault(d, m, &kind)?,
            reachableMachine(m: i64, failing: bool, honours: bool) => {
                self.machine(m, true, failing, honours)?;
            },
            damage(d: i64, m: i64, kind: String) => self.fault(d, m, &kind)?,
            quiescent => {}
        })
    }
}

#[quint_run(
    spec = "../../specs/quint/task_cache.qnt",
    max_samples = 200,
    max_steps = 45
)]
#[ignore = "model-based test: needs quint on PATH; run by the cuenv-quint-mbt flake check"]
fn the_layered_cache_follows_the_model() -> impl Driver {
    CacheDriver::new().expect("a tokio runtime")
}

/// The same model under `corruptionStep`: a reachable remote and an
/// adversary that corrupts remote outputs and evicts local ones, so the
/// corrupt-blob path (detect, re-upload, read back, publish or not) runs.
#[quint_run(
    spec = "../../specs/quint/task_cache.qnt",
    step = "corruptionStep",
    max_samples = 200,
    max_steps = 45
)]
#[ignore = "model-based test: needs quint on PATH; run by the cuenv-quint-mbt flake check"]
fn the_layered_cache_repairs_corrupt_remote_blobs_as_modelled() -> impl Driver {
    CacheDriver::new().expect("a tokio runtime")
}

/// The same model under `localFailureStep`: the fault budget goes to local
/// CAS writes that fail while a fetched blob is kept, so a remote hit is
/// served without its blobs stored locally and must not be promoted.
#[quint_run(
    spec = "../../specs/quint/task_cache.qnt",
    step = "localFailureStep",
    max_samples = 200,
    max_steps = 45
)]
#[ignore = "model-based test: needs quint on PATH; run by the cuenv-quint-mbt flake check"]
fn the_layered_cache_promotes_only_local_blobs_as_modelled() -> impl Driver {
    CacheDriver::new().expect("a tokio runtime")
}
