//! Secrets named in resource and data-source configuration.
//!
//! A `#Secret` placed at a string argument is resolved at runtime through the
//! same resolvers as project `env`. The engine never hands it to CUE: it
//! replaces the secret's position after evaluation, only for a configuration
//! that is about to reach a provider. Every distinct secret is resolved at most
//! once per [`Secrets`], and its value is registered for redaction before it is
//! returned to the engine.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value as Json};
use tokio::sync::{Mutex, OnceCell};

/// What the configuration says about one secret: its resolver and the fields
/// that resolver reads. Never the value.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretReference {
    descriptor: Map<String, Json>,
}

impl SecretReference {
    /// A secret described by `descriptor`, which must name a `resolver`.
    ///
    /// # Errors
    ///
    /// Returns a value-free reason when `resolver` is missing, empty or not a
    /// string.
    pub fn from_descriptor(descriptor: Map<String, Json>) -> Result<Self, &'static str> {
        if descriptor
            .get("writeOnly")
            .is_some_and(|marker| marker != &Json::Bool(true))
        {
            return Err("writeOnly must be true when present");
        }
        match descriptor.get("resolver") {
            Some(Json::String(resolver)) if !resolver.is_empty() => Ok(Self { descriptor }),
            Some(Json::String(_)) => Err("a secret's `resolver` must not be empty"),
            Some(_) => Err("a secret's `resolver` must be a string"),
            None => Err("a secret must name a `resolver`"),
        }
    }

    /// The resolver kind, such as `onepassword` or `exec`.
    #[must_use]
    pub fn resolver(&self) -> &str {
        self.descriptor
            .get("resolver")
            .and_then(Json::as_str)
            .unwrap_or_default()
    }

    /// Every field of the secret, as configured.
    #[must_use]
    pub const fn descriptor(&self) -> &Map<String, Json> {
        &self.descriptor
    }

    /// A key equal for two descriptions of the same secret, whatever their
    /// field order.
    fn identity(&self) -> String {
        let mut descriptor = self.descriptor.clone();
        descriptor.remove("writeOnly");
        canonical(&Json::Object(descriptor))
    }
}

impl fmt::Debug for SecretReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretReference")
            .field("resolver", &self.resolver())
            .finish_non_exhaustive()
    }
}

/// Why a secret could not be resolved. The reason must never contain the
/// value or anything the resolver printed: it is shown as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFailure {
    reason: String,
}

impl SecretFailure {
    /// A failure with a value-free `reason`.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// The value-free reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for SecretFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.reason)
    }
}

/// Resolves one secret to its value.
#[async_trait]
pub trait SecretResolver: Send + Sync {
    /// The secret's value.
    ///
    /// # Errors
    ///
    /// Returns a value-free [`SecretFailure`] when the secret cannot be
    /// resolved.
    async fn resolve(&self, secret: &SecretReference) -> Result<String, SecretFailure>;
}

/// The secrets of one command, each resolved at most once.
///
/// A value is registered for redaction before anyone receives it, and a plan
/// made again under the lock sees the value the operator's plan saw.
pub struct Secrets {
    resolver: Arc<dyn SecretResolver>,
    resolved: Mutex<BTreeMap<String, Arc<OnceCell<String>>>>,
}

impl Secrets {
    /// Secrets resolved through `resolver`.
    #[must_use]
    pub fn new(resolver: Arc<dyn SecretResolver>) -> Self {
        Self {
            resolver,
            resolved: Mutex::new(BTreeMap::new()),
        }
    }

    /// The value of `secret`, resolving it on first use.
    ///
    /// # Errors
    ///
    /// Returns the resolver's value-free failure. A failed resolution is
    /// retried by the next call.
    pub async fn value(&self, secret: &SecretReference) -> Result<String, SecretFailure> {
        let cell = {
            let mut resolved = self.resolved.lock().await;
            Arc::clone(resolved.entry(secret.identity()).or_default())
        };
        cell.get_or_try_init(|| async {
            let mut descriptor = secret.descriptor.clone();
            descriptor.remove("writeOnly");
            let value = self
                .resolver
                .resolve(&SecretReference { descriptor })
                .await?;
            // Before the value leaves this function: a provider or an error
            // that echoes it is redacted from the first output on.
            cuenv_events::register_secret(value.clone());
            Ok(value)
        })
        .await
        .cloned()
    }
}

