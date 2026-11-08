//! Markers for provider changes whose outcome is not yet recorded.
//!
//! Before cuenv asks a provider to create, update or delete an object it
//! writes an in-flight marker to the shared state backend, fenced by the run's
//! lock, and removes it once the provider's result is recorded. A marker that
//! outlives its run means the outcome is uncertain: the provider may have
//! changed the object while cuenv never learned (a lost response, a crash) or
//! could not record (a failed state write saved locally on the machine that
//! ran it, or a response cuenv refused). Plans refuse while a marker exists,
//! so no other runner repeats the change against state that does not know
//! about it.
//!
//! A marker is an ordinary record of the reserved type
//! [`IN_FLIGHT_RESOURCE_TYPE`] whose name encodes the target address, so it
//! needs no backend schema change, every backend fences it with the lock
//! like any other write, and a build that predates markers fails on the
//! unknown record instead of ignoring it. It holds no state values: only the
//! target address, the step, when it started and the lock of the run.
//!
//! A marker is removed by recording the outcome: recovering the change saved
//! locally, importing the object, or, after checking the object by hand,
//! removing the marker record.

use serde::{Deserialize, Serialize};

use crate::error::{InfrastructureError, Result};
use crate::state::{ManagedResource, ResourceAddress};

/// The reserved resource type of in-flight markers. No provider may declare
/// it.
pub const IN_FLIGHT_RESOURCE_TYPE: &str = "cuenv_inflight";

/// The provider name and source recorded on markers.
const MARKER_PROVIDER: &str = "cuenv";
const MARKER_PROVIDER_SOURCE: &str = "cuenv.dev/internal/in-flight";

/// A provider change whose outcome was never recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlightChange {
    /// The resource the provider was asked to change.
    pub address: ResourceAddress,
    /// The marker record's own address, which `state remove` accepts.
    pub marker: ResourceAddress,
    /// `create`, `update` or `delete`.
    pub step: String,
    /// When the change started (RFC 3339).
    pub started_at: String,
    /// The lock of the run that started it.
    pub lock_identifier: String,
}

/// The marker's content: nothing but where, what and when.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarkerContent {
    address: String,
    step: String,
    started_at: String,
    lock_identifier: String,
}

/// Whether `address` is an in-flight marker rather than a managed resource.
#[must_use]
pub fn is_marker(address: &ResourceAddress) -> bool {
    address.resource_type == IN_FLIGHT_RESOURCE_TYPE
}

/// The address of the marker for `address`. A resource type never contains
/// `-`, so `<type>-<name>` names one address and is a valid resource name.
#[must_use]
pub fn marker_address(address: &ResourceAddress) -> ResourceAddress {
    ResourceAddress::new(
        IN_FLIGHT_RESOURCE_TYPE,
        format!("{}-{}", address.resource_type, address.name),
    )
}

/// The marker record for a `step` of `address` started by the run holding
/// `lock_identifier`.
///
/// # Errors
///
/// Returns [`InfrastructureError::Codec`] if the marker cannot be encoded.
pub fn marker_record(
    address: &ResourceAddress,
    step: &str,
    lock_identifier: &str,
) -> Result<ManagedResource> {
    let content = MarkerContent {
        address: address.to_string(),
        step: step.to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        lock_identifier: lock_identifier.to_string(),
    };
    let state = serde_json::to_value(&content).map_err(|error| {
        InfrastructureError::codec(format!(
            "cannot encode the in-flight marker of {address}: {error}"
        ))
    })?;
    Ok(ManagedResource {
        address: marker_address(address),
        provider: MARKER_PROVIDER.to_string(),
        provider_source: MARKER_PROVIDER_SOURCE.to_string(),
        schema_version: 0,
        state,
        private: Vec::new(),
        dependencies: Vec::new(),
        sensitive_attributes: Vec::new(),
        tainted: false,
        identity: None,
        serial: 0,
        generation: uuid::Uuid::nil(),
    })
}

