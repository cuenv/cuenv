//! Model-based test: the write protocols in [`super::protocol`] against
//! `specs/quint/object_state_cas.qnt`.
//!
//! `quint run --mbt` generates random traces of the model. Each trace is
//! replayed here step by step: the clients are the real [`Acquire`],
//! [`Mutate`] and [`Release`] protocols (with the real conditional-put change
//! the store applies), driven by an engine that does what the engine does
//! (acquire, list, put, release, replay an unrecorded change). The bucket is
//! an in-memory `state.json` with conditional writes, and the network is the
//! model's: requests land late or never, responses get lost. After every
//! step the document, the requests in flight and each client's state are
//! compared with the model's, so any divergence between the code and the
//! model fails the test with a diff.
//!
//! Needs `quint` on `PATH`. The `cuenv-quint-mbt` flake check runs it.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use quint_connect::{Driver, State, Step as QuintStep, quint_run, switch};
use serde::Deserialize;
use serde_json::json;

use super::client::{Condition, S3Failure};
use super::document::{DocumentLock, StateDocument};
use super::key::TenantPath;
use super::protocol::{
    Acquire, AcquireRequest, AcquireState, Action, Context, Input, Mutate, MutateState, Mutation,
    Nonces, Protocol, Release, ReleaseOutcome, ReleaseState, Step,
};
use super::store::{apply_conditional_put, insertion_of};
use crate::error::{InfrastructureError, Result};
use crate::state::{ConditionalPut, ManagedResource, RecordVersion, ResourceAddress};
use crate::tenant::TenantKey;

// The model's constants (object_state_cas.qnt).
const CLIENTS: [i64; 2] = [1, 2];
const MAX_ATTEMPTS: u32 = 3;
const MAX_ROUNDS: i64 = 2;
const MAX_TRIES: i64 = 1;
const ABSENT: i64 = -1;
const OPERATOR: i64 = -1;

fn lock_of(client: i64, machine: &Machine) -> i64 {
    client * 100 + machine.round * 10 + machine.tries
}

fn content_of(client: i64) -> i64 {
    client * 1000
}

fn insertion_of_model(client: i64, machine: &Machine) -> i64 {
    client * 10000 + machine.round
}

fn nonce_of(client: i64, machine: &Machine, phase: i64, attempt: u32) -> i64 {
    client * 100_000
        + machine.round * 10_000
        + machine.tries * 1000
        + phase * 100
        + i64::from(attempt)
}

fn address() -> ResourceAddress {
    ResourceAddress::new("random_pet", "pet")
}

fn uuid_of(model: i64) -> uuid::Uuid {
    uuid::Uuid::from_u128(u128::try_from(model).unwrap_or_default())
}

fn model_of_uuid(generation: uuid::Uuid) -> i64 {
    i64::try_from(generation.as_u128()).unwrap_or(-1)
}

fn number(text: &str) -> i64 {
    text.parse().unwrap_or(0)
}

