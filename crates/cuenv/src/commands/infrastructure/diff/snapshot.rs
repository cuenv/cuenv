//! The configuration a diff compares: every provider, managed resource and
//! data source of the selected configuration as a readable value, secrets as
//! what they name and output references by the output they read.

use std::collections::{BTreeMap, BTreeSet};

use cuenv_infrastructure::references::{ConfigurationSelector, NativeReference};
use cuenv_infrastructure::registry::ProviderSource;
use cuenv_manifest::lockfile::LockedInfrastructureProvider;
use cuenv_manifest::manifest::{
    Infrastructure, InfrastructureProvider, ManagedResourceDeclaration,
};
use serde_json::Value as Json;

/// The resolvers `#Secret` accepts: an object naming one of them where a
/// configuration value belongs is a secret reference.
const SECRET_RESOLVERS: [&str; 6] = ["aws", "gcp", "onepassword", "vault", "infisical", "exec"];

/// One value of a declaration, as the diff shows it.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Value {
    /// A JSON null, boolean, number or string.
    Scalar(Json),
    /// A `#Secret`: what it names, never its value.
    Secret(Secret),
    /// A value read from, or computed from, outputs of other declarations.
    Output(Output),
    /// A list or tuple.
    List(Vec<Self>),
    /// A struct or map, by key.
    Object(BTreeMap<String, Self>),
}

impl Value {
    /// Whether the value holds other values a change can be located in.
    pub(super) const fn is_structured(&self) -> bool {
        matches!(self, Self::List(_) | Self::Object(_))
    }
}

/// A secret reference: the resolver and the fields that tell it which secret
/// to read.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Secret {
    /// The resolver, such as `onepassword`.
    pub(super) resolver: String,
    /// The other fields of the reference, such as `ref`, without
    /// `writeOnly`.
    pub(super) fields: BTreeMap<String, Json>,
    /// Refined with `#WriteOnly`.
    pub(super) write_only: bool,
}

/// One output another declaration provides: its address and attribute.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct OutputSource {
    /// `type.name` of a managed resource, `data.type.name` of a data source.
    pub(super) address: String,
    /// The output attribute.
    pub(super) attribute: String,
}

/// A configuration value that comes from outputs of other declarations,
/// known only once they exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Output {
    /// Every output the value reads, in order.
    pub(super) sources: BTreeSet<OutputSource>,
    /// CUE computes the value from the outputs instead of carrying one.
    pub(super) computed: bool,
}

/// The declarations of one configuration, each keyed by its address: the
/// local name of a provider, `type.name` of a managed resource and
/// `data.type.name` of a data source.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Snapshot {
    pub(super) providers: BTreeMap<String, Value>,
    pub(super) resources: BTreeMap<String, Value>,
    pub(super) data_sources: BTreeMap<String, Value>,
}

/// What a snapshot is taken of.
pub(super) struct Selected<'selected> {
    /// The selected configuration, as evaluation decodes it.
    pub(super) infrastructure: &'selected Infrastructure,
    /// The output references evaluation found; their destinations are
    /// `null` in `infrastructure`.
    pub(super) references: &'selected [NativeReference],
    /// The `cuenv.lock` pins that decide the provider versions and schema
    /// digests, when the configuration was evaluated with generated provider
    /// types of another revision (which set both).
    pub(super) pins: Option<&'selected BTreeMap<String, LockedInfrastructureProvider>>,
}

impl Snapshot {
    /// Take a snapshot of a selected configuration.
    pub(super) fn of(selected: &Selected<'_>) -> Self {
        let infrastructure = selected.infrastructure;
        let mut snapshot = Self {
            providers: infrastructure
                .providers
                .iter()
                .map(|(name, provider)| (name.clone(), provider_value(provider, selected.pins)))
                .collect(),
            resources: declarations(&infrastructure.resources, ""),
            data_sources: declarations(&infrastructure.data, "data."),
        };
        for reference in selected.references {
            snapshot.place(reference);
        }
        snapshot
    }

