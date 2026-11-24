//! Concrete output references and functions, as plain CUE exports them.
//!
//! The schema fills every `outputs` field of a resource or data source with a
//! descriptor that names it, and the `#Format`, `#Sha256`, `#Base64`, `#Join`
//! and `#Select` definitions export as descriptors too, so an infrastructure
//! configuration is always concrete data that `cue export` accepts:
//!
//! ```json
//! {"cuenvOutput": {"namespace": "resources", "name": "bucket", "environment": "", "path": ["id"]}, "type": "string"}
//! {"cuenvFunction": "sha256", "of": {"cuenvOutput": ...}, "encoding": "hex", "type": "string"}
//! ```
//!
//! [`lower`] takes those descriptors out of the selected configuration, leaving
//! `null` where each one was, and returns an evaluator that puts the real values
//! back when the engine knows them. A descriptor anywhere but a resource or data
//! source configuration is refused, as is a string that carries one, which is
//! what encoding a descriptor with `json.Marshal` or `yaml.Marshal` produces.

use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use cuenv_manifest::manifest::InfrastructureOutputType;
use serde_json::{Map, Value as Json};
use sha2::{Digest as _, Sha256};

use super::{
    ConfigurationEvaluator, ConfigurationSelector, ConfigurationSpecialization,
    ConfigurationVerification, NativeReference, OutputIdentity, OutputPath, SourceAddress,
    SpecializedConfiguration,
};
use crate::error::{InfrastructureError, Result};
use crate::state::ResourceAddress;
use crate::type_system::{Number, PathStep, Value};

/// The key that marks an output descriptor.
const OUTPUT_KEY: &str = "cuenvOutput";
/// The key that marks a function descriptor.
const FUNCTION_KEY: &str = "cuenvFunction";

/// What [`lower`] took out of a selected configuration.
#[derive(Debug, Default)]
pub struct LoweredReferences {
    evaluator: Option<ExpressionEvaluator>,
    output_types: BTreeMap<SourceAddress, BTreeMap<String, InfrastructureOutputType>>,
}

impl LoweredReferences {
    /// The evaluator, when the configuration declares an output or reads one.
    #[must_use]
    pub fn into_evaluator(self) -> Option<ExpressionEvaluator> {
        self.evaluator
    }

    /// The scalar outputs each declaration declares, by top-level attribute.
    #[must_use]
    pub const fn output_types(
        &self,
    ) -> &BTreeMap<SourceAddress, BTreeMap<String, InfrastructureOutputType>> {
        &self.output_types
    }
}

/// Which configuration to lower.
#[derive(Debug, Clone, Copy)]
pub struct Selection<'selection> {
    /// The environment the configuration belongs to; `None` for the top level.
    pub environment: Option<&'selection str>,
}

impl Selection<'_> {
    fn environment_label(&self) -> &str {
        self.environment.unwrap_or("")
    }
}

/// Take every output declaration and reference out of one selected
/// infrastructure configuration: the top-level `infrastructure` object, or the
/// one environment the run selected.
///
/// # Errors
///
/// Returns a value-free configuration error for a descriptor outside a
/// resource or data source configuration, one that names another environment
/// or an undeclared output, a list or object output used where a scalar is
/// needed, or a string that carries an encoded descriptor.
pub fn lower(configuration: &mut Json, selection: Selection<'_>) -> Result<LoweredReferences> {
    let Some(root) = configuration.as_object_mut() else {
        return Ok(LoweredReferences::default());
    };
    let sources = declared_sources(root)?;
    let mut declarations = Declarations::default();
    for (namespace, name, declaration) in declarations_mut(root) {
        let source = sources.address(namespace, name)?.clone();
        if let Some(outputs) = declaration.remove("outputs") {
            declarations.record(&Outputs {
                source: &source,
                descriptors: &outputs,
                expected: DescriptorOwner {
                    namespace,
                    name,
                    environment: selection.environment_label(),
                },
            })?;
        }
    }
    let resolver = Resolver {
        sources: &sources,
        declarations: &declarations,
        selection,
    };
    let mut placements = Vec::new();
    let mut bases = BTreeMap::new();
    for (namespace, name, declaration) in declarations_mut(root) {
        let consumer = sources.address(namespace, name)?.clone();
        if let Some(Json::Object(arguments)) = declaration.get_mut("configuration") {
            let mut walk = Walk {
                consumer: &consumer,
                resolver: &resolver,
                placements: &mut placements,
            };
            for (field, value) in arguments.iter_mut() {
                walk.visit(value, vec![ConfigurationSelector::Field(field.clone())])?;
            }
            bases.insert(consumer, arguments.clone());
        }
    }
    // Unselected environments are dropped before the configuration is decoded.
    root.iter()
        .filter(|(field, _)| field.as_str() != "environments")
        .try_for_each(|(_, value)| refuse_descriptors(value))?;
    let output_types = declarations.scalar_types();
    if placements.is_empty() && declarations.identities.is_empty() {
        return Ok(LoweredReferences {
            evaluator: None,
            output_types,
        });
    }
    let references = placements
        .iter()
        .flat_map(Placement::references)
        .collect::<Vec<_>>();
    Ok(LoweredReferences {
        evaluator: Some(ExpressionEvaluator {
            references,
            declared: declarations.identities.keys().cloned().collect(),
            declared_types: declarations.identities,
            placements,
            bases,
        }),
        output_types,
    })
}

