//! A backend-neutral, deny-by-default capability contract.
//!
//! The types in this module are intentionally serialization-independent. A
//! future Wasm, IPC, or remote-plugin adapter may encode them, but this module
//! remains the host's authorization boundary.
//!
//! The `fs` namespace is lexical only: the host must resolve symlinks and
//! apply its case-sensitivity policy before a filesystem grant or request
//! enters this model. This module cannot prove filesystem identity by itself.

const MAX_ID_LEN: usize = 128;
const MAX_PRINCIPAL_LEN: usize = 128;
const MAX_SCOPE_PART_LEN: usize = 256;
const MAX_REASON_LEN: usize = 512;

/// Capabilities understood by the host. There is no implicit permission for a
/// capability that is not represented here.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CapabilityKind {
    FilesystemRead,
    FilesystemWrite,
    ProcessSpawn,
    NetworkConnect,
    ClipboardRead,
    ClipboardWrite,
    /// A decoded or future capability that this host does not understand.
    Unknown(String),
}

impl CapabilityKind {
    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Unknown(_))
    }
}

/// The resource boundary to which a capability applies.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ResourceScope {
    /// Explicitly host-wide. This should be granted sparingly.
    Global,
    /// An exact named resource in a namespace.
    Named { namespace: String, resource: String },
    /// A namespace-local prefix, useful for a directory or endpoint family.
    Prefix { namespace: String, prefix: String },
}

impl ResourceScope {
    fn validate(&self) -> Result<(), RequestError> {
        match self {
            Self::Global => Ok(()),
            Self::Named {
                namespace,
                resource,
            } => {
                validate_scope_part(namespace)?;
                validate_resource(namespace, resource)
            }
            Self::Prefix { namespace, prefix } => {
                validate_scope_part(namespace)?;
                validate_resource(namespace, prefix)
            }
        }
    }

