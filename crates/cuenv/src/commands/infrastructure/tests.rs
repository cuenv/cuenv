use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use cuenv_infrastructure::{
    Action, InfrastructureError, LockInformation, ManagedResource, ResourceAddress, StateLock,
    StateStore, TenantKey,
};

use super::evaluation::{self, NameCheck, TargetRequest};
use super::output::{Output, action_name};
use super::{
    ConfirmationPolicy, InfrastructureAction, StoreAccess, UnlockRequest, release, unlock,
};
use crate::cli::{
    CliError, EXIT_CLI, EXIT_EVAL, EXIT_LOCKED, OutputFormat, error_code_for, exit_code_for,
};

const MODULE: &str = "module: \"example.com/infrastructure\"\nlanguage: version: \"v0.14.1\"\n";

const INFRASTRUCTURE: &str = "infrastructure: {\n\
     \tstate: turso: url: \"http://127.0.0.1:8080\"\n\
     \tproviders: random: {source: \"hashicorp/random\", version: \"3.7.2\"}\n\
     \tresources: pet: {type: \"random_pet\", configuration: length: 2}\n\
     }\n";

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A temporary module directory. Its name must not start with a dot: the
/// CUE loader skips hidden directories for `./...`, including the root.
fn module_directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("cuenv-infrastructure-")
        .tempdir()
        .unwrap()
}

/// A module with the target project `app` in `app/` (package `cuenv`).
fn module_with_target() -> tempfile::TempDir {
    let directory = module_directory();
    let root = directory.path();
    write(root, "cue.mod/module.cue", MODULE);
    write(
        root,
        "app/env.cue",
        &format!("package cuenv\n\nname: \"app\"\n{INFRASTRUCTURE}"),
    );
    directory
}

fn evaluate(root: &Path, name_check: NameCheck) -> Result<evaluation::Target, CliError> {
    let path = root.join("app");
    evaluation::evaluate(TargetRequest {
        path: path.to_str().unwrap(),
        package: "cuenv",
        name_check,
    })
}

#[test]
fn a_unique_project_evaluates() {
    let module = module_with_target();
    write(
        module.path(),
        "other/env.cue",
        "package cuenv\n\nname: \"other\"\n",
    );
    let target = evaluate(module.path(), NameCheck::WholeModule).unwrap();
    assert_eq!(
        target.tenant,
        TenantKey::new("example.com/infrastructure", "app").unwrap()
    );
    assert_eq!(target.infrastructure.resources.len(), 1);
}

#[test]
fn a_duplicate_name_in_another_package_is_refused() {
    let module = module_with_target();
    write(
        module.path(),
        "elsewhere/project.cue",
        &format!("package deployment\n\nname: \"app\"\n{INFRASTRUCTURE}"),
    );
    let error = evaluate(module.path(), NameCheck::WholeModule).unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_CLI);
    assert!(
        error.to_string().contains("elsewhere (package deployment)"),
        "{error}"
    );
}

#[test]
fn a_same_named_instance_without_infrastructure_shares_no_state() {
    let module = module_with_target();
    write(
        module.path(),
        "elsewhere/project.cue",
        "package deployment\n\nname: \"app\"\n",
    );
    assert!(evaluate(module.path(), NameCheck::WholeModule).is_ok());
}

#[test]
fn a_project_without_infrastructure_is_named_as_such() {
    let directory = module_directory();
    let root = directory.path();
    write(root, "cue.mod/module.cue", MODULE);
    write(root, "app/env.cue", "package cuenv\n\nname: \"app\"\n");
    let error = evaluate(root, NameCheck::TargetOnly).unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_CLI);
    assert!(
        error
            .to_string()
            .contains("project 'app' has no `infrastructure` block"),
        "{error}"
    );
}

#[test]
fn a_child_directory_inheriting_the_project_is_a_duplicate() {
    let module = module_with_target();
    write(module.path(), "app/child/env.cue", "package cuenv\n");
    let error = evaluate(module.path(), NameCheck::WholeModule).unwrap_err();
    assert!(
        error.to_string().contains("app/child (package cuenv)"),
        "{error}"
    );
}

#[test]
fn an_instance_that_fails_to_evaluate_fails_closed() {
    let module = module_with_target();
    write(
        module.path(),
        "broken/env.cue",
        "package cuenv\n\nname: \"broken\"\nvalue: missingReference\n",
    );
    let error = evaluate(module.path(), NameCheck::WholeModule).unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_EVAL);
    let message = error.to_string();
    assert!(message.contains("broken:cuenv"), "{message}");
}

#[test]
fn state_and_unlock_skip_the_module_check() {
    let module = module_with_target();
    write(
        module.path(),
        "broken/env.cue",
        "package cuenv\n\nname: \"app\"\nvalue: missingReference\n",
    );
    assert!(evaluate(module.path(), NameCheck::TargetOnly).is_ok());
    assert_eq!(
        InfrastructureAction::State.name_check(),
        NameCheck::TargetOnly
    );
    assert_eq!(
        InfrastructureAction::Unlock {
            lock_identifier: None
        }
        .name_check(),
        NameCheck::TargetOnly
    );
    assert_eq!(
        InfrastructureAction::Apply {
            confirmation: ConfirmationPolicy::AssumeYes
        }
        .name_check(),
        NameCheck::WholeModule
    );
}

