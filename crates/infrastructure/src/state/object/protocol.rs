//! The write protocol of [`ObjectStateStore`](super::ObjectStateStore),
//! without I/O.
//!
//! Each operation that changes `state.json` (taking the lock, changing the
//! document under it, releasing the lock) is a state machine here. It asks
//! for one [`Action`] at a time (read the object, write it on a condition,
//! wait) and is resumed with the [`Input`] that action produced. The store
//! only carries the actions out.
//!
//! Keeping the decisions free of I/O is what lets them be checked against
//! the Quint model in `specs/quint/object_state_cas.qnt` step by step
//! (`mbt` tests), and by Kani over every input it can take (`kani`
//! proofs). Each state below is a state of that model.

use std::time::Duration;

use super::client::{Condition, S3Failure};
use super::document::{DocumentLock, DocumentWriter, StateDocument};
use crate::error::{InfrastructureError, Result};
use crate::state::UnlockOutcome;
use crate::tenant::TenantKey;

/// The wait after a 429 without `Retry-After`.
pub const RATE_LIMIT_WAIT: Duration = Duration::from_secs(1);

/// The wait before attempt `attempt + 1` after an unresolved write.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(200 * u64::from(attempt.saturating_add(1)))
}

/// What a protocol needs done next.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Read `state.json` at the top of an attempt.
    Read,
    /// Read `state.json` back to learn whether an earlier write landed.
    ReadBack,
    /// Fail unless an absent `state.json` was never written (no history,
    /// not listed by a migration manifest).
    CheckNotVanished,
    /// Write `body` on `condition`.
    Put {
        /// The document to write.
        body: Box<StateDocument>,
        /// `If-Match` or `If-None-Match`.
        condition: Condition,
    },
    /// Sleep, then resume with [`Input::Waited`].
    Wait(Duration),
}

/// What an [`Action`] produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// The object and its ETag, or `None` when it is absent.
    Read(Option<Box<Found>>),
    /// [`Action::CheckNotVanished`] passed.
    NotVanished,
    /// The write's outcome.
    Put(std::result::Result<(), S3Failure>),
    /// [`Action::Wait`] elapsed.
    Waited,
}

/// An object read from the bucket and its ETag.
pub type Found = (StateDocument, String);

/// The next action, or the operation's result.
#[derive(Debug, Clone, PartialEq)]
pub enum Step<T> {
    /// Carry out this action and resume.
    Next(Action),
    /// Finished.
    Done(T),
}

impl<T> Step<T> {
    /// The same step with its result mapped.
    #[cfg(test)]
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> Step<U> {
        match self {
            Self::Next(action) => Step::Next(action),
            Self::Done(output) => Step::Done(map(output)),
        }
    }
}

/// A write protocol: started once, then resumed with each action's input.
pub trait Protocol {
    /// What the operation returns.
    type Output;

    /// Whether a tombstone found by [`Action::Read`] ends the operation.
    fn refuses_tombstones(&self) -> bool {
        true
    }

    /// The first step.
    ///
    /// # Errors
    ///
    /// As for [`Protocol::resume`].
    fn start(&mut self) -> Result<Step<Self::Output>>;

    /// Continue with what the last action produced.
    ///
    /// # Errors
    ///
    /// Returns the operation's failure: a lost or held lock, a vanished
    /// object, a refused write, or attempts exhausted.
    fn resume(&mut self, input: Input) -> Result<Step<Self::Output>>;
}

/// A source of `writer` nonces. Random in the store; chosen by the model in
/// tests.
pub trait Nonces {
    /// The next nonce.
    fn next(&mut self) -> String;
}

/// Random nonces, for the store.
#[derive(Debug, Default)]
pub struct RandomNonces;

impl Nonces for RandomNonces {
    fn next(&mut self) -> String {
        uuid::Uuid::new_v4().simple().to_string()
    }
}

/// What every protocol needs to name in its errors.
#[derive(Debug, Clone)]
pub struct Context {
    /// The tenant whose state this is.
    pub tenant: TenantKey,
    /// The key of `state.json`.
    pub key: String,
    /// Where `state.json` lives, for display (`s3://bucket/key`).
    pub location: String,
    /// Attempts before giving up.
    pub attempts: u32,
}

impl Context {
    fn vanished(&self) -> InfrastructureError {
        InfrastructureError::state(format!(
            "the state object {} vanished while its lock was held; nothing was recreated. \
             Restore it from history (`state restore`) or from bucket versioning",
            self.location
        ))
    }