/// The declarations of both namespaces, with the namespace and name of each.
fn declarations_mut(
    root: &mut Map<String, Json>,
) -> impl Iterator<Item = (&'static str, &str, &mut Map<String, Json>)> {
    root.iter_mut()
        .filter_map(|(field, value)| {
            let namespace = match field.as_str() {
                "resources" => "resources",
                "data" => "data",
                _ => return None,
            };
            Some((namespace, value.as_object_mut()?))
        })
        .flat_map(|(namespace, declarations)| {
            declarations
                .iter_mut()
                .filter_map(move |(name, declaration)| {
                    Some((namespace, name.as_str(), declaration.as_object_mut()?))
                })
        })
}

/// The address of every declared resource and data source.
#[derive(Debug, Default)]
struct Sources {
    resources: BTreeMap<String, SourceAddress>,
    data: BTreeMap<String, SourceAddress>,
}

impl Sources {
    fn address(&self, namespace: &str, name: &str) -> Result<&SourceAddress> {
        let declared = match namespace {
            "resources" => &self.resources,
            _ => &self.data,
        };
        declared.get(name).ok_or_else(|| {
            InfrastructureError::configuration(format!(
                "an output reference names {namespace}.{name}, which the selected configuration does not declare"
            ))
        })
    }
}

fn declared_sources(root: &Map<String, Json>) -> Result<Sources> {
    let mut sources = Sources::default();
    for (namespace, target) in [
        ("resources", &mut sources.resources),
        ("data", &mut sources.data),
    ] {
        let Some(declarations) = root.get(namespace).and_then(Json::as_object) else {
            continue;
        };
        for (name, declaration) in declarations {
            let Some(resource_type) = declaration.get("type").and_then(Json::as_str) else {
                return Err(InfrastructureError::configuration(format!(
                    "{namespace}.{name}: `type` must be a string"
                )));
            };
            let address = ResourceAddress::new(resource_type, name);
            target.insert(
                name.clone(),
                if namespace == "resources" {
                    SourceAddress::Resource(address)
                } else {
                    SourceAddress::DataSource(address)
                },
            );
        }
    }
    Ok(sources)
}

/// The declaration a descriptor must name.
#[derive(Debug, Clone, Copy)]
struct DescriptorOwner<'owner> {
    namespace: &'owner str,
    name: &'owner str,
    environment: &'owner str,
}

/// One declaration's `outputs` field.
struct Outputs<'outputs> {
    source: &'outputs SourceAddress,
    descriptors: &'outputs Json,
    expected: DescriptorOwner<'outputs>,
}

/// The type of an output as its declaration states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputKind {
    String,
    Number,
    Bool,
    List,
    Object,
}

