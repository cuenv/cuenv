//! Admission stays owned by the actual operation, even after a caller times out.

use crate::{CueEngineError, FrozenWorkerFailure, Result};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_OUTSTANDING: usize = 16;
static SLOTS: OnceLock<Arc<Slots>> = OnceLock::new();

struct Slots {
    active: AtomicUsize,
    limit: usize,
    admission: Mutex<()>,
    available: Condvar,
}

impl Slots {
    fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            active: AtomicUsize::new(0),
            limit,
            admission: Mutex::new(()),
            available: Condvar::new(),
        })
    }

    fn acquire(self: &Arc<Self>) -> Result<Permit> {
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < self.limit).then_some(active + 1)
            })
            .map_err(|_| failure(FrozenWorkerFailure::Capacity))?;
        Ok(Permit(Arc::clone(self)))
    }

    fn acquire_until(self: &Arc<Self>, deadline: Instant) -> Result<Permit> {
        let mut guard = self
            .admission
            .lock()
            .map_err(|_| failure(FrozenWorkerFailure::Stopped))?;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(failure(FrozenWorkerFailure::TimedOut));
            }
            if let Ok(permit) = self.acquire() {
                return Ok(permit);
            }
            (guard, _) = self
                .available
                .wait_timeout(guard, remaining)
                .map_err(|_| failure(FrozenWorkerFailure::Stopped))?;
        }
    }
}

pub struct Permit(Arc<Slots>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
        // Pair notifications with the admission mutex so a release cannot fall
        // between a waiter's capacity check and its transition into waiting.
        let _guard = self
            .0
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.0.available.notify_one();
    }
}

fn slots() -> &'static Arc<Slots> {
    SLOTS.get_or_init(|| Slots::new(MAX_OUTSTANDING))
}

/// Synchronous specialization uses the same capacity without another deadline.
pub fn admit() -> Result<Permit> {
    slots().acquire()
}

pub fn run<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    run_with_slots(slots(), crate::evaluation_timeout()?, operation)
}

fn failure(reason: FrozenWorkerFailure) -> CueEngineError {
    CueEngineError::FrozenWorker { reason }
}

fn run_with_slots<T, F>(slots: &Arc<Slots>, timeout: Duration, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| failure(FrozenWorkerFailure::InvalidDeadline))?;
    // Wait on the calling thread before creating a worker. Admission and CUE
    // execution share one deadline; timed-out workers retain their permits.
    let permit = slots.acquire_until(deadline)?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name("cue-frozen-evaluation".to_owned())
        .spawn(move || {
            let _permit = permit;
            let result = operation();
            // This channel has one buffered slot and exactly one send. After
            // timeout the abandoned result is dropped here without logging.
            let _ = sender.send(result);
        })
        .map_err(|_| failure(FrozenWorkerFailure::SpawnFailed))?;
    drop(worker);
    let remaining = deadline.saturating_duration_since(Instant::now());
    let result = receiver.recv_timeout(remaining).map_err(|error| {
        failure(match error {
            RecvTimeoutError::Timeout => FrozenWorkerFailure::TimedOut,
            RecvTimeoutError::Disconnected => FrozenWorkerFailure::Stopped,
        })
    })?;
    // A buffered response may be ready when a delayed caller resumes after its
    // deadline. Do not accept that late response as an on-time evaluation.
    if Instant::now() >= deadline {
        return Err(failure(FrozenWorkerFailure::TimedOut));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_retains_capacity_until_late_result_is_dropped() {
        struct LateResult(mpsc::Sender<()>);
        impl Drop for LateResult {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }

        let slots = Slots::new(1);
        let (release, gate) = mpsc::channel();
        let (dropped, observed) = mpsc::channel();
        let result = run_with_slots(&slots, Duration::from_millis(10), move || {
            let _ = gate.recv_timeout(Duration::from_secs(5));
            Ok(LateResult(dropped))
        });
        assert!(matches!(
            result,
            Err(CueEngineError::FrozenWorker {
                reason: FrozenWorkerFailure::TimedOut
            })
        ));
        assert!(matches!(
            slots.acquire(),
            Err(CueEngineError::FrozenWorker {
                reason: FrozenWorkerFailure::Capacity
            })
        ));
        assert_eq!(slots.active.load(Ordering::Acquire), 1);
        release.send(()).expect("release worker");
        observed
            .recv_timeout(Duration::from_secs(5))
            .expect("late result must be dropped");
        let deadline = Instant::now() + Duration::from_secs(5);
        while slots.active.load(Ordering::Acquire) != 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(slots.active.load(Ordering::Acquire), 0);
        assert_eq!(
            run_with_slots(&slots, Duration::from_secs(5), || Ok(42)).expect("recovered slot"),
            42
        );
    }

    #[test]
    fn synchronous_admission_and_workers_share_capacity() {
        let slots = Slots::new(1);
        let permit = slots.acquire().expect("synchronous admission");
        assert!(matches!(
            slots.acquire(),
            Err(CueEngineError::FrozenWorker {
                reason: FrozenWorkerFailure::Capacity
            })
        ));
        let error = run_with_slots(&slots, Duration::from_millis(10), || -> Result<()> {
            panic!("an exhausted pool must not start work")
        })
        .unwrap_err();
        assert!(matches!(
            error,
            CueEngineError::FrozenWorker {
                reason: FrozenWorkerFailure::TimedOut
            }
        ));
        drop(permit);
        assert!(run_with_slots(&slots, Duration::from_secs(5), || Ok(())).is_ok());
    }

    #[test]
    fn initial_operation_waits_for_capacity_without_starting_another_worker() {
        let slots = Slots::new(1);
        let permit = slots.acquire().expect("occupy slot");
        let waiting_slots = Arc::clone(&slots);
        let (entered, entry) = mpsc::channel();
        let (completed, result) = mpsc::channel();
        let caller = std::thread::spawn(move || {
            entered.send(()).expect("announce admission attempt");
            let operation = run_with_slots(&waiting_slots, Duration::from_secs(5), || Ok(42));
            completed.send(operation).expect("return operation result");
        });
        entry
            .recv_timeout(Duration::from_secs(5))
            .expect("caller entered");
        assert_eq!(slots.active.load(Ordering::Acquire), 1);
        assert!(result.try_recv().is_err());
        drop(permit);
        assert_eq!(
            result
                .recv_timeout(Duration::from_secs(5))
                .expect("waiting caller resumed")
                .expect("operation completed"),
            42
        );
        caller.join().expect("caller completed");
    }

    #[test]
    fn invalid_deadline_and_worker_errors_exclude_values() {
        let slots = Slots::new(1);
        let error = run_with_slots(&slots, Duration::MAX, || Ok(())).unwrap_err();
        assert!(matches!(
            error,
            CueEngineError::FrozenWorker {
                reason: FrozenWorkerFailure::InvalidDeadline
            }
        ));
        assert_eq!(slots.active.load(Ordering::Acquire), 0);
        assert!(matches!(
            run_with_slots(&slots, Duration::ZERO, || Ok("xy, true, 13.25")),
            Err(CueEngineError::FrozenWorker {
                reason: FrozenWorkerFailure::TimedOut
            })
        ));
        for reason in [
            FrozenWorkerFailure::TimedOut,
            FrozenWorkerFailure::Capacity,
            FrozenWorkerFailure::SpawnFailed,
            FrozenWorkerFailure::Stopped,
            FrozenWorkerFailure::InvalidDeadline,
        ] {
            let error = failure(reason);
            assert_eq!(error.to_string(), format!("Frozen CUE evaluation {reason}"));
            assert!(!format!("{error:?}").contains("secret"));
        }
    }
}
