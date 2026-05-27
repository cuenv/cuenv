//! Persisted manual service control requests.

use std::time::Duration;

use tracing::warn;

use crate::session::SessionManager;

/// Manual control request kinds queued by service lifecycle commands.
#[derive(Clone, Copy)]
pub(crate) enum ManualControlRequest {
    /// Stop a running service.
    Stop,
    /// Restart a running service.
    Restart,
}

impl ManualControlRequest {
    fn take(self, session: &SessionManager, name: &str) -> crate::Result<bool> {
        match self {
            Self::Stop => session.take_service_stop_request(name),
            Self::Restart => session.take_service_restart_request(name),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

/// Wait until a persisted manual control request is available for a service.
pub(crate) async fn wait_for_control_request(
    session: &SessionManager,
    service_name: &str,
    request: ManualControlRequest,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    loop {
        interval.tick().await;
        match request.take(session, service_name) {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => warn!(
                service = %service_name,
                error = %error,
                request = request.name(),
                "Failed to consume service control request"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_for_restart_request_consumes_marker() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(dir.path(), "test-project").unwrap();

        session.request_service_restart("db").unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_control_request(&session, "db", ManualControlRequest::Restart),
        )
        .await
        .unwrap();

        assert!(!session.take_service_restart_request("db").unwrap());
    }

    #[tokio::test]
    async fn wait_for_stop_request_consumes_marker() {
        let dir = tempfile::tempdir().unwrap();
        let session = SessionManager::create(dir.path(), "test-project").unwrap();

        session.request_service_stop("db").unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_control_request(&session, "db", ManualControlRequest::Stop),
        )
        .await
        .unwrap();

        assert!(!session.take_service_stop_request("db").unwrap());
    }
}