impl OutputKind {
    fn parse(descriptor: &Map<String, Json>) -> Result<Self> {
        match descriptor.get("type").and_then(Json::as_str) {
            Some("string") => Ok(Self::String),
            Some("number") => Ok(Self::Number),
            Some("bool") => Ok(Self::Bool),
            Some("list") => Ok(Self::List),
            Some("object") => Ok(Self::Object),
            _ => Err(InfrastructureError::configuration(
                "an output descriptor's `type` must be string, number, bool, list or object",
            )),
        }
    }

    const fn scalar(self) -> Option<InfrastructureOutputType> {
        match self {
            Self::String => Some(InfrastructureOutputType::String),
            Self::Number => Some(InfrastructureOutputType::Number),
            Self::Bool => Some(InfrastructureOutputType::Boolean),
            Self::List | Self::Object => None,
        }
    }
}

/// Every output the selected configuration declares.
#[derive(Debug, Default)]
struct Declarations {
    /// Scalar outputs, which the engine checks against provider schemas.
    identities: BTreeMap<OutputIdentity, InfrastructureOutputType>,
    /// Every declared top-level output, scalar or not.
    kinds: BTreeMap<(SourceAddress, String), OutputKind>,
}

impl Declarations {
    fn record(&mut self, outputs: &Outputs<'_>) -> Result<()> {
        let Some(descriptors) = outputs.descriptors.as_object() else {
            return Err(InfrastructureError::configuration(format!(
                "{}: `outputs` must be an object",
                outputs.source
            )));
        };
        for (attribute, descriptor) in descriptors {
            let descriptor = descriptor
                .as_object()
                .filter(|descriptor| {
                    descriptor.contains_key(OUTPUT_KEY) && !descriptor.contains_key(FUNCTION_KEY)
                })
                .ok_or_else(|| {
                    InfrastructureError::configuration(format!(
                        "{}: output {attribute} is not an output reference: declare it with its type, \
                         such as `type: \"string\"`, in a configuration unified with \
                         `schema.#Infrastructure`, and type a resource declared inside a definition \
                         with `schema.#ManagedResource` or a generated `#Resource`",
                        outputs.source
                    ))
                })?;
            let names_itself = OutputDescriptor::parse(descriptor).is_ok_and(|parsed| {
                parsed.namespace == outputs.expected.namespace
                    && parsed.name == outputs.expected.name
                    && parsed.environment == outputs.expected.environment
                    && parsed.path == [Json::String(attribute.clone())]
            });
            if !names_itself {
                return Err(InfrastructureError::configuration(format!(
                    "{}: output {attribute} must be the schema's own descriptor of that output",
                    outputs.source
                )));
            }
            let kind = OutputKind::parse(descriptor)?;
            if let Some(scalar) = kind.scalar() {
                self.identities.insert(
                    OutputIdentity {
                        source: outputs.source.clone(),
                        attribute: OutputPath::attribute(attribute.clone()),
                    },
                    scalar,
                );
            }
            self.kinds
                .insert((outputs.source.clone(), attribute.clone()), kind);
        }
        Ok(())
    }

    fn kind(&self, source: &SourceAddress, attribute: &str) -> Option<OutputKind> {
        self.kinds
            .get(&(source.clone(), attribute.to_string()))
            .copied()
    }

    fn scalar_types(&self) -> BTreeMap<SourceAddress, BTreeMap<String, InfrastructureOutputType>> {
        let mut types: BTreeMap<SourceAddress, BTreeMap<String, InfrastructureOutputType>> =
            BTreeMap::new();
        for (identity, output_type) in &self.identities {
            types
                .entry(identity.source.clone())
                .or_default()
                .insert(identity.attribute.name().to_string(), *output_type);
        }
        types
    }
}

/// The `cuenvOutput` object of a descriptor.
struct OutputDescriptor<'descriptor> {
    namespace: &'descriptor str,
    name: &'descriptor str,
    environment: &'descriptor str,
    path: &'descriptor [Json],
}

