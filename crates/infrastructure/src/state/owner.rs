//! Tenant ownership: the CUE instance that owns a tenant's state.

use crate::error::{InfrastructureError, Result};
use crate::tenant::{ProjectInstance, TenantKey};

/// The CUE instance that owns a tenant's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantOwner {
    /// The owning instance.
    pub instance: ProjectInstance,
    /// When ownership was claimed or last transferred (RFC 3339).
    pub claimed_at: String,
}

impl TenantOwner {
    /// Refuse unless `instance` is this owner.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::OwnedByAnotherInstance`] when another
    /// instance owns the tenant.
    pub fn require(&self, tenant: &TenantKey, instance: &ProjectInstance) -> Result<()> {
        if self.instance == *instance {
            Ok(())
        } else {
            Err(InfrastructureError::OwnedByAnotherInstance {
                tenant: tenant.to_string(),
                owner: self.instance.to_string(),
                instance: instance.to_string(),
            })
        }
    }
}

/// How [`StateStore::claim_owner`] treats an existing owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerClaimMode {
    /// Record the instance only when the tenant has no owner yet; an
    /// existing owner is kept (and returned).
    IfUnowned,
    /// Make the instance the owner, replacing any existing owner (an
    /// explicit adoption).
    Transfer,
}

/// What [`StateStore::claim_owner`] records.
#[derive(Debug, Clone, Copy)]
pub struct OwnerClaim<'claim> {
    /// The instance claiming ownership.
    pub instance: &'claim ProjectInstance,
    /// Whether an existing owner is kept or replaced.
    pub mode: OwnerClaimMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners_refuse_other_instances() {
        let tenant = TenantKey::try_new("example.com/app", "web").unwrap();
        let owner = TenantOwner {
            instance: ProjectInstance::try_new(".", "web").unwrap(),
            claimed_at: "2026-01-01T00:00:00Z".into(),
        };
        owner
            .require(&tenant, &ProjectInstance::try_new("", "web").unwrap())
            .unwrap();
        let error = owner
            .require(&tenant, &ProjectInstance::try_new("_copy", "web").unwrap())
            .unwrap_err();
        assert!(
            matches!(error, InfrastructureError::OwnedByAnotherInstance { .. }),
            "{error}"
        );
        assert!(error.to_string().contains("_copy:web"), "{error}");
        assert!(error.to_string().contains("transfer ownership"), "{error}");
        assert!(!error.to_string().contains("state adopt"), "{error}");
    }
}
