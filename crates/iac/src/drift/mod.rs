//! Infrastructure drift detection.
//!
//! This module implements a hybrid drift detection system that combines:
//! - **Event-driven detection**: Streaming audit logs from cloud providers
//! - **Polling-based detection**: Periodic state reconciliation
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │  Event-Driven Detection (Primary)                           │
//! │  AWS: EventBridge → Lambda → Actor notification            │
//! │  Azure: Event Hubs → Stream processor → Actor notification │
//! │  GCP: Pub/Sub → Cloud Functions → Actor notification       │
//! └─────────────────────────────────────────────────────────────┘
//!                             ↓
//! ┌─────────────────────────────────────────────────────────────┐
//! │  Polling Fallback                                           │
//! │  Critical resources: 5-15 minute intervals                 │
//! │  Standard resources: 1 hour intervals                       │
//! │  State reconciliation: Compare derived vs actual           │
//! └─────────────────────────────────────────────────────────────┘
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio::time::interval;
use tracing::{debug, info, instrument, warn};

use crate::error::{Error, Result};
use crate::provider::ProviderClient;

/// Configuration for drift detection.
#[derive(Debug, Clone)]
pub struct DriftConfig {
    /// Enable polling-based drift detection
    pub enable_polling: bool,

    /// Polling interval for critical resources
    pub critical_poll_interval: Duration,

    /// Polling interval for standard resources
    pub standard_poll_interval: Duration,

    /// Enable event-based drift detection (requires cloud integration)
    pub enable_events: bool,

    /// Maximum age of cached state before forcing refresh
    pub state_cache_ttl: Duration,
}

impl Default for DriftConfig {
    fn default() -> Self {
        Self {
            enable_polling: true,
            critical_poll_interval: Duration::from_secs(5 * 60),  // 5 minutes
            standard_poll_interval: Duration::from_secs(60 * 60), // 1 hour
            enable_events: false,
            state_cache_ttl: Duration::from_secs(15 * 60), // 15 minutes
        }
    }
}

/// A drift event indicating a change in infrastructure state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftEvent {
    /// Resource identifier
    pub resource_id: String,

    /// Resource type
    pub resource_type: String,

    /// Type of drift detected
    pub drift_type: DriftType,

    /// Expected state (from configuration)
    pub expected_state: Option<serde_json::Value>,

    /// Actual state (from cloud provider)
    pub actual_state: Option<serde_json::Value>,

    /// Specific attributes that drifted
    pub drifted_attributes: Vec<DriftedAttribute>,

    /// When the drift was detected
    pub detected_at: chrono::DateTime<chrono::Utc>,

    /// Source of detection
    pub detection_source: DetectionSource,
}

/// Type of drift detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DriftType {
    /// Resource was modified outside of IaC
    Modified,
    /// Resource was deleted outside of IaC
    Deleted,
    /// Resource was created outside of IaC (orphaned)
    Created,
    /// Resource configuration differs from expected
    ConfigDrift,
}

/// An attribute that has drifted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftedAttribute {
    /// Attribute path (e.g., "tags.Name")
    pub path: String,
    /// Expected value
    pub expected: serde_json::Value,
    /// Actual value
    pub actual: serde_json::Value,
}

/// Source of drift detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DetectionSource {
    /// Detected via polling
    Polling,
    /// Detected via audit log event
    AuditLog,
    /// Detected via webhook/event stream
    Event,
    /// Detected during manual refresh
    Manual,
}

/// Current drift status for a resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriftStatus {
    /// Resource identifier
    pub resource_id: String,

    /// Resource type
    pub resource_type: String,

    /// Whether drift has been detected
    pub has_drift: bool,

    /// Latest drift event (if any)
    pub latest_drift: Option<DriftEvent>,

    /// When the resource was last checked
    pub last_checked: chrono::DateTime<chrono::Utc>,

    /// Current state from provider
    pub current_state: Option<serde_json::Value>,
}

