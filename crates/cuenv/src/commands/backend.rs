//! Shared execution backend selection and ephemeral host service wiring
//! for `cuenv task` and `cuenv ci`.

use std::path::PathBuf;
use std::sync::Arc;

use clap::ValueEnum;
use cuenv_core::Result;
use cuenv_task_exec::{
    EphemeralServiceHandle, EphemeralServiceManager, EphemeralServiceShutdownFuture,
    EphemeralServiceStart, EphemeralServiceStartFuture, TaskBackend,
};

/// CLI `--backend` selector for task and CI execution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum ExecutionBackendArg {
    /// Route tasks with `container` or `dagger` runtime to Dagger; run all other tasks on the host.
    #[default]
    Auto,
    /// Always run tasks on the host (never connect to Dagger).
    Host,
    /// Force every task to execute on the Dagger 1.0 backend.
    Dagger,
}

/// Options for constructing a task execution backend.
#[derive(Debug, Clone, Default)]
pub struct BackendOptions {
    /// Backend selection policy (`auto`, `host`, `dagger`).
    pub backend: ExecutionBackendArg,
    /// Optional Dagger Engine target (`local`, `cloud`, or remote engine URI).
    pub dagger_engine: Option<String>,
    /// Working directory for `dagger session`.
    pub working_dir: Option<PathBuf>,
}

/// Construct the optional [`TaskBackend`] for a `cuenv task` or `cuenv ci` run.
///
/// In `auto` mode, returns a lazy Dagger backend that only spawns `dagger session`
/// if at least one task actually declares a `container` or `dagger` runtime.
///
/// # Errors
///
/// Returns an error if `--backend dagger` is requested when the `dagger` feature
/// is not enabled.
pub fn build_task_backend(options: BackendOptions) -> Result<Option<Arc<dyn TaskBackend>>> {
    match options.backend {
        ExecutionBackendArg::Host => Ok(None),
        ExecutionBackendArg::Auto | ExecutionBackendArg::Dagger => {
            #[cfg(feature = "dagger")]
            {
                let selection = match options.backend {
                    ExecutionBackendArg::Dagger => cuenv_dagger::BackendSelection::ForceAll,
                    ExecutionBackendArg::Auto | ExecutionBackendArg::Host => {
                        cuenv_dagger::BackendSelection::RuntimeDriven
                    }
                };
                Ok(Some(Arc::new(LazyDaggerBackend {
                    engine_override: options.dagger_engine,
                    working_dir: options.working_dir,
                    selection,
                    sessions: tokio::sync::Mutex::default(),
                })))
            }
            #[cfg(not(feature = "dagger"))]
            {
                if options.backend == ExecutionBackendArg::Dagger {
                    return Err(cuenv_core::Error::configuration(
                        "Dagger backend is not enabled in this build. Rebuild with `--features dagger`.",
                    ));
                }
                Ok(None)
            }
        }
    }
}

#[cfg(feature = "dagger")]
#[derive(Debug)]
struct LazyDaggerBackend {
    engine_override: Option<String>,
    working_dir: Option<PathBuf>,
    selection: cuenv_dagger::BackendSelection,
    sessions:
        tokio::sync::Mutex<std::collections::BTreeMap<Option<String>, Arc<DaggerBackendSlot>>>,
}

#[cfg(feature = "dagger")]
type DaggerBackendSlot = tokio::sync::OnceCell<Arc<cuenv_dagger::DaggerTaskBackend>>;