    fn refused(&self, message: &str) -> InfrastructureError {
        InfrastructureError::state(format!(
            "object storage refused writing {}: {message}",
            self.key
        ))
    }

    fn lock_lost(&self, lock_identifier: &str) -> InfrastructureError {
        InfrastructureError::LockLost {
            tenant: self.tenant.to_string(),
            lock_identifier: lock_identifier.to_string(),
        }
    }
}

/// `document` with the next serial and `writer`.
fn advanced(mut document: StateDocument, writer: &str) -> StateDocument {
    document.advance(writer);
    document
}

/// Whether `document` holds the lock `identifier`.
pub fn holds(document: &StateDocument, identifier: &str) -> bool {
    document
        .lock
        .as_ref()
        .is_some_and(|held| held.lock_identifier == identifier)
}

/// The wait a 429 asks for.
fn rate_limit_wait(wait: Option<Duration>) -> Duration {
    wait.unwrap_or(RATE_LIMIT_WAIT)
}

// -- mutate ------------------------------------------------------------------

/// What a mutation did to the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutation {
    /// The document changed and must be written.
    Write,
    /// Nothing to write.
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutateState {
    /// Reading at the top of an attempt.
    Reading,
    /// Waiting for the write with this nonce.
    Writing { nonce: String },
    /// Reading back after an unresolved write with this nonce.
    ReadingBack { nonce: String },
    /// Waiting before the next attempt.
    Pausing,
}

/// Apply `change` to the document under the lock `lock`.
pub struct Mutate<Change, N> {
    context: Context,
    lock: String,
    change: Change,
    nonces: N,
    attempt: u32,
    state: MutateState,
    last_failure: String,
}