/// Internal state for a monitored resource.
struct MonitoredResource {
    /// Resource ID
    resource_id: String,
    /// Resource type
    resource_type: String,
    /// Provider client
    provider: Arc<ProviderClient>,
    /// Expected state
    expected_state: serde_json::Value,
    /// Last known actual state
    last_state: Option<serde_json::Value>,
    /// When state was last fetched
    last_fetched: Option<Instant>,
    /// Whether this is a critical resource
    is_critical: bool,
    /// Latest drift status
    drift_status: Option<DriftStatus>,
}

/// The drift detector system.
pub struct DriftDetector {
    /// Configuration
    config: DriftConfig,

    /// Monitored resources
    resources: DashMap<String, MonitoredResource>,

    /// Drift event handlers
    handlers: RwLock<Vec<Box<dyn DriftHandler>>>,

    /// Whether the detector is running
    running: Arc<RwLock<bool>>,
}

/// Trait for handling drift events.
#[async_trait::async_trait]
pub trait DriftHandler: Send + Sync {
    /// Called when drift is detected.
    async fn on_drift(&self, event: &DriftEvent);

    /// Called when drift is resolved.
    async fn on_resolved(&self, resource_id: &str);
}

impl DriftDetector {
    /// Creates a new drift detector.
    #[must_use]
    pub fn new(config: DriftConfig) -> Self {
        Self {
            config,
            resources: DashMap::new(),
            handlers: RwLock::new(Vec::new()),
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Registers a resource for drift monitoring.
    pub fn register_resource(
        &self,
        resource_id: impl Into<String>,
        resource_type: impl Into<String>,
        provider: Arc<ProviderClient>,
        expected_state: serde_json::Value,
        is_critical: bool,
    ) {
        let resource_id = resource_id.into();
        let resource_type = resource_type.into();

        debug!(
            resource_id = %resource_id,
            resource_type = %resource_type,
            is_critical = is_critical,
            "Registering resource for drift monitoring"
        );

        self.resources.insert(
            resource_id.clone(),
            MonitoredResource {
                resource_id,
                resource_type,
                provider,
                expected_state,
                last_state: None,
                last_fetched: None,
                is_critical,
                drift_status: None,
            },
        );
    }

    /// Unregisters a resource from drift monitoring.
    pub fn unregister_resource(&self, resource_id: &str) {
        self.resources.remove(resource_id);
    }

    /// Adds a drift event handler.
    pub async fn add_handler(&self, handler: Box<dyn DriftHandler>) {
        let mut handlers = self.handlers.write().await;
        handlers.push(handler);
    }

    /// Starts the drift detection background tasks.
    #[instrument(name = "drift_detector_start", skip(self))]
    pub async fn start(&self) -> Result<()> {
        let mut running = self.running.write().await;
        if *running {
            return Ok(());
        }
        *running = true;
        drop(running);

        info!("Starting drift detection");

        if self.config.enable_polling {
            self.start_polling_tasks().await;
        }

        Ok(())
    }

    /// Stops the drift detection background tasks.
    pub async fn stop(&self) {
        let mut running = self.running.write().await;
        *running = false;
        info!("Stopped drift detection");
    }

    /// Checks all resources for drift.
    #[instrument(name = "drift_check_all", skip(self))]
    pub async fn check_all(&self) -> Result<Vec<DriftStatus>> {
        let mut results = Vec::new();

        for entry in self.resources.iter() {
            let status = self.check_resource(&entry.resource_id).await?;
            results.push(status);
        }

        Ok(results)
    }

    /// Checks a specific resource for drift.
    #[instrument(name = "drift_check_resource", skip(self))]
    pub async fn check_resource(&self, resource_id: &str) -> Result<DriftStatus> {
        let mut resource = self
            .resources
            .get_mut(resource_id)
            .ok_or_else(|| Error::ResourceNotFound {
                resource_id: resource_id.to_string(),
            })?;

        // Fetch current state from provider
        let current_state = resource
            .provider
            .read_resource(&resource.resource_type, resource_id)
            .await?;

        resource.last_fetched = Some(Instant::now());
        resource.last_state = current_state.clone();

        // Compare with expected state
        let drift = match &current_state {
            Some(actual) => self.compare_states(&resource.expected_state, actual),
            None => Some(DriftEvent {
                resource_id: resource_id.to_string(),
                resource_type: resource.resource_type.clone(),
                drift_type: DriftType::Deleted,
                expected_state: Some(resource.expected_state.clone()),
                actual_state: None,
                drifted_attributes: Vec::new(),
                detected_at: chrono::Utc::now(),
                detection_source: DetectionSource::Polling,
            }),
        };

        let status = DriftStatus {
            resource_id: resource_id.to_string(),
            resource_type: resource.resource_type.clone(),
            has_drift: drift.is_some(),
            latest_drift: drift.clone(),
            last_checked: chrono::Utc::now(),
            current_state,
        };

        resource.drift_status = Some(status.clone());

        // Notify handlers if drift detected
        if let Some(event) = drift {
            self.notify_handlers(&event).await;
        }

        Ok(status)
    }

    /// Updates the expected state for a resource.
    pub fn update_expected_state(&self, resource_id: &str, state: serde_json::Value) {
        if let Some(mut resource) = self.resources.get_mut(resource_id) {
            resource.expected_state = state;
        }
    }

    /// Processes an external drift event (from audit logs or webhooks).
    #[instrument(name = "drift_process_event", skip(self, event))]
    pub async fn process_event(&self, event: DriftEvent) {
        debug!(
            resource_id = %event.resource_id,
            drift_type = ?event.drift_type,
            "Processing external drift event"
        );

        // Update resource state if we're tracking it
        if let Some(mut resource) = self.resources.get_mut(&event.resource_id) {
            resource.drift_status = Some(DriftStatus {
                resource_id: event.resource_id.clone(),
                resource_type: event.resource_type.clone(),
                has_drift: true,
                latest_drift: Some(event.clone()),
                last_checked: chrono::Utc::now(),
                current_state: event.actual_state.clone(),
            });
        }

        self.notify_handlers(&event).await;
    }

    // Private methods

    async fn start_polling_tasks(&self) {
        let critical_interval = self.config.critical_poll_interval;
        let standard_interval = self.config.standard_poll_interval;
        let running = Arc::clone(&self.running);

        // Spawn critical resource polling task
        let detector = self as *const Self;
        tokio::spawn(async move {
            let detector = unsafe { &*detector };
            let mut interval = interval(critical_interval);

            loop {
                interval.tick().await;

                let is_running = *running.read().await;
                if !is_running {
                    break;
                }

                for entry in detector.resources.iter() {
                    if entry.is_critical {
                        if let Err(e) = detector.check_resource(&entry.resource_id).await {
                            warn!(
                                resource_id = %entry.resource_id,
                                error = %e,
                                "Failed to check critical resource"
                            );
                        }
                    }
                }
            }
        });

        // Spawn standard resource polling task
        let running = Arc::clone(&self.running);
        let detector = self as *const Self;
        tokio::spawn(async move {
            let detector = unsafe { &*detector };
            let mut interval = interval(standard_interval);

            loop {
                interval.tick().await;

                let is_running = *running.read().await;
                if !is_running {
                    break;
                }

                for entry in detector.resources.iter() {
                    if !entry.is_critical {
                        if let Err(e) = detector.check_resource(&entry.resource_id).await {
                            warn!(
                                resource_id = %entry.resource_id,
                                error = %e,
                                "Failed to check standard resource"
                            );
                        }
                    }
                }
            }
        });
    }

    fn compare_states(
        &self,
        expected: &serde_json::Value,
        actual: &serde_json::Value,
    ) -> Option<DriftEvent> {
        let drifted = self.find_drifted_attributes("", expected, actual);

        if drifted.is_empty() {
            None
        } else {
            Some(DriftEvent {
                resource_id: String::new(), // Will be filled by caller
                resource_type: String::new(),
                drift_type: DriftType::Modified,
                expected_state: Some(expected.clone()),
                actual_state: Some(actual.clone()),
                drifted_attributes: drifted,
                detected_at: chrono::Utc::now(),
                detection_source: DetectionSource::Polling,
            })
        }
    }

    fn find_drifted_attributes(
        &self,
        path: &str,
        expected: &serde_json::Value,
        actual: &serde_json::Value,
    ) -> Vec<DriftedAttribute> {
        let mut drifted = Vec::new();

        match (expected, actual) {
            (serde_json::Value::Object(exp_obj), serde_json::Value::Object(act_obj)) => {
                // Check all expected keys
                for (key, exp_val) in exp_obj {
                    let attr_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };

                    match act_obj.get(key) {
                        Some(act_val) => {
                            drifted.extend(self.find_drifted_attributes(&attr_path, exp_val, act_val));
                        }
                        None => {
                            drifted.push(DriftedAttribute {
                                path: attr_path,
                                expected: exp_val.clone(),
                                actual: serde_json::Value::Null,
                            });
                        }
                    }
                }

                // Check for unexpected keys (optional, depending on policy)
                // for (key, act_val) in act_obj {
                //     if !exp_obj.contains_key(key) {
                //         // Unexpected attribute present
                //     }
                // }
            }
            (serde_json::Value::Array(exp_arr), serde_json::Value::Array(act_arr)) => {
                if exp_arr.len() != act_arr.len() {
                    drifted.push(DriftedAttribute {
                        path: path.to_string(),
                        expected: expected.clone(),
                        actual: actual.clone(),
                    });
                } else {
                    for (i, (exp_item, act_item)) in exp_arr.iter().zip(act_arr.iter()).enumerate() {
                        let item_path = format!("{path}[{i}]");
                        drifted.extend(self.find_drifted_attributes(&item_path, exp_item, act_item));
                    }
                }
            }
            _ => {
                if expected != actual {
                    drifted.push(DriftedAttribute {
                        path: path.to_string(),
                        expected: expected.clone(),
                        actual: actual.clone(),
                    });
                }
            }
        }

        drifted
    }

    async fn notify_handlers(&self, event: &DriftEvent) {
        let handlers = self.handlers.read().await;
        for handler in handlers.iter() {
            handler.on_drift(event).await;
        }
    }
}

/// A simple logging drift handler.
pub struct LoggingDriftHandler;

#[async_trait::async_trait]
impl DriftHandler for LoggingDriftHandler {
    async fn on_drift(&self, event: &DriftEvent) {
        warn!(
            resource_id = %event.resource_id,
            resource_type = %event.resource_type,
            drift_type = ?event.drift_type,
            num_attributes = event.drifted_attributes.len(),
            "Drift detected"
        );

        for attr in &event.drifted_attributes {
            warn!(
                attribute = %attr.path,
                expected = %attr.expected,
                actual = %attr.actual,
                "Attribute drifted"
            );
        }
    }

    async fn on_resolved(&self, resource_id: &str) {
        info!(resource_id = %resource_id, "Drift resolved");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_drifted_attributes() {
        let detector = DriftDetector::new(DriftConfig::default());

        let expected = serde_json::json!({
            "name": "test",
            "count": 5,
            "tags": {
                "env": "prod"
            }
        });

        let actual = serde_json::json!({
            "name": "test",
            "count": 10,
            "tags": {
                "env": "dev"
            }
        });

        let drifted = detector.find_drifted_attributes("", &expected, &actual);

        assert_eq!(drifted.len(), 2);
        assert!(drifted.iter().any(|d| d.path == "count"));
        assert!(drifted.iter().any(|d| d.path == "tags.env"));
    }

    #[test]
    fn test_no_drift() {
        let detector = DriftDetector::new(DriftConfig::default());

        let state = serde_json::json!({
            "name": "test",
            "enabled": true
        });

        let drifted = detector.find_drifted_attributes("", &state, &state);
        assert!(drifted.is_empty());
    }
}