/// Separate stored records into managed resources and in-flight changes. A
/// marker whose content cannot be read is still reported, by its address.
#[must_use]
pub fn split(records: Vec<ManagedResource>) -> (Vec<ManagedResource>, Vec<InFlightChange>) {
    let mut resources = Vec::new();
    let mut in_flight = Vec::new();
    for record in records {
        if !is_marker(&record.address) {
            resources.push(record);
            continue;
        }
        let content = serde_json::from_value::<MarkerContent>(record.state.clone()).ok();
        let address = content
            .as_ref()
            .and_then(|content| ResourceAddress::parse(&content.address))
            .or_else(|| {
                let (resource_type, name) = record.address.name.split_once('-')?;
                Some(ResourceAddress::new(resource_type, name))
            })
            .unwrap_or_else(|| record.address.clone());
        in_flight.push(InFlightChange {
            address,
            marker: record.address,
            step: content
                .as_ref()
                .map_or_else(|| "change".to_string(), |content| content.step.clone()),
            started_at: content
                .as_ref()
                .map(|content| content.started_at.clone())
                .unwrap_or_default(),
            lock_identifier: content
                .map(|content| content.lock_identifier)
                .unwrap_or_default(),
        });
    }
    (resources, in_flight)
}

/// Fail with [`InfrastructureError::ChangesInFlight`] when any change is in
/// flight.
///
/// # Errors
///
/// As described.
pub fn refuse(tenant: &str, in_flight: &[InFlightChange]) -> Result<()> {
    if in_flight.is_empty() {
        return Ok(());
    }
    Err(InfrastructureError::ChangesInFlight {
        tenant: tenant.to_string(),
        changes: in_flight
            .iter()
            .map(|change| {
                format!(
                    "{} ({} started {} by lock {}; marker {})",
                    change.address,
                    change.step,
                    if change.started_at.is_empty() {
                        "at an unknown time"
                    } else {
                        change.started_at.as_str()
                    },
                    if change.lock_identifier.is_empty() {
                        "unknown"
                    } else {
                        change.lock_identifier.as_str()
                    },
                    change.marker
                )
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_round_trip_and_hold_no_state_values() {
        let address = ResourceAddress::new("random_pet", "my-pet_1");
        let marker = marker_record(&address, "create", "lock-1").unwrap();
        assert!(is_marker(&marker.address));
        assert_eq!(
            marker.address.to_string(),
            "cuenv_inflight.random_pet-my-pet_1"
        );
        assert_eq!(
            ResourceAddress::parse(&marker.address.to_string()),
            Some(marker.address.clone()),
            "the marker address must be one `state remove` accepts"
        );
        let object = marker.state.as_object().unwrap();
        let mut keys: Vec<_> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["address", "lockIdentifier", "startedAt", "step"]);

        let managed = ManagedResource {
            address: address.clone(),
            ..marker.clone()
        };
        let (resources, in_flight) = split(vec![managed, marker]);
        assert_eq!(resources.len(), 1);
        assert_eq!(in_flight.len(), 1);
        assert_eq!(in_flight[0].address, address);
        assert_eq!(in_flight[0].step, "create");
        assert_eq!(in_flight[0].lock_identifier, "lock-1");
    }

    #[test]
    fn an_unreadable_marker_is_still_reported_by_its_address() {
        let address = ResourceAddress::new("random_pet", "pet");
        let mut marker = marker_record(&address, "delete", "lock-1").unwrap();
        marker.state = serde_json::json!("garbage");
        let (_, in_flight) = split(vec![marker]);
        assert_eq!(in_flight[0].address, address);
        let error = refuse("tenant", &in_flight).unwrap_err().to_string();
        assert!(error.contains("random_pet.pet"), "{error}");
        assert!(error.contains("cuenv_inflight.random_pet-pet"), "{error}");
    }
}