impl<Change, N> Mutate<Change, N>
where
    Change: FnMut(&mut StateDocument) -> Result<Mutation>,
    N: Nonces,
{
    pub fn new(context: Context, lock: &str, change: Change, nonces: N) -> Self {
        Self {
            context,
            lock: lock.to_string(),
            change,
            nonces,
            attempt: 0,
            state: MutateState::Reading,
            last_failure: String::new(),
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> &MutateState {
        &self.state
    }

    #[cfg(test)]
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Begin attempt `self.attempt`, or give up.
    fn attempt_or_give_up(&mut self) -> Result<Step<()>> {
        if self.attempt >= self.context.attempts {
            return Err(InfrastructureError::state(format!(
                "could not write {} after {} attempts: {}",
                self.context.key, self.context.attempts, self.last_failure
            )));
        }
        self.state = MutateState::Reading;
        Ok(Step::Next(Action::Read))
    }

    fn next_attempt(&mut self, failure: impl Into<String>) -> Result<Step<()>> {
        self.last_failure = failure.into();
        self.attempt += 1;
        self.attempt_or_give_up()
    }

    fn on_read(&mut self, found: Option<Box<Found>>) -> Result<Step<()>> {
        let (document, etag) = *found.ok_or_else(|| self.context.vanished())?;
        if !holds(&document, &self.lock) {
            return Err(self.context.lock_lost(&self.lock));
        }
        document.require_writable(&self.context.key)?;
        let mut next = document;
        if (self.change)(&mut next)? == Mutation::Unchanged {
            return Ok(Step::Done(()));
        }
        let nonce = self.nonces.next();
        self.state = MutateState::Writing {
            nonce: nonce.clone(),
        };
        Ok(Step::Next(Action::Put {
            body: Box::new(advanced(next, &nonce)),
            condition: Condition::Matches(etag),
        }))
    }

    fn on_put(
        &mut self,
        nonce: String,
        outcome: std::result::Result<(), S3Failure>,
    ) -> Result<Step<()>> {
        match outcome {
            Ok(()) => Ok(Step::Done(())),
            Err(S3Failure::PreconditionFailed) => {
                self.next_attempt("the object changed concurrently")
            }
            Err(S3Failure::RateLimited(wait)) => {
                self.last_failure = "rate limited".to_string();
                self.state = MutateState::Pausing;
                Ok(Step::Next(Action::Wait(rate_limit_wait(wait))))
            }
            Err(S3Failure::Ambiguous(message)) => {
                self.last_failure = message;
                self.state = MutateState::ReadingBack { nonce };
                Ok(Step::Next(Action::ReadBack))
            }
            Err(S3Failure::Missing) => Err(self.context.vanished()),
            Err(S3Failure::Permanent(message)) => Err(self.context.refused(&message)),
        }
    }

    fn on_read_back(&mut self, nonce: &str, found: Option<Box<Found>>) -> Result<Step<()>> {
        match found.map(|found| found.0) {
            Some(stored) if stored.writer == nonce => Ok(Step::Done(())),
            Some(_) => {
                self.state = MutateState::Pausing;
                Ok(Step::Next(Action::Wait(backoff(self.attempt))))
            }
            None => Err(self.context.vanished()),
        }
    }
}

impl<Change, N> Protocol for Mutate<Change, N>
where
    Change: FnMut(&mut StateDocument) -> Result<Mutation>,
    N: Nonces,
{
    type Output = ();

    fn start(&mut self) -> Result<Step<()>> {
        self.attempt_or_give_up()
    }

    fn resume(&mut self, input: Input) -> Result<Step<()>> {
        match (
            std::mem::replace(&mut self.state, MutateState::Reading),
            input,
        ) {
            (MutateState::Reading, Input::Read(found)) => self.on_read(found),
            (MutateState::Writing { nonce }, Input::Put(outcome)) => self.on_put(nonce, outcome),
            (MutateState::ReadingBack { nonce }, Input::Read(found)) => {
                self.on_read_back(&nonce, found)
            }
            (MutateState::Pausing, Input::Waited) => {
                let failure = std::mem::take(&mut self.last_failure);
                self.next_attempt(failure)
            }
            (state, input) => Err(unexpected("mutate", &state, &input)),
        }
    }
}

fn unexpected(operation: &str, state: &impl std::fmt::Debug, input: &Input) -> InfrastructureError {
    InfrastructureError::state(format!(
        "internal error: {operation} in state {state:?} cannot take {input:?}"
    ))
}

// -- acquire -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireState {
    /// Reading at the top of an attempt.
    Reading,
    /// Checking that an absent object was never written.
    CheckingVanished,
    /// Waiting for a create (`If-None-Match: *`) with this nonce.
    Creating { nonce: String, create_attempt: u32 },
    /// Reading back after a create was refused (412).
    CreateRefused { nonce: String },
    /// Reading back after a create was not resolved.
    CreateReadingBack { nonce: String, create_attempt: u32 },
    /// Waiting before sending the create again.
    CreatePausing { nonce: String, create_attempt: u32 },
    /// Waiting for the lock write with this nonce, which writes `serial`.
    Writing { nonce: String, serial: u64 },
    /// Reading back after a lock write was not resolved.
    ReadingBack,
    /// Waiting before the next attempt.
    Pausing,
}

/// What a new lock is taken with.
pub struct AcquireRequest {
    /// The lock to record.
    pub lock: DocumentLock,
    /// The instance to record as the last writer, if any.
    pub writer: Option<DocumentWriter>,
    /// The document to create for a tenant that has none (serial 1).
    pub fresh: StateDocument,
}

/// Take the lock `request.lock` (creating the document for a new tenant).
/// Returns the serial of the document holding the lock.
pub struct Acquire<N> {
    context: Context,
    request: AcquireRequest,
    nonces: N,
    attempt: u32,
    state: AcquireState,
    last_failure: String,
}

impl<N: Nonces> Acquire<N> {
    pub fn new(context: Context, request: AcquireRequest, nonces: N) -> Self {
        Self {
            context,
            request,
            nonces,
            attempt: 0,
            state: AcquireState::Reading,
            last_failure: String::new(),
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> &AcquireState {
        &self.state
    }

    #[cfg(test)]
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    fn identifier(&self) -> &str {
        &self.request.lock.lock_identifier
    }

    fn attempt_or_give_up(&mut self) -> Result<Step<u64>> {
        if self.attempt >= self.context.attempts {
            return Err(InfrastructureError::state(format!(
                "could not acquire the state lock for {}: {}; the lock may have been acquired \
                 as {}; if so, release that lock by its identifier",
                self.context.tenant,
                self.last_failure,
                self.identifier()
            )));
        }
        self.state = AcquireState::Reading;
        Ok(Step::Next(Action::Read))
    }

    fn next_attempt(&mut self) -> Result<Step<u64>> {
        self.attempt += 1;
        self.attempt_or_give_up()
    }

    fn pause(&mut self, wait: Duration) -> Step<u64> {
        self.state = AcquireState::Pausing;
        Step::Next(Action::Wait(wait))
    }

    fn create(&mut self, nonce: String, create_attempt: u32) -> Result<Step<u64>> {
        if create_attempt >= self.context.attempts {
            return Err(InfrastructureError::state(format!(
                "could not create {} after {} attempts: {}",
                self.context.key, self.context.attempts, self.last_failure
            )));
        }
        let mut document = self.request.fresh.clone();
        document.lock = Some(self.request.lock.clone());
        document.last_writer.clone_from(&self.request.writer);
        document.writer.clone_from(&nonce);
        document.written_by = super::document::written_by();
        self.state = AcquireState::Creating {
            nonce,
            create_attempt,
        };
        Ok(Step::Next(Action::Put {
            body: Box::new(document),
            condition: Condition::Absent,
        }))
    }

    fn on_read(&mut self, found: Option<Box<Found>>) -> Result<Step<u64>> {
        let Some(found) = found else {
            self.state = AcquireState::CheckingVanished;
            return Ok(Step::Next(Action::CheckNotVanished));
        };
        let (document, etag) = *found;
        if let Some(held) = &document.lock {
            if held.lock_identifier == self.identifier() {
                // An earlier attempt whose response was lost did commit.
                return Ok(Step::Done(document.serial));
            }
            let information = held.information();
            return Err(InfrastructureError::Locked {
                tenant: self.context.tenant.to_string(),
                lock_identifier: information.lock_identifier,
                holder: information.holder,
                acquired_at: information.acquired_at,
            });
        }
        document.require_writable(&self.context.key)?;
        let mut next = document;
        next.lock = Some(self.request.lock.clone());
        if self.request.writer.is_some() {
            next.last_writer.clone_from(&self.request.writer);
        }
        let nonce = self.nonces.next();
        let body = advanced(next, &nonce);
        self.state = AcquireState::Writing {
            nonce,
            serial: body.serial,
        };
        Ok(Step::Next(Action::Put {
            body: Box::new(body),
            condition: Condition::Matches(etag),
        }))
    }

    fn on_create(
        &mut self,
        nonce: String,
        create_attempt: u32,
        outcome: std::result::Result<(), S3Failure>,
    ) -> Result<Step<u64>> {
        match outcome {
            Ok(()) => Ok(Step::Done(1)),
            Err(S3Failure::PreconditionFailed) => {
                self.state = AcquireState::CreateRefused { nonce };
                Ok(Step::Next(Action::ReadBack))
            }
            Err(S3Failure::RateLimited(wait)) => {
                self.last_failure = "rate limited".to_string();
                self.state = AcquireState::CreatePausing {
                    nonce,
                    create_attempt,
                };
                Ok(Step::Next(Action::Wait(rate_limit_wait(wait))))
            }
            Err(S3Failure::Ambiguous(message)) => {
                self.last_failure = message;
                self.state = AcquireState::CreateReadingBack {
                    nonce,
                    create_attempt,
                };
                Ok(Step::Next(Action::ReadBack))
            }
            Err(failure) => Err(failure.into_error(&format!("creating {}", self.context.key))),
        }
    }

    fn on_lock_write(
        &mut self,
        serial: u64,
        outcome: std::result::Result<(), S3Failure>,
    ) -> Result<Step<u64>> {
        match outcome {
            Ok(()) => Ok(Step::Done(serial)),
            Err(S3Failure::PreconditionFailed) => {
                self.last_failure = "the object changed concurrently".to_string();
                self.next_attempt()
            }
            Err(S3Failure::RateLimited(wait)) => {
                self.last_failure = "rate limited".to_string();
                Ok(self.pause(rate_limit_wait(wait)))
            }
            Err(S3Failure::Ambiguous(message)) => {
                self.last_failure = message;
                self.state = AcquireState::ReadingBack;
                Ok(Step::Next(Action::ReadBack))
            }
            Err(S3Failure::Missing) => {
                self.last_failure = "the object vanished".to_string();
                self.next_attempt()
            }
            Err(S3Failure::Permanent(message)) => Err(self.context.refused(&message)),
        }
    }
}

impl<N: Nonces> Protocol for Acquire<N> {
    type Output = u64;

    fn start(&mut self) -> Result<Step<u64>> {
        self.attempt_or_give_up()
    }

    fn resume(&mut self, input: Input) -> Result<Step<u64>> {
        match (
            std::mem::replace(&mut self.state, AcquireState::Reading),
            input,
        ) {
            (AcquireState::Reading, Input::Read(found)) => self.on_read(found),
            (AcquireState::CheckingVanished, Input::NotVanished) => {
                let nonce = self.nonces.next();
                self.create(nonce, 0)
            }
            (
                AcquireState::Creating {
                    nonce,
                    create_attempt,
                },
                Input::Put(outcome),
            ) => self.on_create(nonce, create_attempt, outcome),
            // Another writer created it first, unless the refused create was
            // a retry of one of ours that had landed.
            (AcquireState::CreateRefused { nonce }, Input::Read(found)) => {
                if found.is_some_and(|found| found.0.writer == nonce) {
                    return Ok(Step::Done(1));
                }
                self.next_attempt()
            }
            (
                AcquireState::CreateReadingBack {
                    nonce,
                    create_attempt,
                },
                Input::Read(found),
            ) => match found.map(|found| found.0) {
                Some(stored) if stored.writer == nonce => Ok(Step::Done(1)),
                Some(_) => self.next_attempt(),
                None => {
                    self.state = AcquireState::CreatePausing {
                        nonce,
                        create_attempt,
                    };
                    Ok(Step::Next(Action::Wait(backoff(create_attempt))))
                }
            },
            (
                AcquireState::CreatePausing {
                    nonce,
                    create_attempt,
                },
                Input::Waited,
            ) => self.create(nonce, create_attempt + 1),
            (AcquireState::Writing { serial, .. }, Input::Put(outcome)) => {
                self.on_lock_write(serial, outcome)
            }
            (AcquireState::ReadingBack, Input::Read(found)) => match found.map(|found| found.0) {
                Some(stored) if holds(&stored, self.identifier()) => Ok(Step::Done(stored.serial)),
                _ => Ok(self.pause(backoff(self.attempt))),
            },
            (AcquireState::Pausing, Input::Waited) => self.next_attempt(),
            (state, input) => Err(unexpected("acquire", &state, &input)),
        }
    }
}

// -- release -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ReleaseState {
    /// Reading at the top of an attempt.
    Reading,
    /// Waiting for the release write.
    Writing { released: Box<StateDocument> },
    /// Reading back after a release write was not resolved.
    ReadingBack,
    /// Waiting before the next attempt.
    Pausing,
}

/// What [`Release`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum ReleaseOutcome {
    /// The lock is released; the document shows it released.
    Released(Box<StateDocument>),
    /// The lock was not held: nothing was sent.
    NotHeld,
    /// A release was sent, and the lock is gone, but the stored state does
    /// not show it released.
    Superseded,
}

impl ReleaseOutcome {
    /// The lock is gone without a document showing it released.
    const fn gone(sent: bool) -> Self {
        if sent {
            Self::Superseded
        } else {
            Self::NotHeld
        }
    }

    pub const fn unlock_outcome(&self) -> UnlockOutcome {
        match self {
            Self::Released(_) => UnlockOutcome::Released,
            Self::NotHeld => UnlockOutcome::NotHeld,
            Self::Superseded => UnlockOutcome::Superseded,
        }
    }
}

/// Whether `document` shows the lock `identifier` released: no lock is held
/// and it was the last released.
fn shows_released(document: &StateDocument, identifier: &str) -> bool {
    document.lock.is_none() && document.last_released.as_deref() == Some(identifier)
}

/// Clear the lock `identifier` and record it as released.
///
/// Once this has sent a release, a re-read that no longer shows the lock is
/// not by itself "released": that release may have landed after its
/// response was lost (a retry then fails with 412), but an operator may
/// instead have force-unlocked it, and another run taken the lock since.
/// Only a document that shows this lock released (no lock, `lastReleased`
/// naming it) is [`ReleaseOutcome::Released`] and returned for a snapshot;
/// anything else is [`ReleaseOutcome::Superseded`].
/// `specs/quint/object_state_cas.qnt` checks this (`unlockReportsTruth`).
pub struct Release<N> {
    context: Context,
    identifier: String,
    nonces: N,
    attempt: u32,
    sent: bool,
    state: ReleaseState,
    last_failure: String,
}

impl<N: Nonces> Release<N> {
    pub fn new(context: Context, identifier: &str, nonces: N) -> Self {
        Self {
            context,
            identifier: identifier.to_string(),
            nonces,
            attempt: 0,
            sent: false,
            state: ReleaseState::Reading,
            last_failure: String::new(),
        }
    }

    #[cfg(test)]
    pub fn state(&self) -> &ReleaseState {
        &self.state
    }

    #[cfg(test)]
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    fn attempt_or_give_up(&mut self) -> Result<Step<ReleaseOutcome>> {
        if self.attempt >= self.context.attempts {
            return Err(InfrastructureError::state(format!(
                "could not release the state lock {} of {}: {}",
                self.identifier, self.context.tenant, self.last_failure
            )));
        }
        self.state = ReleaseState::Reading;
        Ok(Step::Next(Action::Read))
    }

    fn next_attempt(&mut self) -> Result<Step<ReleaseOutcome>> {
        self.attempt += 1;
        self.attempt_or_give_up()
    }

    fn pause(&mut self, wait: Duration) -> Step<ReleaseOutcome> {
        self.state = ReleaseState::Pausing;
        Step::Next(Action::Wait(wait))
    }

    fn on_read(&mut self, found: Option<Box<Found>>) -> Result<Step<ReleaseOutcome>> {
        let Some(found) = found else {
            return Ok(Step::Done(ReleaseOutcome::gone(self.sent)));
        };
        let (document, etag) = *found;
        if !holds(&document, &self.identifier) {
            return Ok(Step::Done(if !self.sent {
                ReleaseOutcome::NotHeld
            } else if shows_released(&document, &self.identifier) {
                ReleaseOutcome::Released(Box::new(document))
            } else {
                ReleaseOutcome::Superseded
            }));
        }
        document.require_writable(&self.context.key)?;
        let mut next = document;
        next.lock = None;
        next.last_released = Some(self.identifier.clone());
        let nonce = self.nonces.next();
        let body = advanced(next, &nonce);
        self.sent = true;
        self.state = ReleaseState::Writing {
            released: Box::new(body.clone()),
        };
        Ok(Step::Next(Action::Put {
            body: Box::new(body),
            condition: Condition::Matches(etag),
        }))
    }

    fn on_put(
        &mut self,
        released: StateDocument,
        outcome: std::result::Result<(), S3Failure>,
    ) -> Result<Step<ReleaseOutcome>> {
        match outcome {
            Ok(()) => Ok(Step::Done(ReleaseOutcome::Released(Box::new(released)))),
            Err(S3Failure::PreconditionFailed) => {
                self.last_failure = "the object changed concurrently".to_string();
                self.next_attempt()
            }
            Err(S3Failure::RateLimited(wait)) => {
                self.last_failure = "rate limited".to_string();
                Ok(self.pause(rate_limit_wait(wait)))
            }
            Err(S3Failure::Ambiguous(message)) => {
                self.last_failure = message;
                self.state = ReleaseState::ReadingBack;
                Ok(Step::Next(Action::ReadBack))
            }
            Err(S3Failure::Missing) => Ok(Step::Done(ReleaseOutcome::gone(self.sent))),
            Err(S3Failure::Permanent(message)) => Err(self.context.refused(&message)),
        }
    }
}

impl<N: Nonces> Protocol for Release<N> {
    type Output = ReleaseOutcome;

    fn refuses_tombstones(&self) -> bool {
        false
    }

    fn start(&mut self) -> Result<Step<ReleaseOutcome>> {
        self.attempt_or_give_up()
    }

    fn resume(&mut self, input: Input) -> Result<Step<ReleaseOutcome>> {
        match (
            std::mem::replace(&mut self.state, ReleaseState::Reading),
            input,
        ) {
            (ReleaseState::Reading, Input::Read(found)) => self.on_read(found),
            (ReleaseState::Writing { released }, Input::Put(outcome)) => {
                self.on_put(*released, outcome)
            }
            (ReleaseState::ReadingBack, Input::Read(found)) => match found.map(|found| found.0) {
                Some(stored) if shows_released(&stored, &self.identifier) => {
                    Ok(Step::Done(ReleaseOutcome::Released(Box::new(stored))))
                }
                _ => Ok(self.pause(backoff(self.attempt))),
            },
            (ReleaseState::Pausing, Input::Waited) => self.next_attempt(),
            (state, input) => Err(unexpected("release", &state, &input)),
        }
    }
}
