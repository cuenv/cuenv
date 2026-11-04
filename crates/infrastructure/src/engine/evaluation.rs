//! Bounded waiting for pure, immutable configuration specialization.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use tokio::sync::{Semaphore, oneshot};

use crate::cancellation::Cancellation;
use crate::error::{InfrastructureError, Result};
use crate::references::{
    ConfigurationEvaluator, ConfigurationSpecialization, ConfigurationVerification,
    NativeReference, OutputIdentity, SourceAddress, SpecializedConfiguration,
};
use crate::type_system::Value;

/// Workers may outlive a timeout or interrupt, but cannot start provider calls
/// or write state. Bound them across engines until each worker actually exits.
const MAX_OUTSTANDING_EVALUATIONS: usize = 16;
static EVALUATION_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();

pub(super) struct Specialization {
    pub(super) evaluator: Arc<dyn ConfigurationEvaluator>,
    pub(super) consumer: SourceAddress,
    pub(super) sources: BTreeMap<OutputIdentity, Value>,
    pub(super) deferred: Vec<NativeReference>,
}

/// Real outputs to check against the instance on a bounded worker.
pub(super) struct Verification {
    pub(super) evaluator: Arc<dyn ConfigurationEvaluator>,
    pub(super) sources: BTreeMap<OutputIdentity, Value>,
}

fn slots() -> &'static Arc<Semaphore> {
    EVALUATION_SLOTS.get_or_init(|| Arc::new(Semaphore::new(MAX_OUTSTANDING_EVALUATIONS)))
}

pub(super) async fn specialize(
    request: Specialization,
    cancellation: &Cancellation,
) -> Result<SpecializedConfiguration> {
    specialize_with_slots(request, cancellation, slots()).await
}

pub(super) async fn verify(request: Verification, cancellation: &Cancellation) -> Result<()> {
    let timeout = request.evaluator.timeout();
    bounded(
        Bounded {
            subject: "the CUE configuration".to_string(),
            timeout,
            work: move || {
                request.evaluator.verify(&ConfigurationVerification {
                    sources: &request.sources,
                })
            },
        },
        cancellation,
        slots(),
    )
    .await
}

async fn specialize_with_slots(
    request: Specialization,
    cancellation: &Cancellation,
    slots: &Arc<Semaphore>,
) -> Result<SpecializedConfiguration> {
    let subject = request.consumer.to_string();
    let timeout = request.evaluator.timeout();
    bounded(
        Bounded {
            subject,
            timeout,
            work: move || {
                request.evaluator.specialize(&ConfigurationSpecialization {
                    consumer: &request.consumer,
                    sources: &request.sources,
                    deferred: &request.deferred,
                })
            },
        },
        cancellation,
        slots,
    )
    .await
}

struct Bounded<Work> {
    subject: String,
    timeout: std::time::Duration,
    work: Work,
}

