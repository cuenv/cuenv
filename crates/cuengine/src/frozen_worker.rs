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
        match self.0.admission.lock() {
            Ok(_guard) => self.0.available.notify_one(),
            // Keep the error's guard until after notification: the lock still
            // orders this wakeup with waiters entering the condition variable.
            Err(_poisoned) => self.0.available.notify_all(),
        }
    }
}

fn slots() -> &'static Arc<Slots> {
    SLOTS.get_or_init(|| Slots::new(MAX_OUTSTANDING))
}

/// Synchronous specialization shares the capacity and waits for it as a worker
/// does. Its callers bound the operation itself.
pub fn admit() -> Result<Permit> {
    admit_with_slots(slots(), crate::evaluation_timeout()?)
}

fn admit_with_slots(slots: &Arc<Slots>, timeout: Duration) -> Result<Permit> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| failure(FrozenWorkerFailure::InvalidDeadline))?;
    slots.acquire_until(deadline)
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

    fn poison_admission(slots: &Arc<Slots>) {
        let slots = Arc::clone(slots);
        let poisoner = std::thread::spawn(move || {
            let _guard = slots.admission.lock().expect("lock admission");
            panic!("poison admission for the test");
        });
        assert!(poisoner.join().is_err(), "the admission lock was poisoned");
    }

    #[test]
    fn poisoned_admission_stops_new_callers_after_permit_release() {
        let slots = Slots::new(1);
        let permit = slots.acquire().expect("occupy slot");
        poison_admission(&slots);

        drop(permit);

        assert_eq!(slots.active.load(Ordering::Acquire), 0);
        assert!(matches!(
            admit_with_slots(&slots, Duration::from_secs(5)),
            Err(CueEngineError::FrozenWorker {
                reason: FrozenWorkerFailure::Stopped
            })
        ));
    }

    #[test]
    fn releasing_a_permit_wakes_all_waiters_when_admission_is_poisoned() {
        const WAITERS: usize = 3;

        let slots = Slots::new(1);
        let permit = slots.acquire().expect("occupy slot");
        let (ready, waiting) = mpsc::channel();
        let (finished, results) = mpsc::channel();
        let mut waiters = Vec::new();
        for _ in 0..WAITERS {
            let slots = Arc::clone(&slots);
            let ready = ready.clone();
            let finished = finished.clone();
            waiters.push(std::thread::spawn(move || {
                let mut guard = slots.admission.lock().expect("lock admission");
                ready.send(()).expect("announce wait");
                loop {
                    if let Ok((next_guard, _)) =
                        slots.available.wait_timeout(guard, Duration::from_secs(30))
                    {
                        guard = next_guard;
                    } else {
                        finished.send(()).expect("report poisoned admission");
                        break;
                    }
                }
            }));
        }
        for _ in 0..WAITERS {
            waiting
                .recv_timeout(Duration::from_secs(5))
                .expect("waiter acquired admission");
        }

        // Acquiring admission to poison it also proves the last waiter has
        // atomically released the lock and entered the condition-variable wait.
        poison_admission(&slots);
        drop(permit);

        for _ in 0..WAITERS {
            results
                .recv_timeout(Duration::from_secs(5))
                .expect("poisoned waiter was woken");
        }
        for waiter in waiters {
            waiter.join().expect("waiter completed");
        }
        assert_eq!(slots.active.load(Ordering::Acquire), 0);
    }

    #[test]
    fn permit_release_during_unwinding_does_not_panic_on_poison() {
        let slots = Slots::new(1);
        let permit = slots.acquire().expect("occupy slot");
        poison_admission(&slots);

        let operation = std::thread::spawn(move || {
            let _permit = permit;
            panic!("operation failed while holding a permit");
        });

        assert!(operation.join().is_err(), "the operation panicked");
        assert_eq!(slots.active.load(Ordering::Acquire), 0);
    }

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
    fn synchronous_admission_waits_for_capacity_until_its_deadline() {
        let slots = Slots::new(1);
        let permit = slots.acquire().expect("occupy slot");
        assert!(matches!(
            admit_with_slots(&slots, Duration::from_millis(10)),
            Err(CueEngineError::FrozenWorker {
                reason: FrozenWorkerFailure::TimedOut
            })
        ));
        let waiting_slots = Arc::clone(&slots);
        let caller = std::thread::spawn(move || {
            admit_with_slots(&waiting_slots, Duration::from_secs(30)).map(drop)
        });
        drop(permit);
        assert!(caller.join().expect("caller completed").is_ok());
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
