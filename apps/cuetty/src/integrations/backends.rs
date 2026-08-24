//! Replaceable contracts for terminal/session backends.
//!
//! This module deliberately knows nothing about Rio's host implementation (or
//! any other terminal transport).  The application can use the local Rio
//! backend as its default while keeping the session lifecycle replaceable.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;

/// Backends understood by the host. Optional backends can be represented even
/// when their implementation is not compiled into this package.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BackendKind {
    LocalRio,
    /// Deterministic in-memory backend used only by lifecycle tests.
    InMemoryTest,
    Tmux,
    Dtach,
    Remote,
    Wasm,
}

/// Environment handling requested for a session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentPolicy {
    Inherit,
    Clear,
    Explicit(BTreeMap<String, String>),
}

/// Backend-independent description of a session to create.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSpec {
    pub command: Vec<String>,
    pub working_directory: PathBuf,
    pub environment: EnvironmentPolicy,
}

impl SessionSpec {
    pub fn new(
        command: impl IntoIterator<Item = impl Into<String>>,
        working_directory: impl Into<PathBuf>,
        environment: EnvironmentPolicy,
    ) -> Self {
        Self {
            command: command.into_iter().map(Into::into).collect(),
            working_directory: working_directory.into(),
            environment,
        }
    }

    pub fn validate(&self) -> Result<(), BackendError> {
        if self.command.is_empty() || self.command.iter().any(String::is_empty) {
            return Err(BackendError::EmptyCommand);
        }
        if self
            .command
            .iter()
            .any(|part| part.chars().any(char::is_control))
        {
            return Err(BackendError::InvalidCommand);
        }
        let path = self.working_directory.to_string_lossy();
        if path.is_empty() || path.contains('\0') {
            return Err(BackendError::InvalidWorkingDirectory {
                path: self.working_directory.clone(),
            });
        }
        Ok(())
    }
}

/// Stable identity used to refer to a session across reconnects.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionHandle(String);

impl SessionHandle {
    pub fn new(identity: impl Into<String>) -> Result<Self, BackendError> {
        let identity = identity.into();
        if identity.is_empty() {
            return Err(BackendError::InvalidHandle);
        }
        Ok(Self(identity))
    }

    pub fn identity(&self) -> &str {
        &self.0
    }
}

/// Features declared by a backend, in deterministic order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendCapabilities {
    pub backend: BackendKind,
    pub supports_attach: bool,
    pub supports_reconnect: bool,
    pub supports_close: bool,
    pub features: BTreeSet<String>,
}

impl BackendCapabilities {
    pub fn local_rio() -> Self {
        Self {
            backend: BackendKind::LocalRio,
            // The live Rio host currently exposes create, resize/input/paste,
            // frame/effects, and close only.  It has no attach or reconnect
            // transport, so these must stay false until one exists.
            supports_attach: false,
            supports_reconnect: false,
            supports_close: true,
            features: BTreeSet::from(["pty".to_owned(), "vt".to_owned()]),
        }
    }