async fn bounded<Output, Work>(
    request: Bounded<Work>,
    cancellation: &Cancellation,
    slots: &Arc<Semaphore>,
) -> Result<Output>
where
    Output: Send + 'static,
    Work: FnOnce() -> Result<Output> + Send + 'static,
{
    let Bounded {
        subject,
        timeout,
        work,
    } = request;
    let failure = |reason| InfrastructureError::ConfigurationEvaluation {
        consumer: subject.clone(),
        reason,
    };
    if cancellation.is_stop_requested() {
        return Err(InfrastructureError::InterruptedWhilePlanning);
    }
    // Start the budget before admission; a full pool fails immediately rather
    // than holding the lock while waiting for a stuck evaluator.
    let deadline = tokio::time::Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| failure("has an unsupported timeout"))?;
    let permit = Arc::clone(slots)
        .try_acquire_owned()
        .map_err(|_| failure("cannot start because the evaluation worker limit is reached"))?;
    if cancellation.is_stop_requested() {
        return Err(InfrastructureError::InterruptedWhilePlanning);
    }
    let (sender, receiver) = oneshot::channel();
    let worker = std::thread::Builder::new()
        .name("cuenv-configuration-evaluation".to_string())
        .spawn(move || {
            let _permit = permit;
            let result = work();
            // An abandoned result must not log values or trigger any work.
            let _ = sender.send(result);
        })
        .map_err(|_| failure("could not start its worker"))?;
    drop(worker);
    tokio::select! {
        biased;
        () = cancellation.stopped() => Err(InfrastructureError::InterruptedWhilePlanning),
        () = tokio::time::sleep_until(deadline) => Err(failure("timed out")),
        result = receiver => {
            if cancellation.is_stop_requested() {
                return Err(InfrastructureError::InterruptedWhilePlanning);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(failure("timed out"));
            }
            result.map_err(|_| failure("worker stopped unexpectedly"))?
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    use super::*;
    use crate::state::ResourceAddress;

    struct BlockingEvaluator {
        entered: AtomicBool,
        gate: (Mutex<bool>, Condvar),
        timeout: Duration,
    }

    impl BlockingEvaluator {
        fn new(timeout: Duration) -> Self {
            Self {
                entered: AtomicBool::new(false),
                gate: (Mutex::new(false), Condvar::new()),
                timeout,
            }
        }

        fn release(&self) -> Result<()> {
            let mut released = self
                .gate
                .0
                .lock()
                .map_err(|_| InfrastructureError::configuration("test gate poisoned"))?;
            *released = true;
            self.gate.1.notify_all();
            Ok(())
        }
    }

    impl ConfigurationEvaluator for BlockingEvaluator {
        fn references(&self) -> &[NativeReference] {
            &[]
        }
        fn timeout(&self) -> Duration {
            self.timeout
        }
        fn specialize(
            &self,
            _input: &ConfigurationSpecialization<'_>,
        ) -> Result<SpecializedConfiguration> {
            self.entered.store(true, Ordering::SeqCst);
            let released = self
                .gate
                .0
                .lock()
                .map_err(|_| InfrastructureError::configuration("test gate poisoned"))?;
            let _released = self
                .gate
                .1
                .wait_while(released, |released| !*released)
                .map_err(|_| InfrastructureError::configuration("test gate poisoned"))?;
            Ok(SpecializedConfiguration {
                configuration: serde_json::Map::new(),
                unresolved: Vec::new(),
            })
        }
    }

    fn request(evaluator: Arc<BlockingEvaluator>) -> Specialization {
        Specialization {
            evaluator,
            consumer: SourceAddress::Resource(ResourceAddress::new("fake_object", "test")),
            sources: BTreeMap::new(),
            deferred: Vec::new(),
        }
    }

    async fn wait_until(predicate: impl Fn() -> bool + Send + Sync) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !predicate() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| InfrastructureError::configuration("test wait timed out"))
    }

    #[tokio::test]
    async fn pre_cancelled_specialization_never_starts() -> Result<()> {
        let evaluator = Arc::new(BlockingEvaluator::new(Duration::from_secs(5)));
        let cancellation = Cancellation::default();
        cancellation.stop();
        let slots = Arc::new(Semaphore::new(1));
        let result =
            specialize_with_slots(request(Arc::clone(&evaluator)), &cancellation, &slots).await;
        assert!(matches!(
            result,
            Err(InfrastructureError::InterruptedWhilePlanning)
        ));
        assert!(!evaluator.entered.load(Ordering::SeqCst));
        assert_eq!(slots.available_permits(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn interruption_returns_before_worker_and_discards_late_result() -> Result<()> {
        let evaluator = Arc::new(BlockingEvaluator::new(Duration::from_secs(5)));
        let cancellation = Cancellation::default();
        let slots = Arc::new(Semaphore::new(1));
        let continued = Arc::new(AtomicUsize::new(0));
        let run = {
            let evaluator = Arc::clone(&evaluator);
            let cancellation = cancellation.clone();
            let slots = Arc::clone(&slots);
            let continued = Arc::clone(&continued);
            tokio::spawn(async move {
                let result = specialize_with_slots(request(evaluator), &cancellation, &slots).await;
                if result.is_ok() {
                    continued.fetch_add(1, Ordering::SeqCst);
                }
                result
            })
        };
        wait_until(|| evaluator.entered.load(Ordering::SeqCst)).await?;
        cancellation.stop();
        let result = tokio::time::timeout(Duration::from_secs(1), run)
            .await
            .map_err(|_| InfrastructureError::configuration("interrupt did not return"))?
            .map_err(|_| InfrastructureError::configuration("test task stopped"))?;
        assert!(matches!(
            result,
            Err(InfrastructureError::InterruptedWhilePlanning)
        ));
        assert_eq!(slots.available_permits(), 0);
        evaluator.release()?;
        wait_until(|| slots.available_permits() == 1).await?;
        assert_eq!(continued.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[tokio::test]
    async fn timeout_and_capacity_stop_work_until_the_worker_finishes() -> Result<()> {
        let evaluator = Arc::new(BlockingEvaluator::new(Duration::from_millis(500)));
        let cancellation = Cancellation::default();
        let slots = Arc::new(Semaphore::new(1));
        let run = {
            let evaluator = Arc::clone(&evaluator);
            let cancellation = cancellation.clone();
            let slots = Arc::clone(&slots);
            tokio::spawn(async move {
                specialize_with_slots(request(evaluator), &cancellation, &slots).await
            })
        };
        wait_until(|| evaluator.entered.load(Ordering::SeqCst)).await?;
        let result = run
            .await
            .map_err(|_| InfrastructureError::configuration("test task stopped"))?;
        assert!(matches!(
            result,
            Err(InfrastructureError::ConfigurationEvaluation {
                reason: "timed out",
                ..
            })
        ));
        let other = Arc::new(BlockingEvaluator::new(Duration::from_secs(5)));
        let result =
            specialize_with_slots(request(Arc::clone(&other)), &cancellation, &slots).await;
        assert!(matches!(
            result,
            Err(InfrastructureError::ConfigurationEvaluation {
                reason: "cannot start because the evaluation worker limit is reached",
                ..
            })
        ));
        assert!(!other.entered.load(Ordering::SeqCst));
        evaluator.release()?;
        wait_until(|| slots.available_permits() == 1).await?;
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_wins_over_a_result_that_arrives_after_it() -> Result<()> {
        let evaluator = Arc::new(BlockingEvaluator::new(Duration::from_secs(5)));
        let cancellation = Cancellation::default();
        let slots = Arc::new(Semaphore::new(1));
        let run = {
            let evaluator = Arc::clone(&evaluator);
            let cancellation = cancellation.clone();
            let slots = Arc::clone(&slots);
            tokio::spawn(async move {
                specialize_with_slots(request(evaluator), &cancellation, &slots).await
            })
        };
        wait_until(|| evaluator.entered.load(Ordering::SeqCst)).await?;
        cancellation.terminate_providers();
        evaluator.release()?;
        let result = run
            .await
            .map_err(|_| InfrastructureError::configuration("test task stopped"))?;
        assert!(matches!(
            result,
            Err(InfrastructureError::InterruptedWhilePlanning)
        ));
        wait_until(|| slots.available_permits() == 1).await?;
        Ok(())
    }

    #[test]
    fn unavailable_or_timed_out_evaluation_aborts_a_reference_replan() {
        let failure = super::super::OperationFailure::from_reference_replan(
            InfrastructureError::ConfigurationEvaluation {
                consumer: "fake_object.test".to_string(),
                reason: "timed out",
            },
        );
        assert!(matches!(failure, super::super::OperationFailure::Abort(_)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_buffered_result_is_refused_after_the_deadline_even_if_the_timer_has_not_run()
    -> Result<()> {
        use std::future::Future;
        use std::task::{Context, Poll, Waker};

        let evaluator = Arc::new(BlockingEvaluator::new(Duration::from_millis(30)));
        let cancellation = Cancellation::default();
        let slots = Arc::new(Semaphore::new(1));
        let evaluation =
            specialize_with_slots(request(Arc::clone(&evaluator)), &cancellation, &slots);
        tokio::pin!(evaluation);
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(
            evaluation.as_mut().poll(&mut context),
            Poll::Pending
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !evaluator.entered.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(evaluator.entered.load(Ordering::SeqCst));
        std::thread::sleep(Duration::from_millis(60));
        evaluator.release()?;
        while slots.available_permits() == 0 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(slots.available_permits(), 1);
        assert!(matches!(
            evaluation.as_mut().poll(&mut context),
            Poll::Ready(Err(InfrastructureError::ConfigurationEvaluation {
                reason: "timed out",
                ..
            }))
        ));
        Ok(())
    }
}