impl<'descriptor> OutputDescriptor<'descriptor> {
    fn parse(descriptor: &'descriptor Map<String, Json>) -> Result<Self> {
        let invalid = || InfrastructureError::configuration("an output descriptor is malformed");
        let output = descriptor
            .get(OUTPUT_KEY)
            .and_then(Json::as_object)
            .ok_or_else(invalid)?;
        let text = |field: &str| output.get(field).and_then(Json::as_str).ok_or_else(invalid);
        let parsed = Self {
            namespace: text("namespace")?,
            name: text("name")?,
            environment: text("environment")?,
            path: output
                .get("path")
                .and_then(Json::as_array)
                .map(Vec::as_slice)
                .ok_or_else(invalid)?,
        };
        if !matches!(parsed.namespace, "resources" | "data") || parsed.path.is_empty() {
            return Err(invalid());
        }
        Ok(parsed)
    }
}

/// Resolves descriptors against the declarations of the selected configuration.
struct Resolver<'resolver> {
    sources: &'resolver Sources,
    declarations: &'resolver Declarations,
    selection: Selection<'resolver>,
}

impl Resolver<'_> {
    /// The output a reference reads, and the kind it reads it as.
    fn output(
        &self,
        descriptor: &Map<String, Json>,
        selector: &[Json],
    ) -> Result<(OutputIdentity, OutputKind)> {
        let parsed = OutputDescriptor::parse(descriptor)?;
        if parsed.environment != self.selection.environment_label() {
            return Err(InfrastructureError::configuration(format!(
                "an output reference reads {}.{} of another environment; every environment is a complete configuration and reads only its own outputs",
                parsed.namespace, parsed.name
            )));
        }
        let source = self.sources.address(parsed.namespace, parsed.name)?.clone();
        let mut steps = parsed.path.iter().chain(selector);
        let Some(Json::String(attribute)) = steps.next() else {
            return Err(InfrastructureError::configuration(
                "an output descriptor's path must start with the output's name",
            ));
        };
        let declared = self.declarations.kind(&source, attribute).ok_or_else(|| {
            InfrastructureError::configuration(format!(
                "{source}: output {attribute} is read but not declared in `outputs`"
            ))
        })?;
        let selector = steps.map(path_step).collect::<Result<Vec<_>>>()?;
        let kind = OutputKind::parse(descriptor)?;
        if kind != declared {
            return Err(InfrastructureError::configuration(format!(
                "{source}: output {attribute} is read with a type other than the one it is declared with"
            )));
        }
        let attribute = if selector.is_empty() {
            OutputPath::attribute(attribute.clone())
        } else {
            OutputPath::nested(attribute.clone(), selector)?
        };
        Ok((OutputIdentity { source, attribute }, kind))
    }

    /// Parse one descriptor into the expression it stands for.
    fn expression(&self, descriptor: &Map<String, Json>) -> Result<Expression> {
        if descriptor.contains_key(OUTPUT_KEY) {
            let (identity, kind) = self.output(descriptor, &[])?;
            return Ok(Expression::Output(identity, scalar_kind(kind)?));
        }
        let function = descriptor
            .get(FUNCTION_KEY)
            .and_then(Json::as_str)
            .ok_or_else(|| {
                InfrastructureError::configuration("a function descriptor is malformed")
            })?;
        let field = |name: &str| {
            descriptor.get(name).ok_or_else(|| {
                InfrastructureError::configuration(format!(
                    "a {function} function descriptor has no `{name}`"
                ))
            })
        };
        let text = |name: &str| {
            field(name)?.as_str().ok_or_else(|| {
                InfrastructureError::configuration(format!(
                    "a {function} function's `{name}` must be a string"
                ))
            })
        };
        match function {
            "format" => Ok(Expression::Format(
                list(field("parts")?)?
                    .iter()
                    .map(|part| self.operand(part))
                    .collect::<Result<_>>()?,
            )),
            "sha256" => Ok(Expression::Sha256 {
                of: Box::new(self.text_operand(field("of")?)?),
                encoding: match text("encoding")? {
                    "hex" => DigestEncoding::Hex,
                    "base64" => DigestEncoding::Base64,
                    _ => {
                        return Err(InfrastructureError::configuration(
                            "a sha256 function's `encoding` must be hex or base64",
                        ));
                    }
                },
            }),
            "base64" => Ok(Expression::Base64 {
                of: Box::new(self.text_operand(field("of")?)?),
                alphabet: match text("encoding")? {
                    "standard" => Alphabet::Standard,
                    "url" => Alphabet::Url,
                    _ => {
                        return Err(InfrastructureError::configuration(
                            "a base64 function's `encoding` must be standard or url",
                        ));
                    }
                },
            }),
            "join" => Ok(Expression::Join {
                items: list(field("items")?)?
                    .iter()
                    .map(|item| self.text_operand(item))
                    .collect::<Result<_>>()?,
                separator: text("separator")?.to_string(),
            }),
            "select" => {
                let from = field("from")?
                    .as_object()
                    .filter(|from| from.contains_key(OUTPUT_KEY))
                    .ok_or_else(|| {
                        InfrastructureError::configuration(
                            "a select function reads from an output reference",
                        )
                    })?;
                if !matches!(
                    OutputKind::parse(from)?,
                    OutputKind::List | OutputKind::Object
                ) {
                    return Err(InfrastructureError::configuration(
                        "a select function reads from a list or object output",
                    ));
                }
                let (identity, _) = self.output(from, list(field("path")?)?)?;
                Ok(Expression::Output(
                    identity,
                    scalar_kind(OutputKind::parse(descriptor)?)?,
                ))
            }
            _ => Err(InfrastructureError::configuration(format!(
                "`{function}` is not an output function: use #Format, #Sha256, #Base64, #Join or #Select"
            ))),
        }
    }

    fn operand(&self, value: &Json) -> Result<Operand> {
        match value {
            Json::String(text) => Ok(Operand::Literal(Literal::String(text.clone()))),
            Json::Number(number) => Ok(Operand::Literal(Literal::Number(number.to_string()))),
            Json::Bool(flag) => Ok(Operand::Literal(Literal::Bool(*flag))),
            Json::Object(descriptor) if is_descriptor(descriptor) => {
                Ok(Operand::Expression(self.expression(descriptor)?))
            }
            _ => Err(InfrastructureError::configuration(
                "a function operand must be a string, number, bool or output expression",
            )),
        }
    }

    fn text_operand(&self, value: &Json) -> Result<Operand> {
        let operand = self.operand(value)?;
        if operand.is_text() {
            Ok(operand)
        } else {
            Err(InfrastructureError::configuration(
                "this function operand must be a string or a string expression",
            ))
        }
    }
}