impl fmt::Debug for Secrets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Secrets").finish_non_exhaustive()
    }
}

/// JSON text with every object's keys sorted.
fn canonical(value: &Json) -> String {
    match value {
        Json::Object(object) => {
            let sorted: BTreeMap<&String, String> = object
                .iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect();
            let fields: Vec<String> = sorted
                .into_iter()
                .map(|(key, value)| format!("{}:{value}", Json::String(key.clone())))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        Json::Array(items) => format!(
            "[{}]",
            items.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        scalar => scalar.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use super::*;

    struct Counting {
        calls: AtomicUsize,
        fail: bool,
    }

    #[async_trait]
    impl SecretResolver for Counting {
        async fn resolve(&self, secret: &SecretReference) -> Result<String, SecretFailure> {
            assert!(!secret.descriptor().contains_key("writeOnly"));
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(SecretFailure::new("unavailable"));
            }
            Ok(format!("{}-value-{call}", secret.resolver()))
        }
    }

    fn reference(value: Json) -> SecretReference {
        let Json::Object(descriptor) = value else {
            unreachable!("test descriptors are objects")
        };
        SecretReference::from_descriptor(descriptor).unwrap()
    }

    #[test]
    fn requires_a_string_resolver() {
        for (descriptor, reason) in [
            (
                json!({"ref": "op://a/b/c"}),
                "a secret must name a `resolver`",
            ),
            (
                json!({"resolver": 1}),
                "a secret's `resolver` must be a string",
            ),
            (
                json!({"resolver": ""}),
                "a secret's `resolver` must not be empty",
            ),
        ] {
            let Json::Object(descriptor) = descriptor else {
                unreachable!()
            };
            assert_eq!(SecretReference::from_descriptor(descriptor), Err(reason));
        }
    }

    #[test]
    fn debug_shows_only_the_resolver() {
        let secret = reference(json!({"resolver": "exec", "command": "print-token"}));
        let debug = format!("{secret:?}");
        assert!(debug.contains("exec"));
        assert!(!debug.contains("print-token"));
    }

    #[tokio::test]
    async fn resolves_each_secret_once_whatever_its_field_order() {
        let resolver = Arc::new(Counting {
            calls: AtomicUsize::new(0),
            fail: false,
        });
        let secrets = Secrets::new(resolver.clone());
        let first = reference(json!({"resolver": "onepassword", "ref": "op://a/b/c"}));
        let reordered = reference(json!({"ref": "op://a/b/c", "resolver": "onepassword"}));
        let other = reference(json!({"resolver": "onepassword", "ref": "op://a/b/d"}));
        assert_eq!(secrets.value(&first).await.unwrap(), "onepassword-value-0");
        assert_eq!(
            secrets.value(&reordered).await.unwrap(),
            "onepassword-value-0"
        );
        assert_eq!(secrets.value(&other).await.unwrap(), "onepassword-value-1");
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retries_a_failed_resolution() {
        let resolver = Arc::new(Counting {
            calls: AtomicUsize::new(0),
            fail: true,
        });
        let secrets = Secrets::new(resolver.clone());
        let secret = reference(json!({"resolver": "vault", "path": "kv/a"}));
        assert_eq!(
            secrets.value(&secret).await.unwrap_err().reason(),
            "unavailable"
        );
        assert!(secrets.value(&secret).await.is_err());
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn canonical_form_sorts_nested_keys() {
        assert_eq!(
            canonical(&json!({"b": [1, {"d": 2, "c": "x"}], "a": null})),
            r#"{"a":null,"b":[1,{"c":"x","d":2}]}"#
        );
    }
    #[tokio::test]
    async fn ordinary_and_write_only_descriptors_share_one_resolution() {
        let resolver = Arc::new(Counting {
            calls: AtomicUsize::new(0),
            fail: false,
        });
        let secrets = Secrets::new(resolver.clone());
        let ordinary = reference(json!({"resolver":"exec","command":"unused"}));
        let write_only = reference(json!({"resolver":"exec","command":"unused","writeOnly":true}));
        assert_eq!(
            secrets.value(&ordinary).await.unwrap(),
            secrets.value(&write_only).await.unwrap()
        );
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    }
}
