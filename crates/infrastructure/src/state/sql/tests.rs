//! Deterministic interleavings, exercised through both SQLite transports.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

/// Two clients observe an empty database. The first finishes and starts an
/// apply before the second attempts either migration from its stale snapshot.
pub(in crate::state) async fn stale_first_migrator<T: SqlTransport>(store: &SqlStateStore<T>) {
    assert_eq!(store.schema_version().await.unwrap(), 0);
    let mut delayed = SqlStateStore::new(PausedVersionRead {
        inner: &store.transport,
        pause: AtomicBool::new(true),
        observed: Notify::new(),
        resume: Notify::new(),
    });
    delayed.migration_wait.bound = Duration::ZERO;
    let tenant = TenantKey::try_new("example.com/migration-race", "fresh").unwrap();
    let (outcome, lock) = tokio::join!(delayed.migrate(), async {
        delayed.transport.observed.notified().await;
        store.migrate().await.unwrap();
        let lock = acquire_lock(store, &tenant).await;
        delayed.transport.resume.notify_one();
        lock
    });
    outcome.expect("a stale migrator must not wait on a current-schema lock");
    assert_no_pending(store).await;
    store.unlock(&tenant, &lock).await.unwrap();
}

/// Pause only the initial migration version read, after the real database
/// answered it. Later version checks and every write use the real transport.
struct PausedVersionRead<'transport, T> {
    inner: &'transport T,
    pause: AtomicBool,
    observed: Notify,
    resume: Notify,
}

#[async_trait]
impl<T: SqlTransport> SqlTransport for PausedVersionRead<'_, T> {
    const NAME: &'static str = T::NAME;
    const DIALECT: Dialect = T::DIALECT;

    async fn execute_once(&self, statements: &[Statement]) -> Attempted<Vec<ExecuteResult>> {
        let outcome = self.inner.execute_once(statements).await;
        if statements
            .iter()
            .any(|statement| statement.sql == CREATE_SCHEMA_TABLE)
            && self.pause.swap(false, Ordering::SeqCst)
        {
            self.observed.notify_one();
            self.resume.notified().await;
        }
        outcome
    }

    async fn transaction_once(&self, statements: &[Statement]) -> Attempted<TransactionResult> {
        self.inner.transaction_once(statements).await
    }

    fn secret(&self) -> Option<&str> {
        self.inner.secret()
    }

    fn statement_failure(&self, error: &StatementError, disclosure: Disclosure) -> Failure {
        self.inner.statement_failure(error, disclosure)
    }
}

/// A waiter loses the race to another migrator both before and after its
/// announcement reaches the database. Neither ordering leaves a pending row.
pub(in crate::state) async fn completed_announcements<T: SqlTransport>(store: &SqlStateStore<T>) {
    store.migrate_to(&MIGRATIONS[..1]).await.unwrap();
    let version = LATEST_SCHEMA_VERSION;
    let (first, second) = tokio::join!(
        store.announce_migration(version, store.migration_wait),
        store.announce_migration(version, store.migration_wait)
    );
    first.unwrap();
    second.unwrap();
    store.migrate().await.unwrap();
    assert_no_pending(store).await;

    // A waiter paused after observing a held lock resumes its announcement
    // only after another client has committed the migration.
    store
        .announce_migration(version, store.migration_wait)
        .await
        .unwrap();
    assert_no_pending(store).await;
    let tenant = TenantKey::try_new("example.com/migration-race", "announcement").unwrap();
    let lock = acquire_lock(store, &tenant).await;
    store.unlock(&tenant, &lock).await.unwrap();
}

async fn assert_no_pending<T: SqlTransport>(store: &SqlStateStore<T>) {
    assert!(
        store
            .execute(Statement::new(
                "SELECT version FROM cuenv_infrastructure_pending_migration",
                Vec::new(),
            ))
            .await
            .unwrap()
            .rows
            .is_empty(),
        "completed migrations must leave no pending announcements"
    );
}

async fn acquire_lock<T: SqlTransport>(store: &SqlStateStore<T>, tenant: &TenantKey) -> StateLock {
    store
        .acquire_lock(
            tenant,
            &LockRequest {
                lock: &StateLock::generate(),
                holder: "after migration",
                instance: None,
            },
        )
        .await
        .unwrap()
}