fn list(value: &Json) -> Result<&[Json]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| InfrastructureError::configuration("a function operand must be a list"))
}

fn path_step(step: &Json) -> Result<PathStep> {
    match step {
        Json::String(name) => Ok(PathStep::Attribute(name.clone())),
        Json::Number(index) => index.as_i64().map(PathStep::Index).ok_or_else(|| {
            InfrastructureError::configuration("an output path index must be an integer")
        }),
        _ => Err(InfrastructureError::configuration(
            "an output path holds attribute names and list indexes",
        )),
    }
}

fn scalar_kind(kind: OutputKind) -> Result<OutputKind> {
    if kind.scalar().is_some() {
        Ok(kind)
    } else {
        Err(InfrastructureError::configuration(
            "a list or object output cannot be passed whole: read a scalar from it with #Select",
        ))
    }
}

fn is_descriptor(object: &Map<String, Json>) -> bool {
    object.contains_key(OUTPUT_KEY) || object.contains_key(FUNCTION_KEY)
}

/// A walk over one consumer's configuration.
struct Walk<'walk> {
    consumer: &'walk SourceAddress,
    resolver: &'walk Resolver<'walk>,
    placements: &'walk mut Vec<Placement>,
}

impl Walk<'_> {
    fn visit(&mut self, value: &mut Json, destination: Vec<ConfigurationSelector>) -> Result<()> {
        match value {
            Json::Object(object) if is_descriptor(object) => {
                let expression = self.resolver.expression(object)?;
                *value = Json::Null;
                self.placements.push(Placement {
                    consumer: self.consumer.clone(),
                    destination,
                    expression,
                });
                Ok(())
            }
            Json::Object(object) => object.iter_mut().try_for_each(|(field, child)| {
                let mut path = destination.clone();
                path.push(ConfigurationSelector::Field(field.clone()));
                self.visit(child, path)
            }),
            Json::Array(items) => items
                .iter_mut()
                .zip(0_i64..)
                .try_for_each(|(child, index)| {
                    let mut path = destination.clone();
                    path.push(ConfigurationSelector::Index(index));
                    self.visit(child, path)
                }),
            Json::Null | Json::Bool(_) | Json::Number(_) | Json::String(_) => Ok(()),
        }
    }
}