    /// Capabilities for the pure in-memory test backend.  This profile is
    /// intentionally distinct from [`Self::local_rio`]: its identity-
    /// preserving lifecycle methods do not imply that the live Rio host can
    /// attach to or reconnect an existing terminal.
    pub fn in_memory_test() -> Self {
        Self {
            backend: BackendKind::InMemoryTest,
            supports_attach: true,
            supports_reconnect: true,
            supports_close: true,
            features: BTreeSet::from(["in-memory".to_owned()]),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendError {
    EmptyCommand,
    InvalidCommand,
    InvalidWorkingDirectory {
        path: PathBuf,
    },
    InvalidHandle,
    SessionNotFound {
        handle: SessionHandle,
    },
    SessionClosed {
        handle: SessionHandle,
    },
    UnsupportedBackend {
        backend: BackendKind,
    },
    BackendUnavailable {
        backend: BackendKind,
        reason: String,
    },
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCommand => write!(f, "session command must not be empty"),
            Self::InvalidCommand => write!(f, "session command contains a control character"),
            Self::InvalidWorkingDirectory { path } => {
                write!(f, "invalid session working directory: {path:?}")
            }
            Self::InvalidHandle => write!(f, "session handle must not be empty"),
            Self::SessionNotFound { handle } => {
                write!(f, "session not found: {}", handle.identity())
            }
            Self::SessionClosed { handle } => write!(f, "session is closed: {}", handle.identity()),
            Self::UnsupportedBackend { backend } => {
                write!(f, "backend is unsupported: {backend:?}")
            }
            Self::BackendUnavailable { backend, reason } => {
                write!(f, "backend {backend:?} is unavailable: {reason}")
            }
        }
    }
}

impl std::error::Error for BackendError {}

/// Lifecycle boundary implemented by local, remote, or future plugin backends.
pub trait SessionBackend {
    fn capabilities(&self) -> BackendCapabilities;
    fn create(&mut self, spec: SessionSpec) -> Result<SessionHandle, BackendError>;
    fn attach(&mut self, handle: &SessionHandle) -> Result<SessionHandle, BackendError>;
    fn reconnect(&mut self, handle: &SessionHandle) -> Result<SessionHandle, BackendError>;
    fn close(&mut self, handle: &SessionHandle) -> Result<(), BackendError>;
}

/// A pure backend used to test lifecycle consumers without starting a process.
#[derive(Debug, Default)]
pub struct InMemorySessionBackend {
    sessions: BTreeMap<SessionHandle, SessionSpec>,
    closed: BTreeSet<SessionHandle>,
    next_id: u64,
}

impl SessionBackend for InMemorySessionBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::in_memory_test()
    }

    fn create(&mut self, spec: SessionSpec) -> Result<SessionHandle, BackendError> {
        spec.validate()?;
        self.next_id += 1;
        let handle = SessionHandle::new(format!("in-memory-{}", self.next_id))?;
        self.sessions.insert(handle.clone(), spec);
        Ok(handle)
    }

    fn attach(&mut self, handle: &SessionHandle) -> Result<SessionHandle, BackendError> {
        if self.sessions.contains_key(handle) {
            Ok(handle.clone())
        } else if self.closed.contains(handle) {
            Err(BackendError::SessionClosed {
                handle: handle.clone(),
            })
        } else {
            Err(BackendError::SessionNotFound {
                handle: handle.clone(),
            })
        }
    }

    fn reconnect(&mut self, handle: &SessionHandle) -> Result<SessionHandle, BackendError> {
        self.attach(handle)
    }

    fn close(&mut self, handle: &SessionHandle) -> Result<(), BackendError> {
        if self.sessions.remove(handle).is_some() {
            self.closed.insert(handle.clone());
        }
        if self.closed.contains(handle) {
            Ok(())
        } else {
            Err(BackendError::SessionNotFound {
                handle: handle.clone(),
            })
        }
    }
}

/// An explicit marker for optional backends that are not available yet.
pub fn unavailable(backend: BackendKind) -> BackendError {
    BackendError::UnsupportedBackend { backend }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> SessionSpec {
        SessionSpec::new(["sh"], "/tmp", EnvironmentPolicy::Inherit)
    }

    #[test]
    fn local_backend_creates_session() {
        let mut backend = InMemorySessionBackend::default();
        let handle = backend.create(spec()).expect("valid session");
        assert_eq!(handle.identity(), "in-memory-1");
    }

    #[test]
    fn unsupported_backend_is_typed() {
        assert_eq!(
            unavailable(BackendKind::Tmux),
            BackendError::UnsupportedBackend {
                backend: BackendKind::Tmux
            }
        );
    }

    #[test]
    fn reconnect_preserves_identity() {
        let mut backend = InMemorySessionBackend::default();
        let handle = backend.create(spec()).unwrap();
        assert_eq!(backend.reconnect(&handle).unwrap(), handle);
    }

    #[test]
    fn close_is_idempotent() {
        let mut backend = InMemorySessionBackend::default();
        let handle = backend.create(spec()).unwrap();
        backend.close(&handle).unwrap();
        backend.close(&handle).unwrap();
    }

    #[test]
    fn capabilities_are_deterministic() {
        let backend = InMemorySessionBackend::default();
        assert_eq!(backend.capabilities(), backend.capabilities());
        assert_eq!(backend.capabilities().backend, BackendKind::InMemoryTest);
        assert!(backend.capabilities().supports_attach);
        assert!(backend.capabilities().supports_reconnect);
    }

    #[test]
    fn local_rio_does_not_advertise_unimplemented_lifecycle_operations() {
        let capabilities = BackendCapabilities::local_rio();
        assert!(!capabilities.supports_attach);
        assert!(!capabilities.supports_reconnect);
        assert!(capabilities.supports_close);
    }

    #[test]
    fn invalid_specs_are_denied() {
        let mut backend = InMemorySessionBackend::default();
        assert_eq!(
            backend.create(SessionSpec::new(
                Vec::<String>::new(),
                "/tmp",
                EnvironmentPolicy::Inherit
            )),
            Err(BackendError::EmptyCommand)
        );
        assert!(matches!(
            backend.create(SessionSpec::new(["sh"], "", EnvironmentPolicy::Inherit)),
            Err(BackendError::InvalidWorkingDirectory { .. })
        ));
    }
}
