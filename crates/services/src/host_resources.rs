//! Serialize task-scoped services that may compete for host resources.

use std::collections::{BTreeSet, HashMap};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use cuenv_core::manifest::{Readiness, Service};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use crate::probes::http::parse_http_url;

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
enum HostResource {
    Service { root: PathBuf, name: String },
    TcpPort(u16),
    UdpPort(u16),
}

type ResourceLocks = Mutex<HashMap<HostResource, Weak<AsyncMutex<()>>>>;
static RESOURCE_LOCKS: OnceLock<ResourceLocks> = OnceLock::new();

pub async fn lock_service_resources(
    root: &Path,
    services: &HashMap<String, Service>,
) -> crate::Result<Vec<OwnedMutexGuard<()>>> {
    let root = root.canonicalize()?;
    let resources = service_resources(&root, services)?;

    let locks = {
        let mut registry = RESOURCE_LOCKS
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| crate::Error::session("host service resource registry lock poisoned"))?;
        registry.retain(|_, lock| lock.strong_count() > 0);
        resources
            .into_iter()
            .map(|resource| {
                let entry = registry.entry(resource).or_default();
                entry.upgrade().unwrap_or_else(|| {
                    let lock = Arc::new(AsyncMutex::new(()));
                    *entry = Arc::downgrade(&lock);
                    lock
                })
            })
            .collect::<Vec<_>>()
    };

    // Every session acquires its distinct resources in the same order, so
    // two sessions with intersecting service sets cannot create a lock cycle.
    let mut acquired = Vec::with_capacity(locks.len());
    for lock in locks {
        acquired.push(lock.lock_owned().await);
    }
    Ok(acquired)
}

fn service_resources(
    root: &Path,
    services: &HashMap<String, Service>,
) -> crate::Result<BTreeSet<HostResource>> {
    let mut resources = BTreeSet::new();
    let mut declared_ports = HashMap::new();
    for (name, service) in services {
        resources.insert(HostResource::Service {
            root: root.to_path_buf(),
            name: name.clone(),
        });
        for port in &service.ports {
            let (protocol, resource) = if port.protocol == "udp" {
                ("udp", HostResource::UdpPort(port.port))
            } else {
                ("tcp", HostResource::TcpPort(port.port))
            };
            if let Some(other) = declared_ports.insert((protocol, port.port), name)
                && other != name
            {
                return Err(cuenv_core::Error::configuration(format!(
                    "host ephemeral services '{other}' and '{name}' both declare {protocol} port {}; use distinct host ports",
                    port.port
                ))
                .into());
            }
            resources.insert(resource);
        }
        if let Some(port) = readiness_port(service.readiness.as_ref()) {
            resources.insert(HostResource::TcpPort(port));
        }
    }
    Ok(resources)
}

fn readiness_port(readiness: Option<&Readiness>) -> Option<u16> {
    match readiness {
        Some(Readiness::Port(probe)) if probe.host.as_deref().is_none_or(is_loopback) => {
            Some(probe.port)
        }
        Some(Readiness::Http(probe)) => {
            let (host, port, _) = parse_http_url(&probe.url).ok()?;
            is_loopback(host).then_some(port)
        }
        _ => None,
    }
}

fn is_loopback(host: &str) -> bool {
    let ip = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
        || ip.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn declared_port_and_loopback_readiness_share_a_host_lock()
    -> Result<(), Box<dyn std::error::Error>> {
        let first_root = tempfile::tempdir()?;
        let second_root = tempfile::tempdir()?;
        let declared = serde_json::from_value(serde_json::json!({
            "ports": [{"port": 32562}]
        }))?;
        let first_project = HashMap::from([("web".to_string(), declared)]);
        let readiness = serde_json::from_value(serde_json::json!({
            "readiness": {"kind": "http", "url": "http://localhost:32562/ready"}
        }))?;
        let second_project = HashMap::from([("api".to_string(), readiness)]);

        let first = lock_service_resources(first_root.path(), &first_project).await?;
        let mut second = Box::pin(lock_service_resources(second_root.path(), &second_project));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut second)
                .await
                .is_err()
        );
        drop(first);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), second)
                .await??
                .len()
                >= 2
        );
        Ok(())
    }

    #[test]
    fn different_services_cannot_declare_the_same_port_and_protocol()
    -> Result<(), Box<dyn std::error::Error>> {
        let first: Service = serde_json::from_value(serde_json::json!({
            "ports": [{"port": 32563}]
        }))?;
        let services = HashMap::from([
            ("worker".to_string(), first.clone()),
            ("api".to_string(), first),
        ]);
        let root = Path::new(".");
        assert!(
            service_resources(root, &services)
                .is_err_and(|error| { error.to_string().contains("both declare tcp port 32563") })
        );
        Ok(())
    }

    #[test]
    fn distinct_protocols_and_readiness_aliases_can_share_a_port_number()
    -> Result<(), Box<dyn std::error::Error>> {
        let tcp = serde_json::from_value(serde_json::json!({
            "ports": [{"port": 32564}]
        }))?;
        let udp = serde_json::from_value(serde_json::json!({
            "ports": [{"port": 32564, "protocol": "udp"}],
            "readiness": {"kind": "port", "port": 32564}
        }))?;
        let services = HashMap::from([("web".to_string(), tcp), ("worker".to_string(), udp)]);
        let resources = service_resources(Path::new("."), &services)?;
        assert!(resources.contains(&HostResource::TcpPort(32564)));
        assert!(resources.contains(&HostResource::UdpPort(32564)));
        Ok(())
    }
}