/// Refuse a descriptor left anywhere in the configuration, and any string that
/// carries one: [`lower`] has already taken out every descriptor it supports.
fn refuse_descriptors(value: &Json) -> Result<()> {
    match value {
        Json::Object(object) if is_descriptor(object) => Err(InfrastructureError::configuration(
            "an output reference or function may only set a resource or data source `configuration` argument",
        )),
        Json::Object(object) => object.values().try_for_each(refuse_descriptors),
        Json::Array(items) => items.iter().try_for_each(refuse_descriptors),
        Json::String(text) if text.contains(OUTPUT_KEY) || text.contains(FUNCTION_KEY) => {
            Err(InfrastructureError::configuration(
                "a string carries an encoded output reference: outputs are only known at plan time, so pass the reference itself or build the string with #Format",
            ))
        }
        Json::Null | Json::Bool(_) | Json::Number(_) | Json::String(_) => Ok(()),
    }
}

/// How a sha256 digest is written.
#[derive(Debug, Clone, Copy)]
enum DigestEncoding {
    Hex,
    Base64,
}

/// Which base64 alphabet, always padded.
#[derive(Debug, Clone, Copy)]
enum Alphabet {
    Standard,
    Url,
}

/// A literal function operand, kept as its exported text.
#[derive(Debug, Clone)]
enum Literal {
    String(String),
    Number(String),
    Bool(bool),
}

#[derive(Debug, Clone)]
enum Operand {
    Literal(Literal),
    Expression(Expression),
}

impl Operand {
    const fn is_text(&self) -> bool {
        match self {
            Self::Literal(Literal::String(_)) => true,
            Self::Literal(Literal::Number(_) | Literal::Bool(_)) => false,
            Self::Expression(expression) => expression.is_text(),
        }
    }

    fn sources(&self) -> Vec<&OutputIdentity> {
        match self {
            Self::Literal(_) => Vec::new(),
            Self::Expression(expression) => expression.sources(),
        }
    }

    fn render(&self, values: &Values<'_>) -> Result<Option<Rendered>> {
        match self {
            Self::Literal(Literal::String(text) | Literal::Number(text)) => {
                Ok(Some(Rendered::plain(text.clone())))
            }
            Self::Literal(Literal::Bool(flag)) => Ok(Some(Rendered::plain(flag.to_string()))),
            Self::Expression(expression) => expression.render(values),
        }
    }
}

/// An output reference, or a function of output references and literals.
#[derive(Debug, Clone)]
enum Expression {
    /// One scalar output, or a scalar inside a list or object output.
    Output(OutputIdentity, OutputKind),
    Format(Vec<Operand>),
    Sha256 {
        of: Box<Operand>,
        encoding: DigestEncoding,
    },
    Base64 {
        of: Box<Operand>,
        alphabet: Alphabet,
    },
    Join {
        items: Vec<Operand>,
        separator: String,
    },
}

impl Expression {
    const fn is_text(&self) -> bool {
        match self {
            Self::Output(_, kind) => matches!(kind, OutputKind::String),
            Self::Format(_) | Self::Sha256 { .. } | Self::Base64 { .. } | Self::Join { .. } => true,
        }
    }

    fn sources(&self) -> Vec<&OutputIdentity> {
        match self {
            Self::Output(identity, _) => vec![identity],
            Self::Format(operands)
            | Self::Join {
                items: operands, ..
            } => operands.iter().flat_map(Operand::sources).collect(),
            Self::Sha256 { of, .. } | Self::Base64 { of, .. } => of.sources(),
        }
    }