#[test]
fn the_infrastructure_block_must_be_concrete_only_for_this_command() {
    let directory = module_directory();
    let root = directory.path();
    write(root, "cue.mod/module.cue", MODULE);
    write(
        root,
        "app/env.cue",
        "package cuenv\n\nname: \"app\"\ninfrastructure: {\n\
         \tstate: turso: url: \"http://127.0.0.1:8080\"\n\
         \tresources: pet: {type: \"random_pet\", configuration: length: int}\n\
         }\n",
    );
    let error = evaluate(root, NameCheck::TargetOnly).unwrap_err();
    assert!(error.to_string().contains("infrastructure"), "{error}");

    // Other commands evaluate the same instance without the requirement.
    let executor = crate::commands::CommandExecutor::new(
        tokio::sync::mpsc::unbounded_channel().0,
        "cuenv".to_string(),
    );
    assert!(executor.get_module(&root.join("app")).is_ok());
}

#[test]
fn reads_never_migrate() {
    assert_eq!(
        InfrastructureAction::Plan.store_access(),
        StoreAccess::ReadOnly
    );
    assert_eq!(
        InfrastructureAction::State.store_access(),
        StoreAccess::ReadOnly
    );
    assert_eq!(
        InfrastructureAction::Unlock {
            lock_identifier: None
        }
        .store_access(),
        StoreAccess::ReadOnly
    );
    assert_eq!(
        InfrastructureAction::Unlock {
            lock_identifier: Some("lock".to_string())
        }
        .store_access(),
        StoreAccess::ReadWrite
    );
    assert_eq!(
        InfrastructureAction::Destroy {
            confirmation: ConfirmationPolicy::Prompt
        }
        .store_access(),
        StoreAccess::ReadWrite
    );
}

#[test]
fn json_action_names_are_explicit() {
    assert_eq!(action_name(Action::NoOp), "no-op");
    assert_eq!(action_name(Action::Create), "create");
    assert_eq!(action_name(Action::Update), "update");
    assert_eq!(action_name(Action::Replace), "replace");
    assert_eq!(action_name(Action::Delete), "delete");
}

/// A store whose unlock always fails (counting attempts) and whose lock is
/// held by `current`.
#[derive(Debug, Default)]
struct FakeStore {
    unlock_attempts: AtomicUsize,
    current: Option<LockInformation>,
}

#[async_trait]
impl StateStore for FakeStore {
    async fn migrate(&self) -> cuenv_infrastructure::Result<()> {
        Ok(())
    }

    async fn list(
        &self,
        _tenant: &TenantKey,
    ) -> cuenv_infrastructure::Result<Vec<ManagedResource>> {
        Ok(Vec::new())
    }

    async fn put(
        &self,
        _tenant: &TenantKey,
        _lock: &StateLock,
        _resource: &ManagedResource,
    ) -> cuenv_infrastructure::Result<()> {
        Ok(())
    }

    async fn delete(
        &self,
        _tenant: &TenantKey,
        _lock: &StateLock,
        _address: &ResourceAddress,
    ) -> cuenv_infrastructure::Result<()> {
        Ok(())
    }

    async fn lock(
        &self,
        _tenant: &TenantKey,
        _holder: &str,
    ) -> cuenv_infrastructure::Result<StateLock> {
        Ok(StateLock {
            lock_identifier: "lock".to_string(),
        })
    }

    async fn unlock(
        &self,
        _tenant: &TenantKey,
        _lock: &StateLock,
    ) -> cuenv_infrastructure::Result<()> {
        self.unlock_attempts.fetch_add(1, Ordering::SeqCst);
        Err(InfrastructureError::state("unavailable"))
    }

    async fn current_lock(
        &self,
        _tenant: &TenantKey,
    ) -> cuenv_infrastructure::Result<Option<LockInformation>> {
        Ok(self.current.clone())
    }

    async fn force_unlock(
        &self,
        _tenant: &TenantKey,
        _lock_identifier: &str,
    ) -> cuenv_infrastructure::Result<bool> {
        Ok(false)
    }
}

#[tokio::test]
async fn release_does_not_wait_after_its_last_attempt() {
    let store = Arc::new(FakeStore::default());
    let tenant = TenantKey::new("example.com/infrastructure", "app").unwrap();
    let lock = StateLock {
        lock_identifier: "lock".to_string(),
    };
    let started = Instant::now();
    assert!(release(store.as_ref(), &tenant, &lock).await.is_err());
    let elapsed = started.elapsed();
    assert_eq!(store.unlock_attempts.load(Ordering::SeqCst), 3);
    // 250 ms and 500 ms between the three attempts; a wait after the last
    // one would add another 750 ms.
    assert!(elapsed.as_millis() < 1_300, "{elapsed:?}");
}

#[tokio::test]
async fn unlock_with_another_identifier_is_a_lock_failure() {
    let store = FakeStore {
        current: Some(LockInformation {
            lock_identifier: "current".to_string(),
            holder: "someone".to_string(),
            acquired_at: "2026-09-28T00:00:00Z".to_string(),
        }),
        ..FakeStore::default()
    };
    let tenant = TenantKey::new("example.com/infrastructure", "app").unwrap();
    let error = unlock(&UnlockRequest {
        store: &store,
        tenant: &tenant,
        lock_identifier: Some("stale"),
        output: Output::new(OutputFormat::Text),
    })
    .await
    .unwrap_err();
    assert_eq!(exit_code_for(&error), EXIT_LOCKED);
    assert_eq!(error_code_for(&error), "infrastructure_locked");
}