    /// Returns whether this validated granted scope contains the requested scope.
    ///
    /// Keep this helper private so every external authorization request passes
    /// through [`CapabilityRequest::validate`] first.
    fn contains(&self, requested: &Self) -> bool {
        match (self, requested) {
            (Self::Global, _) => true,
            (
                Self::Named {
                    namespace: a_ns,
                    resource: a_resource,
                },
                Self::Named {
                    namespace: b_ns,
                    resource: b_resource,
                },
            ) => a_ns == b_ns && resources_equal(a_ns, a_resource, b_resource),
            (
                Self::Prefix {
                    namespace: a_ns,
                    prefix: a_prefix,
                },
                Self::Named {
                    namespace: b_ns,
                    resource: b_resource,
                },
            ) => b_ns == a_ns && resource_contains(a_ns, a_prefix, b_resource),
            (
                Self::Prefix {
                    namespace: a_ns,
                    prefix: a_prefix,
                },
                Self::Prefix {
                    namespace: b_ns,
                    prefix: b_prefix,
                },
            ) => b_ns == a_ns && resource_contains(a_ns, a_prefix, b_prefix),
            // A grant for one exact resource cannot authorize a wider request.
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityRequest {
    pub request_id: String,
    pub principal: String,
    pub capability: CapabilityKind,
    pub scope: ResourceScope,
    pub reason: String,
}

impl CapabilityRequest {
    pub fn validate(&self) -> Result<(), RequestError> {
        validate_bounded("request_id", &self.request_id, MAX_ID_LEN)?;
        validate_bounded("principal", &self.principal, MAX_PRINCIPAL_LEN)?;
        validate_bounded("reason", &self.reason, MAX_REASON_LEN)?;
        if !self.capability.is_known() {
            return Err(RequestError::UnknownCapability);
        }
        self.scope.validate()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityGrant {
    pub grant_id: String,
    pub principal: String,
    pub capability: CapabilityKind,
    pub scope: ResourceScope,
}

impl CapabilityGrant {
    pub fn new(
        grant_id: impl Into<String>,
        principal: impl Into<String>,
        capability: CapabilityKind,
        scope: ResourceScope,
    ) -> Self {
        Self {
            grant_id: grant_id.into(),
            principal: principal.into(),
            capability,
            scope,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecisionReason {
    Granted,
    InvalidRequest(RequestError),
    UnknownCapability,
    NoMatchingGrant,
    RevokedGrant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationDecision {
    Allow { grant_id: String },
    Deny { reason: DecisionReason },
}

impl AuthorizationDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }
}

/// An append-only, secret-free record of an authorization decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditRecord {
    pub request_id: String,
    pub principal: String,
    pub capability: CapabilityKind,
    pub scope: ResourceScope,
    pub decision: AuthorizationDecision,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequestError {
    EmptyField(&'static str),
    OversizedField { field: &'static str, max: usize },
    ControlCharacter(&'static str),
    UnknownCapability,
    InvalidFilesystemPath(&'static str),
}

fn validate_bounded(field: &'static str, value: &str, max: usize) -> Result<(), RequestError> {
    if value.is_empty() {
        return Err(RequestError::EmptyField(field));
    }
    if value.chars().count() > max {
        return Err(RequestError::OversizedField { field, max });
    }
    if value.chars().any(char::is_control) {
        return Err(RequestError::ControlCharacter(field));
    }
    Ok(())
}

fn validate_scope_part(value: &str) -> Result<(), RequestError> {
    validate_bounded("scope", value, MAX_SCOPE_PART_LEN)
}

const FILESYSTEM_NAMESPACE: &str = "fs";

fn validate_resource(namespace: &str, resource: &str) -> Result<(), RequestError> {
    validate_scope_part(resource)?;
    if namespace == FILESYSTEM_NAMESPACE {
        canonical_filesystem_path(resource).map(|_| ())
    } else {
        Ok(())
    }
}

fn resources_equal(namespace: &str, left: &str, right: &str) -> bool {
    if namespace == FILESYSTEM_NAMESPACE {
        canonical_filesystem_path(left) == canonical_filesystem_path(right)
    } else {
        left == right
    }
}

fn resource_contains(namespace: &str, granted: &str, requested: &str) -> bool {
    if namespace != FILESYSTEM_NAMESPACE {
        return requested.starts_with(granted);
    }
    let Ok(granted) = canonical_filesystem_path(granted) else {
        return false;
    };
    let Ok(requested) = canonical_filesystem_path(requested) else {
        return false;
    };
    granted == "/" || granted == requested || requested.starts_with(&(granted + "/"))
}

/// Returns a stable lexical form for a host-resolved filesystem path.
///
/// This deliberately does not touch the filesystem. Relative paths, traversal,
/// repeated separators, and backslashes are rejected rather than guessed at.
fn canonical_filesystem_path(path: &str) -> Result<String, RequestError> {
    if !path.starts_with('/') {
        return Err(RequestError::InvalidFilesystemPath("absolute"));
    }
    if path.contains('\\') {
        return Err(RequestError::InvalidFilesystemPath("backslash"));
    }
    if path.contains("//") {
        return Err(RequestError::InvalidFilesystemPath("normalization"));
    }
    let path = if path.len() > 1 && path.ends_with('/') {
        &path[..path.len() - 1]
    } else {
        path
    };
    let mut parts = path.split('/');
    let _root = parts.next();
    let mut canonical = String::new();
    for part in parts {
        if matches!(part, "." | "..") {
            return Err(RequestError::InvalidFilesystemPath("traversal"));
        }
        canonical.push('/');
        canonical.push_str(part);
    }
    if canonical.is_empty() {
        canonical.push('/');
    }
    Ok(canonical)
}

/// Mutable host policy. Revocation is effective immediately for subsequent
/// authorization calls and does not remove the historical grant record.
#[derive(Clone, Debug, Default)]
pub struct GrantSet {
    grants: Vec<CapabilityGrant>,
    revoked: Vec<String>,
}

impl GrantSet {
    pub fn grant(&mut self, grant: CapabilityGrant) -> Result<(), RequestError> {
        validate_bounded("grant_id", &grant.grant_id, MAX_ID_LEN)?;
        validate_bounded("principal", &grant.principal, MAX_PRINCIPAL_LEN)?;
        if !grant.capability.is_known() {
            return Err(RequestError::UnknownCapability);
        }
        grant.scope.validate()?;
        if self
            .grants
            .iter()
            .any(|existing| existing.grant_id == grant.grant_id)
        {
            return Err(RequestError::EmptyField("duplicate_grant_id"));
        }
        self.grants.push(grant);
        Ok(())
    }

    pub fn revoke(&mut self, grant_id: &str) -> bool {
        if self.grants.iter().any(|grant| grant.grant_id == grant_id)
            && !self.revoked.iter().any(|id| id == grant_id)
        {
            self.revoked.push(grant_id.to_owned());
            true
        } else {
            false
        }
    }

    pub fn is_revoked(&self, grant_id: &str) -> bool {
        self.revoked.iter().any(|id| id == grant_id)
    }

    fn decide(&self, request: &CapabilityRequest) -> AuthorizationDecision {
        if let Err(error) = request.validate() {
            return AuthorizationDecision::Deny {
                reason: if matches!(error, RequestError::UnknownCapability) {
                    DecisionReason::UnknownCapability
                } else {
                    DecisionReason::InvalidRequest(error)
                },
            };
        }

        let mut had_revoked_match = false;
        // The first matching grant is deterministic because grant IDs are
        // unique and the set preserves insertion order.
        for grant in &self.grants {
            if grant.principal == request.principal
                && grant.capability == request.capability
                && grant.scope.contains(&request.scope)
            {
                if self.is_revoked(&grant.grant_id) {
                    had_revoked_match = true;
                } else {
                    return AuthorizationDecision::Allow {
                        grant_id: grant.grant_id.clone(),
                    };
                }
            }
        }
        AuthorizationDecision::Deny {
            reason: if had_revoked_match {
                DecisionReason::RevokedGrant
            } else {
                DecisionReason::NoMatchingGrant
            },
        }
    }
}

/// Host authorization entry point for future plugin adapters.
pub trait HostAuthorizer {
    fn authorize(&self, request: &CapabilityRequest) -> (AuthorizationDecision, AuditRecord);
}

impl HostAuthorizer for GrantSet {
    fn authorize(&self, request: &CapabilityRequest) -> (AuthorizationDecision, AuditRecord) {
        let decision = self.decide(request);
        let audit = AuditRecord {
            request_id: request.request_id.clone(),
            principal: request.principal.clone(),
            capability: request.capability.clone(),
            scope: request.scope.clone(),
            decision: decision.clone(),
        };
        (decision, audit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(capability: CapabilityKind, scope: ResourceScope) -> CapabilityRequest {
        CapabilityRequest {
            request_id: "req-1".into(),
            principal: "plugin.alpha".into(),
            capability,
            scope,
            reason: "needed for test".into(),
        }
    }

    fn named(resource: &str) -> ResourceScope {
        ResourceScope::Named {
            namespace: "fs".into(),
            resource: if resource.starts_with('/') {
                resource.into()
            } else {
                format!("/{resource}")
            },
        }
    }

    #[test]
    fn denies_by_default_and_unknown_capability() {
        let grants = GrantSet::default();
        let (decision, _) = grants.authorize(&request(CapabilityKind::FilesystemRead, named("a")));
        assert_eq!(
            decision,
            AuthorizationDecision::Deny {
                reason: DecisionReason::NoMatchingGrant
            }
        );
        let (decision, _) = grants.authorize(&request(
            CapabilityKind::Unknown("future".into()),
            named("a"),
        ));
        assert_eq!(
            decision,
            AuthorizationDecision::Deny {
                reason: DecisionReason::UnknownCapability
            }
        );
    }

    #[test]
    fn scoped_grant_does_not_cross_resource_or_capability() {
        let mut grants = GrantSet::default();
        grants
            .grant(CapabilityGrant::new(
                "g1",
                "plugin.alpha",
                CapabilityKind::FilesystemRead,
                named("a"),
            ))
            .unwrap();
        assert!(
            grants
                .authorize(&request(CapabilityKind::FilesystemRead, named("a")))
                .0
                .is_allowed()
        );
        assert!(
            !grants
                .authorize(&request(CapabilityKind::FilesystemRead, named("b")))
                .0
                .is_allowed()
        );
        assert!(
            !grants
                .authorize(&request(CapabilityKind::FilesystemWrite, named("a")))
                .0
                .is_allowed()
        );
    }

    #[test]
    fn prefix_grant_is_narrower_than_global_but_contains_children() {
        let mut grants = GrantSet::default();
        grants
            .grant(CapabilityGrant::new(
                "g1",
                "plugin.alpha",
                CapabilityKind::FilesystemRead,
                ResourceScope::Prefix {
                    namespace: "fs".into(),
                    prefix: "/safe/".into(),
                },
            ))
            .unwrap();
        assert!(
            grants
                .authorize(&request(
                    CapabilityKind::FilesystemRead,
                    named("/safe/file")
                ))
                .0
                .is_allowed()
        );
        assert!(
            !grants
                .authorize(&request(
                    CapabilityKind::FilesystemRead,
                    named("/unsafe/file")
                ))
                .0
                .is_allowed()
        );
    }

    #[test]
    fn filesystem_prefix_is_segment_bounded() {
        let mut grants = GrantSet::default();
        grants
            .grant(CapabilityGrant::new(
                "g1",
                "plugin.alpha",
                CapabilityKind::FilesystemRead,
                ResourceScope::Prefix {
                    namespace: "fs".into(),
                    prefix: "/safe".into(),
                },
            ))
            .unwrap();
        assert!(
            grants
                .authorize(&request(
                    CapabilityKind::FilesystemRead,
                    named("/safe/file")
                ))
                .0
                .is_allowed()
        );
        assert!(
            !grants
                .authorize(&request(
                    CapabilityKind::FilesystemRead,
                    named("/safe2/file")
                ))
                .0
                .is_allowed()
        );
    }

    #[test]
    fn filesystem_traversal_is_rejected_before_authorization() {
        let mut grants = GrantSet::default();
        let error = grants
            .grant(CapabilityGrant::new(
                "g1",
                "plugin.alpha",
                CapabilityKind::FilesystemRead,
                ResourceScope::Prefix {
                    namespace: "fs".into(),
                    prefix: "/safe/../secret".into(),
                },
            ))
            .unwrap_err();
        assert_eq!(error, RequestError::InvalidFilesystemPath("traversal"));

        let (decision, _) = grants.authorize(&request(
            CapabilityKind::FilesystemRead,
            named("/safe/../secret"),
        ));
        assert!(matches!(
            decision,
            AuthorizationDecision::Deny {
                reason: DecisionReason::InvalidRequest(RequestError::InvalidFilesystemPath(
                    "traversal"
                ))
            }
        ));
    }

    #[test]
    fn filesystem_paths_have_deterministic_trailing_slash_normalization() {
        let mut grants = GrantSet::default();
        grants
            .grant(CapabilityGrant::new(
                "g1",
                "plugin.alpha",
                CapabilityKind::FilesystemRead,
                ResourceScope::Named {
                    namespace: "fs".into(),
                    resource: "/safe/".into(),
                },
            ))
            .unwrap();
        assert!(
            grants
                .authorize(&request(CapabilityKind::FilesystemRead, named("/safe")))
                .0
                .is_allowed()
        );

        for invalid in ["safe/file", "/safe//file", "/safe/./file", "/safe\\file"] {
            let (decision, _) = grants.authorize(&request(
                CapabilityKind::FilesystemRead,
                ResourceScope::Named {
                    namespace: "fs".into(),
                    resource: invalid.into(),
                },
            ));
            assert!(
                matches!(
                    decision,
                    AuthorizationDecision::Deny {
                        reason: DecisionReason::InvalidRequest(
                            RequestError::InvalidFilesystemPath(_)
                        )
                    }
                ),
                "invalid path should be denied: {}",
                invalid
            );
        }
    }

    #[test]
    fn filesystem_paths_reject_repeated_trailing_separators() {
        let mut grants = GrantSet::default();
        grants
            .grant(CapabilityGrant::new(
                "g1",
                "plugin.alpha",
                CapabilityKind::FilesystemRead,
                ResourceScope::Prefix {
                    namespace: "fs".into(),
                    prefix: "/safe".into(),
                },
            ))
            .unwrap();

        let (decision, _) =
            grants.authorize(&request(CapabilityKind::FilesystemRead, named("/safe//")));
        assert!(matches!(
            decision,
            AuthorizationDecision::Deny {
                reason: DecisionReason::InvalidRequest(RequestError::InvalidFilesystemPath(
                    "normalization"
                ))
            }
        ));
    }

    #[test]
    fn non_filesystem_prefixes_remain_raw_and_deterministic() {
        let mut grants = GrantSet::default();
        grants
            .grant(CapabilityGrant::new(
                "g1",
                "plugin.alpha",
                CapabilityKind::NetworkConnect,
                ResourceScope::Prefix {
                    namespace: "endpoint".into(),
                    prefix: "api://example".into(),
                },
            ))
            .unwrap();
        assert!(
            grants
                .authorize(&request(
                    CapabilityKind::NetworkConnect,
                    ResourceScope::Named {
                        namespace: "endpoint".into(),
                        resource: "api://example/v1".into(),
                    },
                ))
                .0
                .is_allowed()
        );
    }

    #[test]
    fn revocation_is_immediate_and_audited_without_secrets() {
        let mut grants = GrantSet::default();
        grants
            .grant(CapabilityGrant::new(
                "g1",
                "plugin.alpha",
                CapabilityKind::ClipboardRead,
                ResourceScope::Global,
            ))
            .unwrap();
        assert!(
            grants
                .authorize(&request(
                    CapabilityKind::ClipboardRead,
                    ResourceScope::Global
                ))
                .0
                .is_allowed()
        );
        assert!(grants.revoke("g1"));
        let (decision, audit) = grants.authorize(&request(
            CapabilityKind::ClipboardRead,
            ResourceScope::Global,
        ));
        assert_eq!(
            decision,
            AuthorizationDecision::Deny {
                reason: DecisionReason::RevokedGrant
            }
        );
        assert_eq!(
            audit,
            AuditRecord {
                request_id: "req-1".into(),
                principal: "plugin.alpha".into(),
                capability: CapabilityKind::ClipboardRead,
                scope: ResourceScope::Global,
                decision,
            }
        );
    }

    #[test]
    fn malformed_and_oversized_requests_are_rejected() {
        let grants = GrantSet::default();
        let mut malformed = request(CapabilityKind::FilesystemRead, named("a"));
        malformed.request_id.clear();
        assert!(matches!(
            grants.authorize(&malformed).0,
            AuthorizationDecision::Deny {
                reason: DecisionReason::InvalidRequest(RequestError::EmptyField("request_id"))
            }
        ));
        malformed.request_id = "r".repeat(MAX_ID_LEN + 1);
        assert!(matches!(
            grants.authorize(&malformed).0,
            AuthorizationDecision::Deny {
                reason: DecisionReason::InvalidRequest(RequestError::OversizedField {
                    field: "request_id",
                    ..
                })
            }
        ));
    }

    #[test]
    fn authorization_is_deterministic_for_same_grant_set() {
        let mut grants = GrantSet::default();
        grants
            .grant(CapabilityGrant::new(
                "g1",
                "plugin.alpha",
                CapabilityKind::FilesystemRead,
                named("a"),
            ))
            .unwrap();
        let first = grants
            .authorize(&request(CapabilityKind::FilesystemRead, named("a")))
            .0;
        let second = grants
            .authorize(&request(CapabilityKind::FilesystemRead, named("a")))
            .0;
        assert_eq!(first, second);
    }
}