    /// The text of a computed value, or `None` while a source is unknown.
    fn render(&self, values: &Values<'_>) -> Result<Option<Rendered>> {
        let rendered = match self {
            Self::Output(identity, _) => values
                .scalar(identity)?
                .map(|scalar| Rendered::from_scalar(&scalar)),
            Self::Format(parts) => {
                Rendered::concatenate(parts.iter().map(|part| part.render(values)), "")?
            }
            Self::Join { items, separator } => {
                Rendered::concatenate(items.iter().map(|item| item.render(values)), separator)?
            }
            Self::Sha256 { of, encoding } => of.render(values)?.map(|input| {
                let digest = Sha256::digest(input.text.as_bytes());
                input.derive(match encoding {
                    DigestEncoding::Hex => hex::encode(digest),
                    DigestEncoding::Base64 => {
                        base64::engine::general_purpose::STANDARD.encode(digest)
                    }
                })
            }),
            Self::Base64 { of, alphabet } => of.render(values)?.map(|input| {
                let encoded = match alphabet {
                    Alphabet::Standard => {
                        base64::engine::general_purpose::STANDARD.encode(input.text.as_bytes())
                    }
                    Alphabet::Url => {
                        base64::engine::general_purpose::URL_SAFE.encode(input.text.as_bytes())
                    }
                };
                input.derive(encoded)
            }),
        };
        Ok(rendered)
    }
}

/// Computed text, and whether a secret went into it.
struct Rendered {
    text: String,
    secret: Secrecy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Secrecy {
    Public,
    Secret,
}

impl Rendered {
    const fn plain(text: String) -> Self {
        Self {
            text,
            secret: Secrecy::Public,
        }
    }

    fn from_scalar(scalar: &Scalar) -> Self {
        let text = match scalar {
            Scalar::String(text) => text.clone(),
            Scalar::Number(number) => number.as_str().to_string(),
            Scalar::Bool(flag) => flag.to_string(),
        };
        let secret = if matches!(scalar, Scalar::String(text) if is_registered_secret(text)) {
            Secrecy::Secret
        } else {
            Secrecy::Public
        };
        Self { text, secret }
    }

    const fn derive(&self, text: String) -> Self {
        Self {
            text,
            secret: self.secret,
        }
    }

    fn concatenate(
        parts: impl Iterator<Item = Result<Option<Self>>>,
        separator: &str,
    ) -> Result<Option<Self>> {
        let mut texts = Vec::new();
        let mut secret = Secrecy::Public;
        for part in parts {
            let Some(part) = part? else {
                return Ok(None);
            };
            if part.secret == Secrecy::Secret {
                secret = Secrecy::Secret;
            }
            texts.push(part.text);
        }
        Ok(Some(Self {
            text: texts.join(separator),
            secret,
        }))
    }
}

/// Whether the text is, or contains, a value registered for redaction.
fn is_registered_secret(text: &str) -> bool {
    matches!(
        cuenv_events::redaction::redact_cow(text),
        std::borrow::Cow::Owned(_)
    )
}

/// A scalar output value.
enum Scalar {
    String(String),
    Number(Number),
    Bool(bool),
}

impl Scalar {
    /// A number JSON cannot hold exactly is carried as its decimal text,
    /// which a number destination decodes exactly.
    fn json(&self) -> Json {
        match self {
            Self::String(text) => Json::String(text.clone()),
            Self::Number(number) => number
                .as_json_number()
                .map_or_else(|| Json::String(number.as_str().to_string()), Json::Number),
            Self::Bool(flag) => Json::Bool(*flag),
        }
    }
}

/// The source values one specialization supplies.
struct Values<'values> {
    sources: &'values BTreeMap<OutputIdentity, Value>,
}

impl Values<'_> {
    fn scalar(&self, identity: &OutputIdentity) -> Result<Option<Scalar>> {
        let Some(value) = self.sources.get(identity) else {
            return Ok(None);
        };
        scalar(value).map(Some).ok_or_else(|| {
            InfrastructureError::configuration(format!(
                "{}: output {} is not a string, number or bool",
                identity.source, identity.attribute
            ))
        })
    }
}

fn scalar(value: &Value) -> Option<Scalar> {
    match value {
        Value::String(text) => Some(Scalar::String(text.clone())),
        Value::Number(number) => Some(Scalar::Number(number.clone())),
        Value::Boolean(flag) => Some(Scalar::Bool(*flag)),
        Value::Typed(typed) => scalar(&typed.value),
        Value::Null | Value::Unknown | Value::List(_) | Value::Object(_) => None,
    }
}