// -- the model's state, as compared -------------------------------------------

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DocView {
    exists: bool,
    ver: i64,
    lock: i64,
    last_released: i64,
    writer: i64,
    #[serde(rename = "gen")]
    generation: i64,
    serial: i64,
    content: i64,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
struct RequestView {
    client: i64,
    nonce: i64,
    kind: String,
    cond: i64,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientView {
    pc: String,
    attempt: i64,
    round: i64,
    tries: i64,
    exp_gen: i64,
    exp_serial: i64,
    pay_gen: i64,
    acq_out: String,
    put_out: String,
    rel_out: String,
    rel_lock: i64,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct Observed {
    doc: DocView,
    inflight: BTreeSet<RequestView>,
    clients: BTreeMap<i64, ClientView>,
}

impl State<ProtocolDriver> for Observed {
    fn from_driver(driver: &ProtocolDriver) -> quint_connect::Result<Self> {
        Ok(Self {
            doc: driver.doc_view(),
            inflight: driver
                .inflight
                .iter()
                .map(|request| RequestView {
                    client: request.client,
                    nonce: request.nonce,
                    kind: request.kind.to_string(),
                    cond: request.cond,
                })
                .collect(),
            clients: driver
                .machines
                .iter()
                .map(|(client, machine)| (*client, machine.view()))
                .collect(),
        })
    }
}

// -- the driver ---------------------------------------------------------------

/// Nonces chosen by the model: the driver sets the next one before each
/// resume.
#[derive(Clone, Default)]
struct ModelNonces(Rc<Cell<i64>>);

impl Nonces for ModelNonces {
    fn next(&mut self) -> String {
        self.0.get().to_string()
    }
}

type Change = Box<dyn FnMut(&mut StateDocument) -> Result<Mutation>>;

enum Running {
    Acquire(Box<Acquire<ModelNonces>>),
    Mutate(Mutate<Change, ModelNonces>),
    Release(Release<ModelNonces>),
}

impl Running {
    fn phase(&self) -> i64 {
        match self {
            Self::Acquire(_) => 1,
            Self::Mutate(_) => 2,
            Self::Release(_) => 3,
        }
    }

    fn attempt(&self) -> u32 {
        match self {
            Self::Acquire(protocol) => protocol.attempt(),
            Self::Mutate(protocol) => protocol.attempt(),
            Self::Release(protocol) => protocol.attempt(),
        }
    }

    /// The model's `pc` for the protocol's current state.
    fn pc(&self) -> &'static str {
        match self {
            Self::Acquire(protocol) => match protocol.state() {
                AcquireState::Reading => "acq_read",
                AcquireState::Creating { .. } => "acq_create",
                AcquireState::CreateRefused { .. } => "acq_create_refused",
                AcquireState::CreateReadingBack { .. } => "acq_create_readback",
                AcquireState::Writing { .. } => "acq_write",
                AcquireState::ReadingBack => "acq_readback",
                AcquireState::CheckingVanished
                | AcquireState::CreatePausing { .. }
                | AcquireState::Pausing => "acq_transient",
            },
            Self::Mutate(protocol) => match protocol.state() {
                MutateState::Reading => "put_read",
                MutateState::Writing { .. } => "put_write",
                MutateState::ReadingBack { .. } => "put_readback",
                MutateState::Pausing => "put_transient",
            },
            Self::Release(protocol) => match protocol.state() {
                ReleaseState::Reading => "rel_read",
                ReleaseState::Writing { .. } => "rel_write",
                ReleaseState::ReadingBack => "rel_readback",
                ReleaseState::Pausing => "rel_transient",
            },
        }
    }
}

/// What a finished protocol returned.
enum Finished {
    Acquired(Result<u64>),
    Put(Result<()>),
    Released(Result<ReleaseOutcome>),
}

struct Machine {
    pc: String,
    attempt: u32,
    round: i64,
    tries: i64,
    replay: bool,
    expected: RecordVersion,
    pay_gen: i64,
    acq_out: String,
    put_out: String,
    rel_out: String,
    /// The lock in the document the release returned, which `unlock`
    /// snapshots.
    rel_lock: i64,
    running: Option<Running>,
    waiting: Option<i64>,
    response: Option<std::result::Result<(), S3Failure>>,
}

impl Machine {
    fn fresh() -> Self {
        Self {
            pc: "idle".to_string(),
            attempt: 0,
            round: 1,
            tries: 0,
            replay: false,
            expected: RecordVersion::Absent,
            pay_gen: 0,
            acq_out: String::new(),
            put_out: String::new(),
            rel_out: String::new(),
            rel_lock: 0,
            running: None,
            waiting: None,
            response: None,
        }
    }

    fn view(&self) -> ClientView {
        let (exp_gen, exp_serial) = match self.expected {
            RecordVersion::Absent => (0, 0),
            RecordVersion::Generation { generation, serial } => (model_of_uuid(generation), serial),
        };
        ClientView {
            pc: self.pc.clone(),
            attempt: i64::from(self.attempt),
            round: self.round,
            tries: self.tries,
            exp_gen,
            exp_serial,
            pay_gen: self.pay_gen,
            acq_out: self.acq_out.clone(),
            put_out: self.put_out.clone(),
            rel_out: self.rel_out.clone(),
            rel_lock: self.rel_lock,
        }
    }
}

struct Request {
    client: i64,
    nonce: i64,
    kind: &'static str,
    cond: i64,
    body: StateDocument,
}

struct ProtocolDriver {
    tenant: TenantKey,
    path: TenantPath,
    doc: Option<StateDocument>,
    inflight: Vec<Request>,
    machines: BTreeMap<i64, Machine>,
    nonces: ModelNonces,
}

impl Default for ProtocolDriver {
    fn default() -> Self {
        let tenant = TenantKey::try_new("example.com/mbt", "web").expect("valid tenant");
        let path = TenantPath::new(None, "cuenv", &tenant).expect("valid path");
        Self {
            tenant,
            path,
            doc: None,
            inflight: Vec::new(),
            machines: CLIENTS
                .iter()
                .map(|client| (*client, Machine::fresh()))
                .collect(),
            nonces: ModelNonces::default(),
        }
    }
}

impl ProtocolDriver {
    fn context(&self) -> Context {
        Context {
            tenant: self.tenant.clone(),
            key: "state.json".to_string(),
            location: "s3://mbt/state.json".to_string(),
            attempts: MAX_ATTEMPTS,
        }
    }

    fn machine(&mut self, client: i64) -> anyhow::Result<&mut Machine> {
        self.machines
            .get_mut(&client)
            .ok_or_else(|| anyhow::anyhow!("no client {client}"))
    }

    fn read(&self) -> Option<(StateDocument, String)> {
        self.doc
            .as_ref()
            .map(|document| (document.clone(), document.serial.to_string()))
    }

    fn doc_view(&self) -> DocView {
        let Some(document) = &self.doc else {
            return DocView {
                exists: false,
                ver: 0,
                lock: 0,
                last_released: 0,
                writer: 0,
                generation: 0,
                serial: 0,
                content: 0,
            };
        };
        let record = document.resource(&address()).ok().flatten();
        DocView {
            exists: true,
            ver: i64::try_from(document.serial).unwrap_or(-1),
            lock: document
                .lock
                .as_ref()
                .map_or(0, |lock| number(&lock.lock_identifier)),
            last_released: document.last_released.as_deref().map_or(0, number),
            writer: number(&document.writer),
            generation: record
                .as_ref()
                .map_or(0, |record| model_of_uuid(record.generation)),
            serial: record.as_ref().map_or(0, |record| record.serial),
            content: record
                .as_ref()
                .and_then(|record| record.state["content"].as_i64())
                .unwrap_or(0),
        }
    }

    /// The record client `client` writes.
    fn resource(client: i64, generation: uuid::Uuid) -> ManagedResource {
        ManagedResource {
            address: address(),
            provider: "random".into(),
            provider_source: "registry.terraform.io/hashicorp/random".into(),
            schema_version: 0,
            state: json!({"content": content_of(client)}),
            private: Vec::new(),
            dependencies: Vec::new(),
            sensitive_attributes: Vec::new(),
            tainted: false,
            identity: None,
            serial: 0,
            generation,
        }
    }

    /// Drop requests that can no longer match and that nobody waits for,
    /// as the model does.
    fn collect_garbage(&mut self) {
        let serial = self.doc.as_ref().map(|document| document.serial);
        let waiting: BTreeSet<i64> = self
            .machines
            .values()
            .filter_map(|machine| machine.waiting)
            .collect();
        self.inflight.retain(|request| {
            let live = if request.cond == ABSENT {
                serial.is_none()
            } else {
                serial.is_some_and(|serial| i64::try_from(serial).unwrap_or(-1) == request.cond)
            };
            live || waiting.contains(&request.nonce)
        });
    }

    /// Resume (or start) client `client`'s protocol, carrying out waits and
    /// the vanished-state check on the spot, until it reads, writes or ends.
    fn run(&mut self, client: i64, input: Option<Input>) -> anyhow::Result<()> {
        let nonces = self.nonces.clone();
        let mut input = input;
        loop {
            let machine = self.machine(client)?;
            let (phase, attempt) = machine
                .running
                .as_ref()
                .map(|running| (running.phase(), running.attempt()))
                .ok_or_else(|| anyhow::anyhow!("client {client} runs no protocol"))?;
            nonces.0.set(nonce_of(client, machine, phase, attempt));
            let running = machine
                .running
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("client {client} runs no protocol"))?;
            let step = match (running, input.take()) {
                (Running::Acquire(protocol), None) => protocol
                    .start()
                    .map(|step| step.map(|output| Finished::Acquired(Ok(output)))),
                (Running::Acquire(protocol), Some(input)) => protocol
                    .resume(input)
                    .map(|step| step.map(|output| Finished::Acquired(Ok(output)))),
                (Running::Mutate(protocol), None) => protocol
                    .start()
                    .map(|step| step.map(|output| Finished::Put(Ok(output)))),
                (Running::Mutate(protocol), Some(input)) => protocol
                    .resume(input)
                    .map(|step| step.map(|output| Finished::Put(Ok(output)))),
                (Running::Release(protocol), None) => protocol
                    .start()
                    .map(|step| step.map(|output| Finished::Released(Ok(output)))),
                (Running::Release(protocol), Some(input)) => protocol
                    .resume(input)
                    .map(|step| step.map(|output| Finished::Released(Ok(output)))),
            };
            match step {
                Ok(Step::Next(Action::Wait(_))) => input = Some(Input::Waited),
                Ok(Step::Next(Action::CheckNotVanished)) => input = Some(Input::NotVanished),
                Ok(Step::Next(Action::Read | Action::ReadBack)) => {
                    let machine = self.machine(client)?;
                    if let Some(running) = &machine.running {
                        machine.pc = running.pc().to_string();
                        machine.attempt = running.attempt();
                    }
                    machine.waiting = None;
                    machine.response = None;
                    return Ok(());
                }
                Ok(Step::Next(Action::Put { body, condition })) => {
                    let machine = self.machine(client)?;
                    let running = machine
                        .running
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("no protocol"))?;
                    let (kind, cond) = match (&condition, running) {
                        (Condition::Absent, _) => ("create", ABSENT),
                        (Condition::Matches(etag), Running::Acquire(_)) => {
                            ("acquire", number(etag))
                        }
                        (Condition::Matches(etag), Running::Mutate(_)) => ("put", number(etag)),
                        (Condition::Matches(etag), Running::Release(_)) => {
                            ("release", number(etag))
                        }
                        (Condition::None, _) => {
                            anyhow::bail!("an unconditional write of state.json")
                        }
                    };
                    let nonce = number(&body.writer);
                    machine.pc = running.pc().to_string();
                    machine.attempt = running.attempt();
                    machine.waiting = Some(nonce);
                    machine.response = None;
                    // A resent create carries the same nonce and body: one
                    // request, as in the model's set.
                    if !self.inflight.iter().any(|request| {
                        request.client == client && request.nonce == nonce && request.cond == cond
                    }) {
                        self.inflight.push(Request {
                            client,
                            nonce,
                            kind,
                            cond,
                            body: *body,
                        });
                    }
                    return Ok(());
                }
                Ok(Step::Done(finished)) => return self.finish(client, finished),
                Err(error) => {
                    let finished = match self.machine(client)?.running.as_ref() {
                        Some(Running::Acquire(_)) => Finished::Acquired(Err(error)),
                        Some(Running::Mutate(_)) => Finished::Put(Err(error)),
                        _ => Finished::Released(Err(error)),
                    };
                    return self.finish(client, finished);
                }
            }
        }
    }

    /// What the engine does when a protocol ends.
    fn finish(&mut self, client: i64, finished: Finished) -> anyhow::Result<()> {
        let lock = lock_of(client, self.machine(client)?);
        let machine = self.machine(client)?;
        machine.waiting = None;
        machine.response = None;
        let attempt = machine
            .running
            .as_ref()
            .map_or(machine.attempt, Running::attempt);
        machine.running = None;
        match finished {
            Finished::Acquired(Ok(_)) => {
                machine.acq_out = "acquired".to_string();
                machine.attempt = 0;
                if machine.replay {
                    return self.start_put(client);
                }
                machine.pc = "list".to_string();
            }
            Finished::Acquired(Err(InfrastructureError::Locked { .. })) => {
                machine.acq_out = "locked".to_string();
                machine.attempt = attempt;
                if machine.tries + 1 < MAX_TRIES {
                    machine.tries += 1;
                    machine.pc = "idle".to_string();
                } else {
                    machine.pc = "done".to_string();
                }
            }
            Finished::Acquired(Err(error)) => {
                // acquire_lock keeps the marker when the error names the lock.
                machine.acq_out = if error.to_string().contains(&lock.to_string()) {
                    "uncertain"
                } else {
                    "error"
                }
                .to_string();
                machine.attempt = attempt;
                machine.pc = "done".to_string();
            }
            Finished::Put(outcome) => {
                machine.put_out = match outcome {
                    Ok(()) => "ok",
                    Err(InfrastructureError::LockLost { .. }) => "lockLost",
                    Err(InfrastructureError::StateChanged { .. }) => "stateChanged",
                    Err(error) if error.to_string().contains("vanished") => "vanished",
                    Err(error) if error.to_string().contains("could not write") => "exhausted",
                    Err(error) => anyhow::bail!("unexpected put failure: {error}"),
                }
                .to_string();
                return self.start_release(client);
            }
            Finished::Released(outcome) => {
                let (out, lock) = match outcome {
                    Ok(ReleaseOutcome::Released(document)) => (
                        "released",
                        document
                            .lock
                            .as_ref()
                            .map_or(0, |held| number(&held.lock_identifier)),
                    ),
                    Ok(ReleaseOutcome::NotHeld) => ("notHeld", 0),
                    Ok(ReleaseOutcome::Superseded) => ("gone", 0),
                    Err(_) => ("error", 0),
                };
                machine.rel_out = out.to_string();
                machine.rel_lock = lock;
                machine.attempt = attempt;
                machine.pc = "released".to_string();
            }
        }
        Ok(())
    }

    fn start_put(&mut self, client: i64) -> anyhow::Result<()> {
        let lock = lock_of(client, self.machine(client)?);
        let context = self.context();
        let nonces = self.nonces.clone();
        let machine = self.machine(client)?;
        let resource = Self::resource(client, uuid_of(machine.pay_gen));
        let expected = machine.expected;
        let mut insertion = None;
        let change: Change = Box::new(move |document: &mut StateDocument| {
            let put = ConditionalPut {
                resource: &resource,
                expected,
            };
            let insertion = *insertion.get_or_insert_with(|| insertion_of(&put));
            apply_conditional_put(document, &put, insertion)
        });
        machine.running = Some(Running::Mutate(Mutate::new(
            context,
            &lock.to_string(),
            change,
            nonces,
        )));
        machine.attempt = 0;
        self.run(client, None)
    }

    fn start_release(&mut self, client: i64) -> anyhow::Result<()> {
        let lock = lock_of(client, self.machine(client)?);
        let context = self.context();
        let nonces = self.nonces.clone();
        let machine = self.machine(client)?;
        machine.running = Some(Running::Release(Release::new(
            context,
            &lock.to_string(),
            nonces,
        )));
        machine.attempt = 0;
        self.run(client, None)
    }

    // -- the model's actions ----------------------------------------------------

    fn init(&mut self) {
        *self = Self::default();
    }

    fn begin(&mut self, client: i64) -> anyhow::Result<()> {
        let lock = lock_of(client, self.machine(client)?);
        let request = AcquireRequest {
            lock: DocumentLock {
                lock_identifier: lock.to_string(),
                holder: "mbt".to_string(),
                acquired_at: "2026-01-01T00:00:00Z".to_string(),
                cuenv_version: String::new(),
                last_change: None,
                extra: serde_json::Map::new(),
            },
            writer: None,
            fresh: StateDocument::new(&self.path, 1),
        };
        let context = self.context();
        let nonces = self.nonces.clone();
        let machine = self.machine(client)?;
        anyhow::ensure!(machine.pc == "idle", "begin from {}", machine.pc);
        machine.running = Some(Running::Acquire(Box::new(Acquire::new(
            context, request, nonces,
        ))));
        machine.attempt = 0;
        self.run(client, None)
    }

    fn read_step(&mut self, client: i64) -> anyhow::Result<()> {
        let found = self.read();
        self.run(client, Some(Input::Read(found.map(Box::new))))?;
        self.collect_garbage();
        Ok(())
    }

    fn respond(&mut self, client: i64) -> anyhow::Result<()> {
        let response = self
            .machine(client)?
            .response
            .take()
            .ok_or_else(|| anyhow::anyhow!("client {client} has no response"))?;
        self.run(client, Some(Input::Put(response)))?;
        self.collect_garbage();
        Ok(())
    }

    fn timeout(&mut self, client: i64) -> anyhow::Result<()> {
        self.run(
            client,
            Some(Input::Put(Err(S3Failure::Ambiguous(
                "timed out".to_string(),
            )))),
        )?;
        self.collect_garbage();
        Ok(())
    }

    fn list(&mut self, client: i64) -> anyhow::Result<()> {
        let stored = self
            .doc
            .as_ref()
            .and_then(|document| document.resource(&address()).ok().flatten());
        let machine = self.machine(client)?;
        anyhow::ensure!(machine.pc == "list", "list from {}", machine.pc);
        machine.expected = RecordVersion::of(stored.as_ref());
        // engine.rs `write_generation`.
        machine.pay_gen = match machine.expected {
            RecordVersion::Generation { generation, .. } => model_of_uuid(generation),
            RecordVersion::Absent => insertion_of_model(client, machine),
        };
        self.start_put(client)
    }

    fn finish_round(&mut self, client: i64) -> anyhow::Result<()> {
        let machine = self.machine(client)?;
        anyhow::ensure!(machine.pc == "released", "finish from {}", machine.pc);
        if machine.put_out != "ok" && machine.round < MAX_ROUNDS {
            let mut next = Machine::fresh();
            next.round = machine.round + 1;
            next.replay = true;
            next.expected = machine.expected;
            next.pay_gen = machine.pay_gen;
            *machine = next;
        } else {
            machine.pc = "done".to_string();
        }
        self.collect_garbage();
        Ok(())
    }

    fn deliver(&mut self, request: &RequestView) -> anyhow::Result<()> {
        let index = self
            .inflight
            .iter()
            .position(|candidate| {
                candidate.client == request.client
                    && candidate.nonce == request.nonce
                    && candidate.cond == request.cond
            })
            .ok_or_else(|| anyhow::anyhow!("no request in flight matches {request:?}"))?;
        let delivered = self.inflight.remove(index);
        let matches = if delivered.cond == ABSENT {
            self.doc.is_none()
        } else {
            self.doc.as_ref().is_some_and(|document| {
                i64::try_from(document.serial).unwrap_or(-1) == delivered.cond
            })
        };
        if matches {
            self.doc = Some(delivered.body);
        }
        let machine = self.machine(delivered.client)?;
        if machine.waiting == Some(delivered.nonce) {
            machine.response = Some(if matches {
                Ok(())
            } else {
                Err(S3Failure::PreconditionFailed)
            });
        }
        self.collect_garbage();
        Ok(())
    }

    fn force_unlock(&mut self) -> anyhow::Result<()> {
        let document = self
            .doc
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("force unlock without a document"))?;
        let held = document
            .lock
            .take()
            .ok_or_else(|| anyhow::anyhow!("force unlock without a lock"))?;
        document.last_released = Some(held.lock_identifier);
        document.advance(&OPERATOR.to_string());
        self.collect_garbage();
        Ok(())
    }
}

impl Driver for ProtocolDriver {
    type State = Observed;

    fn step(&mut self, step: &QuintStep) -> quint_connect::Result {
        switch!(step {
            init => self.init(),
            begin(c: i64) => self.begin(c)?,
            read(c: i64) => self.read_step(c)?,
            respond(c: i64) => self.respond(c)?,
            timeout(c: i64) => self.timeout(c)?,
            list(c: i64) => self.list(c)?,
            finish(c: i64) => self.finish_round(c)?,
            deliver(r: RequestView) => self.deliver(&r)?,
            forceUnlock => self.force_unlock()?,
            quiescent => {}
        })
    }
}

#[quint_run(
    spec = "../../specs/quint/object_state_cas.qnt",
    max_samples = 300,
    max_steps = 80
)]
#[ignore = "model-based test: needs quint on PATH; run by the cuenv-quint-mbt flake check"]
fn the_write_protocols_follow_the_model() -> impl Driver {
    ProtocolDriver::default()
}