#[cfg(feature = "dagger")]
impl LazyDaggerBackend {
    async fn get_or_init(
        &self,
        runtime: Option<&cuenv_core::manifest::Runtime>,
    ) -> Result<Arc<cuenv_dagger::DaggerTaskBackend>> {
        let engine = selected_engine(self.engine_override.as_deref(), runtime);
        let slot = {
            let mut sessions = self.sessions.lock().await;
            Arc::clone(
                sessions
                    .entry(engine.as_ref().map(ToString::to_string))
                    .or_default(),
            )
        };
        let backend = slot
            .get_or_try_init(|| async {
                let config = cuenv_dagger::SessionConfig {
                    engine: engine.clone().unwrap_or_default(),
                    working_dir: self.working_dir.clone(),
                    ..cuenv_dagger::SessionConfig::default()
                };
                let session = if engine.is_some() {
                    cuenv_dagger::DaggerSession::connect_with_engine(config).await
                } else {
                    cuenv_dagger::DaggerSession::connect(config).await
                }
                .map_err(cuenv_core::Error::from)?;

                let mut config =
                    cuenv_dagger::DaggerBackendConfig::runtime_driven(Arc::new(session));
                config.selection = self.selection;
                Ok::<_, cuenv_core::Error>(Arc::new(cuenv_dagger::DaggerTaskBackend::new(config)))
            })
            .await?;
        Ok(Arc::clone(backend))
    }
}

#[cfg(feature = "dagger")]
fn selected_engine(
    cli_engine: Option<&str>,
    runtime: Option<&cuenv_core::manifest::Runtime>,
) -> Option<cuenv_dagger::EngineTarget> {
    let runtime_engine = match runtime {
        Some(cuenv_core::manifest::Runtime::Dagger(config)) => config.engine.as_deref(),
        _ => None,
    };
    cli_engine
        .or(runtime_engine)
        .map(cuenv_dagger::EngineTarget::parse)
}

#[cfg(feature = "dagger")]
impl TaskBackend for LazyDaggerBackend {
    fn name(&self) -> &'static str {
        "dagger"
    }

    fn manages_own_cache(&self) -> bool {
        true
    }

    fn handles_task(
        &self,
        _task: &cuenv_task_exec::Task,
        runtime: Option<&cuenv_core::manifest::Runtime>,
    ) -> bool {
        match self.selection {
            cuenv_dagger::BackendSelection::ForceAll => true,
            cuenv_dagger::BackendSelection::RuntimeDriven => matches!(
                runtime,
                Some(
                    cuenv_core::manifest::Runtime::Container(_)
                        | cuenv_core::manifest::Runtime::Dagger(_)
                )
            ),
        }
    }

    fn execute_attempt<'a>(
        &'a self,
        request: cuenv_task_exec::BackendAttemptRequest<'a>,
    ) -> cuenv_task_exec::BackendAttemptFuture<'a> {
        Box::pin(async move {
            let backend = self.get_or_init(request.runtime).await?;
            backend.execute_attempt(request).await
        })
    }
}

/// Ephemeral service manager backed by `cuenv-services` for host-executed tasks.
#[derive(Debug, Default)]
pub struct ServicesEphemeralManager;

struct ServicesEphemeralHandle(cuenv_services::EphemeralServicesGuard);

impl EphemeralServiceHandle for ServicesEphemeralHandle {
    fn shutdown(self: Box<Self>) -> EphemeralServiceShutdownFuture<'static> {
        Box::pin(async move {
            self.0.shutdown().await;
        })
    }
}

impl EphemeralServiceManager for ServicesEphemeralManager {
    fn start<'a>(&'a self, request: EphemeralServiceStart<'a>) -> EphemeralServiceStartFuture<'a> {
        Box::pin(async move {
            let guard = cuenv_services::start_ephemeral_services(
                cuenv_services::EphemeralServicesRequest {
                    project_root: request.project_root.to_path_buf(),
                    project_name: request.project_name,
                    services: request.services,
                    requested: request.requested,
                },
            )
            .await
            .map_err(|err| cuenv_core::Error::execution(err.to_string()))?;
            Ok(Box::new(ServicesEphemeralHandle(guard)) as Box<dyn EphemeralServiceHandle>)
        })
    }
}

/// Construct the default host ephemeral service manager.
#[must_use]
pub fn ephemeral_service_manager() -> Arc<dyn EphemeralServiceManager> {
    Arc::new(ServicesEphemeralManager)
}