/// Where one expression sets a configuration argument.
#[derive(Debug, Clone)]
struct Placement {
    consumer: SourceAddress,
    destination: Vec<ConfigurationSelector>,
    expression: Expression,
}

impl Placement {
    /// The engine-facing references: a plain output is carried as it is,
    /// and a function reads each of its sources once.
    fn references(&self) -> Vec<NativeReference> {
        let reference = |identity: &OutputIdentity| NativeReference {
            consumer: self.consumer.clone(),
            destination: self.destination.clone(),
            source: identity.source.clone(),
            attribute: identity.attribute.clone(),
            computed: !matches!(self.expression, Expression::Output(..)),
        };
        self.expression
            .sources()
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(reference)
            .collect()
    }

    /// The value for the destination, or `None` while a source is unknown.
    fn value(&self, values: &Values<'_>) -> Result<Option<Json>> {
        if let Expression::Output(identity, _) = &self.expression {
            return Ok(values.scalar(identity)?.map(|scalar| scalar.json()));
        }
        Ok(self.expression.render(values)?.map(|rendered| {
            if rendered.secret == Secrecy::Secret {
                cuenv_events::register_secret(rendered.text.clone());
            }
            Json::String(rendered.text)
        }))
    }
}

/// Puts output values, and the functions computed from them, into the
/// configurations [`lower`] took the descriptors out of.
#[derive(Debug)]
pub struct ExpressionEvaluator {
    references: Vec<NativeReference>,
    declared: Vec<OutputIdentity>,
    declared_types: BTreeMap<OutputIdentity, InfrastructureOutputType>,
    placements: Vec<Placement>,
    /// Each consumer's configuration with `null` where each descriptor was.
    bases: BTreeMap<SourceAddress, Map<String, Json>>,
}

impl ConfigurationEvaluator for ExpressionEvaluator {
    fn references(&self) -> &[NativeReference] {
        &self.references
    }

    fn declared_outputs(&self) -> &[OutputIdentity] {
        &self.declared
    }

    fn declared_output_type(&self, identity: &OutputIdentity) -> Option<InfrastructureOutputType> {
        self.declared_types.get(identity).copied()
    }

    /// Every reference is explicit data, so no value can change a
    /// configuration through a path the references do not name.
    fn verifies_outputs(&self) -> bool {
        false
    }

    fn verify(&self, _input: &ConfigurationVerification<'_>) -> Result<()> {
        Ok(())
    }

    fn specialize(
        &self,
        input: &ConfigurationSpecialization<'_>,
    ) -> Result<SpecializedConfiguration> {
        let mut configuration = self.bases.get(input.consumer).cloned().unwrap_or_default();
        if let Some(foreign) = input
            .deferred
            .iter()
            .find(|deferred| !self.references.contains(deferred))
        {
            return Err(InfrastructureError::configuration(format!(
                "{}: a deferred output is not one of its references",
                foreign.consumer
            )));
        }
        let values = Values {
            sources: input.sources,
        };
        for placement in self
            .placements
            .iter()
            .filter(|placement| &placement.consumer == input.consumer)
        {
            if let Some(value) = placement.value(&values)? {
                set(&mut configuration, &placement.destination, value)?;
            }
        }
        Ok(SpecializedConfiguration {
            configuration,
            unresolved: input.deferred.to_vec(),
        })
    }
}

/// Set the value at a destination that [`lower`] left as `null`.
fn set(
    configuration: &mut Map<String, Json>,
    destination: &[ConfigurationSelector],
    value: Json,
) -> Result<()> {
    let missing = || InfrastructureError::configuration("an output destination is missing");
    let Some((ConfigurationSelector::Field(first), rest)) = destination.split_first() else {
        return Err(missing());
    };
    let mut slot = configuration.get_mut(first).ok_or_else(missing)?;
    for selector in rest {
        slot = match selector {
            ConfigurationSelector::Field(field) => slot.get_mut(field.as_str()),
            ConfigurationSelector::Index(index) => usize::try_from(*index)
                .ok()
                .and_then(|index| slot.get_mut(index)),
        }
        .ok_or_else(missing)?;
    }
    *slot = value;
    Ok(())
}

#[cfg(test)]
mod tests;