    /// Put an output reference at its destination in its consumer's
    /// configuration.
    fn place(&mut self, reference: &NativeReference) {
        let consumer = reference.consumer.to_string();
        let declaration = self
            .resources
            .get_mut(&consumer)
            .or_else(|| self.data_sources.get_mut(&consumer));
        let Some(Value::Object(fields)) = declaration else {
            return;
        };
        let Some(mut destination) = fields.get_mut("configuration") else {
            return;
        };
        for selector in &reference.destination {
            let next = match (selector, destination) {
                (ConfigurationSelector::Field(name), Value::Object(fields)) => fields.get_mut(name),
                (ConfigurationSelector::Index(index), Value::List(items)) => {
                    usize::try_from(*index)
                        .ok()
                        .and_then(|index| items.get_mut(index))
                }
                _ => None,
            };
            let Some(next) = next else {
                return;
            };
            destination = next;
        }
        let source = OutputSource {
            address: reference.source.to_string(),
            attribute: reference.attribute.clone(),
        };
        match destination {
            Value::Output(output) => {
                output.sources.insert(source);
                output.computed |= reference.computed;
            }
            other => {
                *other = Value::Output(Output {
                    sources: BTreeSet::from([source]),
                    computed: reference.computed,
                });
            }
        }
    }
}

/// Managed resources or data sources keyed by `<prefix>type.name`.
fn declarations(
    declarations: &BTreeMap<String, ManagedResourceDeclaration>,
    prefix: &str,
) -> BTreeMap<String, Value> {
    declarations
        .iter()
        .map(|(name, declaration)| {
            (
                format!("{prefix}{}.{name}", declaration.resource_type),
                declaration_value(declaration),
            )
        })
        .collect()
}

/// The fields of a managed resource or data source a diff compares. The
/// provider is the one the declaration uses, named or derived from its type.
fn declaration_value(declaration: &ManagedResourceDeclaration) -> Value {
    let mut fields = BTreeMap::from([(
        "provider".to_string(),
        Value::Scalar(Json::String(declaration.provider_name().to_string())),
    )]);
    if !declaration.depends_on.is_empty() {
        fields.insert(
            "dependsOn".to_string(),
            Value::List(
                declaration
                    .depends_on
                    .iter()
                    .map(|name| Value::Scalar(Json::String(name.clone())))
                    .collect(),
            ),
        );
    }
    if !declaration.configuration.is_empty() {
        fields.insert(
            "configuration".to_string(),
            object(&declaration.configuration),
        );
    }
    Value::Object(fields)
}

/// The fields of a provider a diff compares.
fn provider_value(
    provider: &InfrastructureProvider,
    pins: Option<&BTreeMap<String, LockedInfrastructureProvider>>,
) -> Value {
    let pin = pins.and_then(|pins| {
        ProviderSource::parse(&provider.source)
            .ok()
            .and_then(|source| pins.get(&source.to_string()))
    });
    let text = |value: &str| Value::Scalar(Json::String(value.to_string()));
    let mut fields = BTreeMap::from([("source".to_string(), text(&provider.source))]);
    if let Some(version) = &provider.version {
        let version = pin.map_or(version.as_str(), |pin| pin.version.as_str());
        fields.insert("version".to_string(), text(version));
    }
    if let Some(path) = &provider.path {
        fields.insert("path".to_string(), text(path));
    }
    if let Some(digest) = &provider.schema_digest {
        let digest = pin.map_or(digest.as_str(), |pin| pin.schema_digest.as_str());
        fields.insert("schemaDigest".to_string(), text(digest));
    }
    if !provider.configuration.is_empty() {
        fields.insert("configuration".to_string(), object(&provider.configuration));
    }
    Value::Object(fields)
}

fn object(fields: &serde_json::Map<String, Json>) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|(key, value)| (key.clone(), value_of(value)))
            .collect(),
    )
}

/// A configuration value as the diff shows it.
pub(super) fn value_of(value: &Json) -> Value {
    match value {
        Json::Array(items) => Value::List(items.iter().map(value_of).collect()),
        Json::Object(fields) => secret(fields).map_or_else(|| object(fields), Value::Secret),
        scalar => Value::Scalar(scalar.clone()),
    }
}

/// The secret an object describes, if it is a `#Secret`.
fn secret(fields: &serde_json::Map<String, Json>) -> Option<Secret> {
    let resolver = fields.get("resolver").and_then(Json::as_str)?;
    if !SECRET_RESOLVERS.contains(&resolver) {
        return None;
    }
    Some(Secret {
        resolver: resolver.to_string(),
        fields: fields
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "resolver" | "writeOnly"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        write_only: fields.get("writeOnly") == Some(&Json::Bool(true)),
    })
}
