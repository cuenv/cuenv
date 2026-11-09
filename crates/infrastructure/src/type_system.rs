//! Minimal implementation of Terraform's `cty` type system.
//!
//! Terraform providers exchange values as MessagePack (and state as JSON)
//! using a *type-directed* encoding: the same bytes mean different things
//! depending on the schema type they are decoded against. This module
//! implements exactly enough of `github.com/zclconf/go-cty` to:
//!
//! - parse the JSON type specifications found in provider schemas,
//! - convert CUE-evaluated JSON configuration into typed values,
//! - encode/decode values to and from cty MessagePack, including unknown
//!   values and `DynamicPseudoType` wrappers, and
//! - render values as cty JSON for durable state storage.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Write as _};
use std::str::FromStr;

use serde::Deserialize;
use serde::de::{MapAccess, Visitor};
use serde_json::Number as JsonNumber;
use serde_json::value::RawValue;

use crate::error::{InfrastructureError, Result};
use crate::references::ConfigurationSelector;

/// MessagePack extension code cty uses for unknown values.
const UNKNOWN_EXTENSION: i8 = 0;

/// An exact decimal local to the Terraform codec, independent of global JSON settings.
#[derive(Clone)]
pub struct Number {
    text: String,
}

impl fmt::Debug for Number {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Number(<redacted>)")
    }
}

impl FromStr for Number {
    type Err = InfrastructureError;
    fn from_str(text: &str) -> Result<Self> {
        let raw: &RawValue = serde_json::from_str(text)
            .map_err(|_| InfrastructureError::codec("invalid numeric literal"))?;
        if !matches!(raw.get().as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
            return Err(InfrastructureError::codec("invalid numeric literal"));
        }
        checked_number(Self {
            text: raw.get().to_string(),
        })
    }
}

impl From<JsonNumber> for Number {
    fn from(number: JsonNumber) -> Self {
        Self {
            text: number.to_string(),
        }
    }
}

macro_rules! number_from_integer {
    ($($integer:ty),+ $(,)?) => { $(impl From<$integer> for Number {
        fn from(value: $integer) -> Self { Self { text: value.to_string() } }
    })+ };
}

number_from_integer!(
    i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize
);

impl PartialEq for Number {
    fn eq(&self, other: &Self) -> bool {
        numbers_equal(self, other)
    }
}
impl Eq for Number {}

impl fmt::Display for Number {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

/// An exact JSON numeric token and its path before default JSON conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonNumberToken {
    /// Location relative to the scanned JSON root.
    pub path: Vec<ConfigurationSelector>,
    /// Exact numeric literal.
    pub number: Number,
}

/// Inspect numeric tokens without changing ordinary object keys or strings.
///
/// # Errors
/// Returns a value-free error for malformed JSON or invalid numeric literals.
pub fn scan_json_numbers(raw: &RawValue) -> Result<Vec<JsonNumberToken>> {
    let mut tokens = Vec::new();
    walk_json_numbers(raw, &mut Vec::new(), &mut tokens)?;
    Ok(tokens)
}

/// Parse JSON losslessly. Numbers at proven numeric-reference paths become
/// temporary numeric strings. All other numbers must survive default JSON
/// conversion without changing their decimal value.
///
/// # Errors
/// Returns a value-free error when an ordinary numeric token would round.
pub fn parse_json_preserving_numbers(
    raw: &RawValue,
    numeric_paths: &[Vec<ConfigurationSelector>],
) -> Result<serde_json::Value> {
    parse_raw_json(
        raw,
        &mut Vec::new(),
        &numeric_paths.iter().cloned().collect(),
    )
}

/// Deserialize an open-ended JSON value without rounding numbers or treating
/// serde_json's private map keys as scalar values.
pub(crate) fn deserialize_json_value<'de, D>(
    deserializer: D,
) -> std::result::Result<serde_json::Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Box::<RawValue>::deserialize(deserializer)?;
    parse_json_preserving_numbers(&raw, &[]).map_err(serde::de::Error::custom)
}

/// Deserialize an optional open-ended JSON value with the same guarantees.
pub(crate) fn deserialize_optional_json_value<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Box<RawValue>>::deserialize(deserializer)?
        .map(|raw| parse_json_preserving_numbers(&raw, &[]).map_err(serde::de::Error::custom))
        .transpose()
}

fn walk_json_numbers(
    raw: &RawValue,
    path: &mut Vec<ConfigurationSelector>,
    tokens: &mut Vec<JsonNumberToken>,
) -> Result<()> {
    if path.len() >= 128 {
        return Err(InfrastructureError::codec(
            "JSON nesting exceeds the supported depth",
        ));
    }
    match raw.get().as_bytes().first() {
        Some(b'{') => {
            let fields = raw_object(raw)?;
            for (name, value) in fields {
                path.push(ConfigurationSelector::Field(name));
                walk_json_numbers(&value, path, tokens)?;
                path.pop();
            }
        }
        Some(b'[') => {
            let elements: Vec<Box<RawValue>> =
                serde_json::from_str(raw.get()).map_err(|_| json_number_error())?;
            for (index, value) in elements.iter().enumerate() {
                path.push(ConfigurationSelector::Index(
                    i64::try_from(index).map_err(|_| json_number_error())?,
                ));
                walk_json_numbers(value, path, tokens)?;
                path.pop();
            }
        }
        Some(b'-' | b'0'..=b'9') => tokens.push(JsonNumberToken {
            path: path.clone(),
            number: raw.get().parse()?,
        }),
        _ => {}
    }
    Ok(())
}

fn json_number_error() -> InfrastructureError {
    InfrastructureError::codec("JSON numeric data could not be decoded losslessly")
}

/// Decode object members without losing duplicate keys before validation.
fn raw_object(raw: &RawValue) -> Result<BTreeMap<String, Box<RawValue>>> {
    struct Fields(BTreeMap<String, Box<RawValue>>);

    impl<'de> Deserialize<'de> for Fields {
        fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            struct FieldsVisitor;

            impl<'de> Visitor<'de> for FieldsVisitor {
                type Value = Fields;

                fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str("a JSON object with unique keys")
                }

                fn visit_map<M>(self, mut map: M) -> std::result::Result<Fields, M::Error>
                where
                    M: MapAccess<'de>,
                {
                    let mut fields = BTreeMap::new();
                    while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                        if fields.insert(key, value).is_some() {
                            return Err(serde::de::Error::custom("duplicate JSON object key"));
                        }
                    }
                    Ok(Fields(fields))
                }
            }

            deserializer.deserialize_map(FieldsVisitor)
        }
    }

    serde_json::from_str::<Fields>(raw.get())
        .map(|fields| fields.0)
        .map_err(|_| json_number_error())
}

fn parse_raw_json(
    raw: &RawValue,
    path: &mut Vec<ConfigurationSelector>,
    preserved: &BTreeSet<Vec<ConfigurationSelector>>,
) -> Result<serde_json::Value> {
    use serde_json::Value as Json;
    if path.len() >= 128 {
        return Err(InfrastructureError::codec(
            "JSON nesting exceeds the supported depth",
        ));
    }
    match raw.get().as_bytes().first() {
        Some(b'{') => {
            let fields = raw_object(raw)?;
            let mut object = serde_json::Map::new();
            for (name, raw) in fields {
                path.push(ConfigurationSelector::Field(name.clone()));
                let value = parse_raw_json(&raw, path, preserved)?;
                path.pop();
                object.insert(name, value);
            }
            Ok(Json::Object(object))
        }
        Some(b'[') => {
            let elements: Vec<Box<RawValue>> =
                serde_json::from_str(raw.get()).map_err(|_| json_number_error())?;
            let mut array = Vec::with_capacity(elements.len());
            for (index, raw) in elements.iter().enumerate() {
                path.push(ConfigurationSelector::Index(
                    i64::try_from(index).map_err(|_| json_number_error())?,
                ));
                array.push(parse_raw_json(raw, path, preserved)?);
                path.pop();
            }
            Ok(Json::Array(array))
        }
        Some(b'-' | b'0'..=b'9') => {
            let number: Number = raw.get().parse()?;
            if preserved.contains(path) {
                return Ok(Json::String(number.text));
            }
            number
                .as_json_number()
                .map(Json::Number)
                .ok_or_else(json_number_error)
        }
        _ => serde_json::from_str(raw.get()).map_err(|_| json_number_error()),
    }
}

impl Number {
    /// Exact numeric literal, suitable for a CUE scalar fill.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }
    /// An integral spelling that fits a signed 64-bit integer.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        self.text.parse().ok()
    }
    /// An integral spelling that fits an unsigned 64-bit integer.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        self.text.parse().ok()
    }
    /// A floating-point approximation. Encoding independently verifies exactness.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        self.text.parse().ok()
    }
    /// Preserve the exact binary value of a finite provider float.
    #[must_use]
    pub fn from_f64(value: f64) -> Option<Self> {
        exact_float_number(value).ok()
    }
    /// A default JSON number only when its decimal spelling is lossless.
    #[must_use]
    pub fn as_json_number(&self) -> Option<JsonNumber> {
        let candidate = self.text.parse::<JsonNumber>().ok()?;
        numbers_equal(self, &Self::from(candidate.clone())).then_some(candidate)
    }
    fn json_value(&self) -> serde_json::Value {
        self.as_json_number().map_or_else(
            || serde_json::Value::String(self.text.clone()),
            serde_json::Value::Number,
        )
    }
}

/// A cty type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// `bool`
    Boolean,
    /// `number`
    Number,
    /// `string`
    String,
    /// `dynamic` — the concrete type travels alongside the value.
    Dynamic,
    /// `list(T)`
    List(Box<Self>),
    /// `set(T)`
    Set(Box<Self>),
    /// `map(T)`
    Map(Box<Self>),
    /// `object({...})`
    Object(BTreeMap<String, Self>),
    /// `tuple([...])`
    Tuple(Vec<Self>),
}

impl Type {
    /// Parse a cty JSON type specification such as `"string"` or
    /// `["list", ["object", {"a": "number"}]]`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Codec`] when the specification is malformed.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self> {
        let json: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
            InfrastructureError::codec(format!("invalid cty type JSON: {error}"))
        })?;
        Self::from_json(&json)
    }

    /// Parse a cty JSON type specification from an already-decoded value.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Codec`] when the specification is malformed.
    pub fn from_json(json: &serde_json::Value) -> Result<Self> {
        use serde_json::Value as Json;
        match json {
            Json::String(primitive) => match primitive.as_str() {
                "bool" => Ok(Self::Boolean),
                "number" => Ok(Self::Number),
                "string" => Ok(Self::String),
                "dynamic" => Ok(Self::Dynamic),
                _ => Err(InfrastructureError::codec("unknown cty primitive type")),
            },
            Json::Array(parts) => {
                let kind = parts.first().and_then(Json::as_str).ok_or_else(|| {
                    InfrastructureError::codec("cty type array must start with a kind")
                })?;
                let argument = parts.get(1).ok_or_else(|| {
                    InfrastructureError::codec("cty type is missing its argument")
                })?;
                match kind {
                    "list" => Ok(Self::List(Box::new(Self::from_json(argument)?))),
                    "set" => Ok(Self::Set(Box::new(Self::from_json(argument)?))),
                    "map" => Ok(Self::Map(Box::new(Self::from_json(argument)?))),
                    "object" => {
                        let attributes = argument.as_object().ok_or_else(|| {
                            InfrastructureError::codec(
                                "cty object type attributes must be a JSON object",
                            )
                        })?;
                        let attributes = attributes
                            .iter()
                            .map(|(name, specification)| {
                                Ok((name.clone(), Self::from_json(specification)?))
                            })
                            .collect::<Result<_>>()?;
                        Ok(Self::Object(attributes))
                    }
                    "tuple" => {
                        let element_types = argument.as_array().ok_or_else(|| {
                            InfrastructureError::codec(
                                "cty tuple type elements must be a JSON array",
                            )
                        })?;
                        Ok(Self::Tuple(
                            element_types
                                .iter()
                                .map(Self::from_json)
                                .collect::<Result<_>>()?,
                        ))
                    }
                    _ => Err(InfrastructureError::codec("unknown cty type kind")),
                }
            }
            _ => Err(InfrastructureError::codec(
                "unsupported cty type specification",
            )),
        }
    }

    /// A value-free type label for diagnostics, including dynamic types whose
    /// object field names may originate in provider data.
    const fn diagnostic_kind(&self) -> &'static str {
        match self {
            Self::Boolean => "bool",
            Self::Number => "number",
            Self::String => "string",
            Self::Dynamic => "dynamic",
            Self::List(_) => "list",
            Self::Set(_) => "set",
            Self::Map(_) => "map",
            Self::Object(_) => "object",
            Self::Tuple(_) => "tuple",
        }
    }

    /// Render this type as a cty JSON type specification.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::{Value as Json, json};
        match self {
            Self::Boolean => Json::from("bool"),
            Self::Number => Json::from("number"),
            Self::String => Json::from("string"),
            Self::Dynamic => Json::from("dynamic"),
            Self::List(element_type) => json!(["list", element_type.to_json()]),
            Self::Set(element_type) => json!(["set", element_type.to_json()]),
            Self::Map(element_type) => json!(["map", element_type.to_json()]),
            Self::Object(attributes) => {
                let attributes: serde_json::Map<String, Json> = attributes
                    .iter()
                    .map(|(name, attribute_type)| (name.clone(), attribute_type.to_json()))
                    .collect();
                json!(["object", attributes])
            }
            Self::Tuple(element_types) => {
                json!([
                    "tuple",
                    element_types.iter().map(Self::to_json).collect::<Vec<_>>()
                ])
            }
        }
    }
}

/// A cty value. Collections are untyped here; the [`Type`] they are
/// encoded against supplies the distinction between list, set and tuple,
/// and between map and object.
///
/// The one exception is a value held in a `dynamic` slot: a provider sends
/// it with its concrete type, and that type must travel back unchanged (a
/// `list(string)` must not come back as a `tuple`). Such values are
/// [`Value::Typed`]. Values from CUE configuration carry no type; theirs is
/// inferred when they are encoded.
#[derive(Debug, Clone)]
pub enum Value {
    /// A null value of any type.
    Null,
    /// A value that will only be known after apply.
    Unknown,
    /// A boolean.
    Boolean(bool),
    /// A number.
    Number(Number),
    /// A string.
    String(String),
    /// A list, set or tuple.
    List(Vec<Self>),
    /// A map or object.
    Object(BTreeMap<String, Self>),
    /// A value of a `dynamic` slot with the concrete type it arrived with.
    Typed(Box<TypedValue>),
}

/// Structural equality keeps collection order and typed wrappers significant,
/// while number leaves compare by exact value rather than JSON spelling.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) | (Self::Unknown, Self::Unknown) => true,
            (Self::Boolean(left), Self::Boolean(right)) => left == right,
            (Self::Number(left), Self::Number(right)) => numbers_equal(left, right),
            (Self::String(left), Self::String(right)) => left == right,
            (Self::List(left), Self::List(right)) => left == right,
            (Self::Object(left), Self::Object(right)) => left == right,
            (Self::Typed(left), Self::Typed(right)) => left == right,
            _ => false,
        }
    }
}

/// A value together with its concrete cty type; see [`Value::Typed`].
#[derive(Debug, Clone, PartialEq)]
pub struct TypedValue {
    /// The concrete type.
    pub value_type: Type,
    /// The value, conforming to `value_type`.
    pub value: Value,
}

impl Value {
    /// A value of a `dynamic` slot with its concrete type.
    #[must_use]
    pub fn typed(value_type: Type, value: Self) -> Self {
        Self::Typed(Box::new(TypedValue { value_type, value }))
    }

    /// This value without its [`Value::Typed`] wrapper, if any.
    #[must_use]
    pub fn untyped(&self) -> &Self {
        match self {
            Self::Typed(typed) => typed.value.untyped(),
            other => other,
        }
    }

    /// Whether this value is null.
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self.untyped(), Self::Null)
    }

    /// Whether this value (not something inside it) is unknown.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        matches!(self.untyped(), Self::Unknown)
    }

    /// Whether this value, or anything nested inside it, is unknown.
    #[must_use]
    pub fn contains_unknown(&self) -> bool {
        match self {
            Self::Unknown => true,
            Self::List(items) => items.iter().any(Self::contains_unknown),
            Self::Object(attributes) => attributes.values().any(Self::contains_unknown),
            Self::Typed(typed) => typed.value.contains_unknown(),
            _ => false,
        }
    }

    /// Replace every unknown value with null, as Terraform does before
    /// saving the result of a failed apply.
    #[must_use]
    pub fn unknown_as_null(&self) -> Self {
        match self {
            Self::Unknown => Self::Null,
            Self::List(elements) => {
                Self::List(elements.iter().map(Self::unknown_as_null).collect())
            }
            Self::Object(attributes) => Self::Object(
                attributes
                    .iter()
                    .map(|(name, value)| (name.clone(), value.unknown_as_null()))
                    .collect(),
            ),
            Self::Typed(typed) => {
                Self::typed(typed.value_type.clone(), typed.value.unknown_as_null())
            }
            other => other.clone(),
        }
    }

    /// Name this value's kind without revealing its content.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self.untyped() {
            Self::Null => "null",
            Self::Unknown => "unknown",
            Self::Boolean(_) => "boolean",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::List(_) => "list",
            Self::Object(_) | Self::Typed(_) => "object",
        }
    }

    /// Look up an attribute of an object value.
    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<&Self> {
        match self.untyped() {
            Self::Object(attributes) => attributes.get(name),
            _ => None,
        }
    }

    /// Convert CUE-evaluated JSON into a value conforming to `value_type`.
    ///
    /// Object attributes missing from the JSON become null, mirroring how
    /// Terraform decodes a configuration block with unset arguments.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] when the JSON cannot be converted.
    pub fn from_configuration_json(json: &serde_json::Value, value_type: &Type) -> Result<Self> {
        from_configuration_json(json, value_type, &mut Vec::new())
    }

    /// Render this value as cty JSON (the format Terraform stores state in).
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Codec`] if the value contains unknowns, which
    /// can never be persisted.
    pub fn to_state_json(&self, value_type: &Type) -> Result<serde_json::Value> {
        to_state_json(self, value_type)
    }

    /// Render this value as plain JSON for display. Unknown values render
    /// as the string `(known after apply)`.
    #[must_use]
    pub fn to_display_json(&self) -> serde_json::Value {
        use serde_json::Value as Json;
        match self {
            Self::Null => Json::Null,
            Self::Unknown => Json::from("(known after apply)"),
            Self::Boolean(boolean) => Json::Bool(*boolean),
            Self::Number(number) => number.json_value(),
            Self::String(text) => Json::String(text.clone()),
            Self::List(items) => Json::Array(items.iter().map(Self::to_display_json).collect()),
            Self::Object(attributes) => Json::Object(
                attributes
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_display_json()))
                    .collect(),
            ),
            Self::Typed(typed) => typed.value.to_display_json(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.untyped() {
            Self::Unknown => formatter.write_str("(known after apply)"),
            Self::Null => formatter.write_str("null"),
            Self::Number(number) => number.fmt(formatter),
            other => write!(formatter, "{}", other.to_display_json()),
        }
    }
}

/// Compare two values of type `value_type` the way cty's `Equals` does:
/// set elements are unordered, numbers compare numerically, and anything
/// unknown is never equal to anything.
///
/// Providers may return set elements in any order, so positional equality
/// would report perpetual differences.
#[must_use]
pub fn semantically_equal(left: &Value, right: &Value, value_type: &Type) -> bool {
    Equality {
        unknowns: UnknownComparison::NeverEqual,
    }
    .equal(left, right, value_type)
}

/// Compare two values of type `value_type` the way cty's `RawEquals` does:
/// like [`semantically_equal`], except that an unknown value equals another
/// unknown value in the same position.
#[must_use]
pub fn raw_equal(left: &Value, right: &Value, value_type: &Type) -> bool {
    Equality {
        unknowns: UnknownComparison::EqualToUnknown,
    }
    .equal(left, right, value_type)
}

/// `values` without the elements equal (by [`raw_equal`]) to an earlier
/// one, as a cty set of `element_type` holds them.
#[must_use]
pub fn deduplicate(values: Vec<Value>, element_type: &Type) -> Vec<Value> {
    let mut unique: Vec<Value> = Vec::with_capacity(values.len());
    for value in values {
        if !unique
            .iter()
            .any(|kept| raw_equal(kept, &value, element_type))
        {
            unique.push(value);
        }
    }
    unique
}

/// How [`Equality`] treats unknown values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnknownComparison {
    /// Unknown values are never equal to anything (cty `Equals`).
    NeverEqual,
    /// Two unknown values are equal to each other (cty `RawEquals`).
    EqualToUnknown,
}

/// The concrete type and value of a value in a `dynamic` slot: its own type
/// when it carries one, otherwise the type inferred from its shape.
fn concrete(value: &Value) -> (Type, &Value) {
    match value {
        Value::Typed(typed) => (typed.value_type.clone(), &typed.value),
        other => (infer_type(other), other),
    }
}

/// Whether two types describe the same values, treating `dynamic` (a type
/// not yet known) as matching anything.
fn types_match(left: &Type, right: &Type) -> bool {
    match (left, right) {
        (Type::Dynamic, _) | (_, Type::Dynamic) => true,
        (Type::List(left), Type::List(right))
        | (Type::Set(left), Type::Set(right))
        | (Type::Map(left), Type::Map(right)) => types_match(left, right),
        (Type::Object(left), Type::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(name, left_type)| {
                    right
                        .get(name)
                        .is_some_and(|right_type| types_match(left_type, right_type))
                })
        }
        (Type::Tuple(left), Type::Tuple(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left_type, right_type)| types_match(left_type, right_type))
        }
        (left, right) => left == right,
    }
}

/// A cty equality: `Equals` or `RawEquals`, by how unknowns compare.
#[derive(Debug, Clone, Copy)]
struct Equality {
    unknowns: UnknownComparison,
}

impl Equality {
    fn equal(self, left: &Value, right: &Value, value_type: &Type) -> bool {
        if *value_type == Type::Dynamic {
            let (left_type, left_value) = concrete(left);
            let (right_type, right_value) = concrete(right);
            // A null's type does not make it differ from another null.
            if left_value.is_null() || right_value.is_null() {
                return left_value.is_null() && right_value.is_null();
            }
            if !types_match(&left_type, &right_type) {
                return false;
            }
            let comparison_type = if left_type == Type::Dynamic {
                right_type
            } else {
                left_type
            };
            if comparison_type == Type::Dynamic {
                // Only unknowns have no concrete type.
                return self.equal_untyped(
                    left_value.untyped(),
                    right_value.untyped(),
                    &Type::Dynamic,
                );
            }
            return self.equal(left_value, right_value, &comparison_type);
        }
        self.equal_untyped(left.untyped(), right.untyped(), value_type)
    }

    fn equal_untyped(self, left: &Value, right: &Value, value_type: &Type) -> bool {
        let unknowns = self.unknowns;
        let equal =
            |left: &Value, right: &Value, value_type: &Type| self.equal(left, right, value_type);
        match (left, right, value_type) {
            (Value::Unknown, Value::Unknown, _) => unknowns == UnknownComparison::EqualToUnknown,
            (Value::Unknown, _, _) | (_, Value::Unknown, _) => false,
            (Value::List(left_elements), Value::List(right_elements), Type::Set(element_type)) => {
                left_elements.len() == right_elements.len()
                    && left_elements.iter().all(|element| {
                        let want = left_elements
                            .iter()
                            .filter(|candidate| equal(candidate, element, element_type))
                            .count();
                        let have = right_elements
                            .iter()
                            .filter(|candidate| equal(candidate, element, element_type))
                            .count();
                        want == have
                    })
            }
            (Value::List(left_elements), Value::List(right_elements), Type::List(element_type)) => {
                left_elements.len() == right_elements.len()
                    && left_elements.iter().zip(right_elements).all(
                        |(left_element, right_element)| {
                            equal(left_element, right_element, element_type)
                        },
                    )
            }
            (
                Value::List(left_elements),
                Value::List(right_elements),
                Type::Tuple(element_types),
            ) => {
                left_elements.len() == right_elements.len()
                    && left_elements
                        .iter()
                        .zip(right_elements)
                        .zip(element_types)
                        .all(|((left_element, right_element), element_type)| {
                            equal(left_element, right_element, element_type)
                        })
            }
            (
                Value::Object(left_entries),
                Value::Object(right_entries),
                Type::Map(element_type),
            ) => {
                left_entries.len() == right_entries.len()
                    && left_entries.iter().all(|(key, left_element)| {
                        right_entries.get(key).is_some_and(|right_element| {
                            equal(left_element, right_element, element_type)
                        })
                    })
            }
            (
                Value::Object(left_entries),
                Value::Object(right_entries),
                Type::Object(attribute_types),
            ) => attribute_types.iter().all(|(name, attribute_type)| {
                equal(
                    left_entries.get(name).unwrap_or(&Value::Null),
                    right_entries.get(name).unwrap_or(&Value::Null),
                    attribute_type,
                )
            }),
            (Value::Number(left_number), Value::Number(right_number), _) => {
                numbers_equal(left_number, right_number)
            }
            (left, right, _) => left == right,
        }
    }
}

fn path_string(path: &[String]) -> String {
    if path.is_empty() {
        "<root>".to_string()
    } else {
        path.join(".")
    }
}

fn from_configuration_json(
    json: &serde_json::Value,
    value_type: &Type,
    path: &mut Vec<String>,
) -> Result<Value> {
    use serde_json::Value as Json;
    if json.is_null() {
        return Ok(Value::Null);
    }
    let mismatch = |expected: &str, path: &[String]| {
        InfrastructureError::configuration(format!(
            "{}: expected {expected}, got {}",
            path_string(path),
            json_kind(json)
        ))
    };
    match value_type {
        Type::Dynamic => Ok(infer_from_json(json)),
        Type::Boolean => match json {
            Json::Bool(boolean) => Ok(Value::Boolean(*boolean)),
            Json::String(text) if text == "true" => Ok(Value::Boolean(true)),
            Json::String(text) if text == "false" => Ok(Value::Boolean(false)),
            _ => Err(mismatch("bool", path)),
        },
        Type::Number => match json {
            Json::Number(number) => checked_number(number.clone().into()).map(Value::Number),
            Json::String(text) => text
                .parse::<Number>()
                .map_err(|_| mismatch("number", path))
                .and_then(checked_number)
                .map(Value::Number),
            _ => Err(mismatch("number", path)),
        },
        Type::String => match json {
            Json::String(text) => Ok(Value::String(text.clone())),
            Json::Number(number) => Ok(Value::String(number.to_string())),
            Json::Bool(boolean) => Ok(Value::String(boolean.to_string())),
            _ => Err(mismatch("string", path)),
        },
        Type::List(element_type) | Type::Set(element_type) => {
            let items = json.as_array().ok_or_else(|| mismatch("list", path))?;
            let mut converted = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                path.push(index.to_string());
                converted.push(from_configuration_json(item, element_type, path)?);
                path.pop();
            }
            // A set holds each distinct value once, as Terraform's
            // configuration decoding does.
            if matches!(value_type, Type::Set(_)) {
                converted = deduplicate(converted, element_type);
            }
            Ok(Value::List(converted))
        }
        Type::Tuple(element_types) => {
            let items = json.as_array().ok_or_else(|| mismatch("tuple", path))?;
            if items.len() != element_types.len() {
                return Err(mismatch(
                    &format!("tuple of {} elements", element_types.len()),
                    path,
                ));
            }
            let mut converted = Vec::with_capacity(items.len());
            let indexed_items = items.iter().zip(element_types).enumerate();
            for (index, (item, element_type)) in indexed_items {
                path.push(index.to_string());
                converted.push(from_configuration_json(item, element_type, path)?);
                path.pop();
            }
            Ok(Value::List(converted))
        }
        Type::Map(element_type) => {
            let object = json.as_object().ok_or_else(|| mismatch("map", path))?;
            let mut converted = BTreeMap::new();
            for (key, element_json) in object {
                path.push("<hidden key>".into());
                converted.insert(
                    key.clone(),
                    from_configuration_json(element_json, element_type, path)?,
                );
                path.pop();
            }
            Ok(Value::Object(converted))
        }
        Type::Object(attributes) => {
            let object = json.as_object().ok_or_else(|| mismatch("object", path))?;
            if object.keys().any(|key| !attributes.contains_key(key)) {
                return Err(InfrastructureError::configuration(format!(
                    "{}: unsupported argument '<hidden field>'",
                    path_string(path)
                )));
            }
            let mut converted = BTreeMap::new();
            for (name, attribute_type) in attributes {
                let value = match object.get(name) {
                    Some(attribute_json) => {
                        path.push(name.clone());
                        let value = from_configuration_json(attribute_json, attribute_type, path)?;
                        path.pop();
                        value
                    }
                    None => Value::Null,
                };
                converted.insert(name.clone(), value);
            }
            Ok(Value::Object(converted))
        }
    }
}

fn infer_from_json(json: &serde_json::Value) -> Value {
    use serde_json::Value as Json;
    match json {
        Json::Null => Value::Null,
        Json::Bool(boolean) => Value::Boolean(*boolean),
        Json::Number(number) => Value::Number(number.clone().into()),
        Json::String(text) => Value::String(text.clone()),
        Json::Array(items) => Value::List(items.iter().map(infer_from_json).collect()),
        Json::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, element_json)| (key.clone(), infer_from_json(element_json)))
                .collect(),
        ),
    }
}

/// Infer the concrete type of a value held in a `dynamic` slot, from its
/// own type when it carries one. Only values from configuration have none.
fn infer_type(value: &Value) -> Type {
    match value {
        Value::Typed(typed) => typed.value_type.clone(),
        Value::Null | Value::Unknown => Type::Dynamic,
        Value::Boolean(_) => Type::Boolean,
        Value::Number(_) => Type::Number,
        Value::String(_) => Type::String,
        Value::List(items) => Type::Tuple(items.iter().map(infer_type).collect()),
        Value::Object(attributes) => Type::Object(
            attributes
                .iter()
                .map(|(key, element)| (key.clone(), infer_type(element)))
                .collect(),
        ),
    }
}

fn to_state_json(value: &Value, value_type: &Type) -> Result<serde_json::Value> {
    use serde_json::Value as Json;
    match (value, value_type) {
        (Value::Typed(typed), Type::Dynamic) => Ok(serde_json::json!({
            "value": to_state_json(&typed.value, &typed.value_type)?,
            "type": typed.value_type.to_json(),
        })),
        (Value::Typed(typed), _) => to_state_json(&typed.value, value_type),
        (Value::Null, _) => Ok(Json::Null),
        (Value::Unknown, _) => Err(InfrastructureError::codec(
            "cannot persist a value that is unknown after apply",
        )),
        (dynamic_value, Type::Dynamic) => {
            let concrete = infer_type(dynamic_value);
            Ok(serde_json::json!({
                "value": to_state_json(dynamic_value, &concrete)?,
                "type": concrete.to_json(),
            }))
        }
        (Value::Boolean(boolean), _) => Ok(Json::Bool(*boolean)),
        (Value::Number(number), _) => {
            checked_number(number.clone()).map(|number| number.json_value())
        }
        (Value::String(text), _) => Ok(Json::String(text.clone())),
        (Value::List(items), Type::List(element_type) | Type::Set(element_type)) => {
            Ok(Json::Array(
                items
                    .iter()
                    .map(|item| to_state_json(item, element_type))
                    .collect::<Result<_>>()?,
            ))
        }
        (Value::List(items), Type::Tuple(element_types)) => Ok(Json::Array(
            items
                .iter()
                .zip(element_types)
                .map(|(item, element_type)| to_state_json(item, element_type))
                .collect::<Result<_>>()?,
        )),
        (Value::Object(attributes), Type::Map(element_type)) => Ok(Json::Object(
            attributes
                .iter()
                .map(|(key, element)| Ok((key.clone(), to_state_json(element, element_type)?)))
                .collect::<Result<_>>()?,
        )),
        (Value::Object(attributes), Type::Object(attribute_types)) => Ok(Json::Object(
            attribute_types
                .iter()
                .map(|(name, attribute_type)| {
                    let attribute = attributes.get(name).unwrap_or(&Value::Null);
                    Ok((name.clone(), to_state_json(attribute, attribute_type)?))
                })
                .collect::<Result<_>>()?,
        )),
        (mismatched, expected_type) => Err(InfrastructureError::codec(format!(
            "a {} value does not conform to type {}",
            mismatched.kind(),
            expected_type.diagnostic_kind()
        ))),
    }
}

// ---------------------------------------------------------------------------
// MessagePack
// ---------------------------------------------------------------------------

/// Encode a value as cty MessagePack against `value_type`.
///
/// # Errors
///
/// Returns [`InfrastructureError::Codec`] if the value does not conform to the type.
pub fn to_message_pack(value: &Value, value_type: &Type) -> Result<Vec<u8>> {
    let encoded = encode(value, value_type)?;
    let mut buffer = Vec::new();
    rmpv::encode::write_value(&mut buffer, &encoded).map_err(|error| {
        InfrastructureError::codec(format!("MessagePack encode failed: {error}"))
    })?;
    Ok(buffer)
}

/// Decode cty MessagePack against `value_type`. Empty input decodes to null.
///
/// # Errors
///
/// Returns [`InfrastructureError::Codec`] if the bytes are not valid for the type.
pub fn from_message_pack(bytes: &[u8], value_type: &Type) -> Result<Value> {
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    let mut cursor = bytes;
    let raw = rmpv::decode::read_value(&mut cursor).map_err(|error| {
        InfrastructureError::codec(format!("MessagePack decode failed: {error}"))
    })?;
    decode(&raw, value_type)
}

fn encode(value: &Value, value_type: &Type) -> Result<rmpv::Value> {
    use rmpv::Value as MessagePack;
    let dynamic = |concrete: &Type, inner: &Value| -> Result<rmpv::Value> {
        let type_json = serde_json::to_vec(&concrete.to_json()).map_err(|error| {
            InfrastructureError::codec(format!(
                "encode dynamic type ({})",
                crate::error::json_error_category(&error)
            ))
        })?;
        Ok(MessagePack::Array(vec![
            MessagePack::Binary(type_json),
            encode(inner, concrete)?,
        ]))
    };
    match (value, value_type) {
        // The type the value arrived with goes back with it, even for a
        // null or unknown value, as cty encodes them.
        (Value::Typed(typed), Type::Dynamic) => dynamic(&typed.value_type, &typed.value),
        (Value::Typed(typed), _) => encode(&typed.value, value_type),
        (Value::Null, _) => Ok(MessagePack::Nil),
        (Value::Unknown, _) => Ok(MessagePack::Ext(UNKNOWN_EXTENSION, Vec::new())),
        (dynamic_value, Type::Dynamic) => dynamic(&infer_type(dynamic_value), dynamic_value),
        (Value::Boolean(boolean), Type::Boolean) => Ok(MessagePack::Boolean(*boolean)),
        (Value::Number(number), Type::Number) => encode_number(number),
        (Value::String(text), Type::String) => Ok(MessagePack::String(text.clone().into())),
        (Value::List(items), Type::List(element_type) | Type::Set(element_type)) => {
            Ok(MessagePack::Array(
                items
                    .iter()
                    .map(|item| encode(item, element_type))
                    .collect::<Result<_>>()?,
            ))
        }
        (Value::List(items), Type::Tuple(element_types)) if items.len() == element_types.len() => {
            Ok(MessagePack::Array(
                items
                    .iter()
                    .zip(element_types)
                    .map(|(item, element_type)| encode(item, element_type))
                    .collect::<Result<_>>()?,
            ))
        }
        (Value::Object(attributes), Type::Map(element_type)) => Ok(MessagePack::Map(
            attributes
                .iter()
                .map(|(key, element)| {
                    Ok((
                        MessagePack::String(key.clone().into()),
                        encode(element, element_type)?,
                    ))
                })
                .collect::<Result<_>>()?,
        )),
        (Value::Object(attributes), Type::Object(attribute_types)) => {
            if attributes
                .keys()
                .any(|name| !attribute_types.contains_key(name))
            {
                return Err(InfrastructureError::codec(
                    "unexpected attribute '<hidden field>'",
                ));
            }
            Ok(MessagePack::Map(
                attribute_types
                    .iter()
                    .map(|(name, attribute_type)| {
                        let attribute = attributes.get(name).unwrap_or(&Value::Null);
                        Ok((
                            MessagePack::String(name.clone().into()),
                            encode(attribute, attribute_type)?,
                        ))
                    })
                    .collect::<Result<_>>()?,
            ))
        }
        (mismatched, expected_type) => Err(InfrastructureError::codec(format!(
            "a {} value does not conform to type {}",
            mismatched.kind(),
            expected_type.diagnostic_kind()
        ))),
    }
}

/// An exact finite decimal, independent of exponent or decimal-point spelling.
#[derive(Debug, PartialEq, Eq)]
struct NormalizedNumber {
    negative: bool,
    coefficient: String,
    exponent: i64,
}

fn normalized_number(number: &Number) -> Option<NormalizedNumber> {
    let text = number.to_string();
    let (mantissa, exponent) = text
        .split_once('e')
        .or_else(|| text.split_once('E'))
        .map_or(Some((text.as_str(), 0)), |(mantissa, exponent)| {
            exponent
                .parse::<i64>()
                .ok()
                .map(|exponent| (mantissa, exponent))
        })?;
    let negative = mantissa.starts_with('-');
    let mantissa = mantissa.strip_prefix('-').unwrap_or(mantissa);
    let fractional_digits = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let digits: String = mantissa
        .chars()
        .filter(|character| *character != '.')
        .collect();
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return Some(NormalizedNumber {
            negative: false,
            coefficient: "0".into(),
            exponent: 0,
        });
    }
    let coefficient = significant.trim_end_matches('0');
    let trailing_zeros = significant.len().checked_sub(coefficient.len())?;
    let shift = i64::try_from(trailing_zeros)
        .ok()?
        .checked_sub(i64::try_from(fractional_digits).ok()?)?;
    let exponent = exponent.checked_add(shift)?;
    Some(NormalizedNumber {
        negative,
        coefficient: coefficient.to_string(),
        exponent,
    })
}

fn numbers_equal(left: &Number, right: &Number) -> bool {
    left.text == right.text
        || normalized_number(left)
            .zip(normalized_number(right))
            .is_some_and(|(left, right)| left == right)
}

fn checked_number(number: Number) -> Result<Number> {
    let raw: &RawValue = serde_json::from_str(number.as_str())
        .map_err(|_| InfrastructureError::codec("invalid numeric literal"))?;
    if !matches!(raw.get().as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
        return Err(InfrastructureError::codec("invalid numeric literal"));
    }
    Ok(number)
}

/// Recover the exact rational binary float as a finite decimal. A shortest
/// float rendering (for example, "0.1") does not preserve the cty number.
fn exact_float_number(float: f64) -> Result<Number> {
    if !float.is_finite() {
        return Err(InfrastructureError::codec(
            "non-finite cty numbers are unsupported",
        ));
    }
    let bits = float.to_bits();
    let exponent_bits = u16::try_from((bits >> 52) & 0x7ff)
        .map_err(|_| InfrastructureError::codec("invalid floating-point exponent"))?;
    let mut significand = bits & ((1_u64 << 52) - 1);
    let mut exponent = if exponent_bits == 0 {
        -1074
    } else {
        significand |= 1_u64 << 52;
        i32::from(exponent_bits) - 1075
    };
    if significand == 0 {
        return if bits >> 63 == 0 {
            "0.0".parse()
        } else {
            "-0.0".parse()
        };
    }
    while exponent < 0 && significand.is_multiple_of(2) {
        significand /= 2;
        exponent += 1;
    }
    let coefficient = decimal_coefficient(significand, exponent);
    let sign = if bits >> 63 == 0 { "" } else { "-" };
    format!("{sign}{coefficient}e{}", exponent.min(0))
        .parse::<Number>()
        .map_err(|_| {
            InfrastructureError::codec("exact floating-point number could not be represented")
        })
}

/// Decimal limbs keep the conversion bounded by the IEEE exponent (1074),
/// without a bigint dependency or any intermediate floating-point arithmetic.
fn decimal_coefficient(significand: u64, exponent: i32) -> String {
    const BASE: u64 = 1_000_000_000;
    let mut limbs = vec![significand % BASE];
    if significand >= BASE {
        limbs.push(significand / BASE);
    }
    let factor = if exponent < 0 { 5 } else { 2 };
    for _ in 0..exponent.unsigned_abs() {
        let mut carry = 0;
        for limb in &mut limbs {
            // Each limb is below BASE and factor is 2 or 5; this cannot overflow.
            let expanded = *limb * factor + carry;
            *limb = expanded % BASE;
            carry = expanded / BASE;
        }
        if carry > 0 {
            limbs.push(carry);
        }
    }
    let mut chunks = limbs.iter().rev();
    let mut text = chunks.next().map_or_else(String::new, ToString::to_string);
    for chunk in chunks {
        // Writing to String is infallible.
        let _ = write!(&mut text, "{chunk:09}");
    }
    text
}

fn encode_number(number: &Number) -> Result<rmpv::Value> {
    checked_number(number.clone())?;
    let normalized = normalized_number(number)
        .ok_or_else(|| InfrastructureError::codec("unsupported numeric exponent"))?;
    if let Some(signed) = number.as_i64() {
        return Ok(rmpv::Value::from(signed));
    }
    if let Some(unsigned) = number.as_u64() {
        return Ok(rmpv::Value::from(unsigned));
    }
    if let Some(float) = number.as_f64().filter(|float| float.is_finite())
        && numbers_equal(number, &exact_float_number(float)?)
        && (normalized.exponent < 0 || normalized.coefficient == "0")
    {
        return Ok(rmpv::Value::F64(float));
    }
    // This subset stays separated by more than a 512-bit provider ULP,
    // including from exact F64 values. Integer strings need at most 496
    // significant binary bits; fractional exponents stay bounded as well.
    if normalized.coefficient.len() > 100 || !(-190..=70).contains(&normalized.exponent) {
        return Err(InfrastructureError::codec(
            "number exceeds the lossless Terraform provider precision limits",
        ));
    }
    Ok(rmpv::Value::String(number.to_string().into()))
}

fn decode(raw: &rmpv::Value, value_type: &Type) -> Result<Value> {
    use rmpv::Value as MessagePack;
    match (raw, value_type) {
        (MessagePack::Nil, _) => Ok(Value::Null),
        (MessagePack::Ext(..), _) => Ok(Value::Unknown),
        (MessagePack::Array(parts), Type::Dynamic) if parts.len() == 2 => {
            let type_bytes: &[u8] = match &parts[0] {
                MessagePack::Binary(binary) => binary,
                MessagePack::String(text) => text.as_bytes(),
                other => {
                    return Err(InfrastructureError::codec(format!(
                        "dynamic value type must be bytes, got {}",
                        message_pack_kind(other)
                    )));
                }
            };
            let concrete = Type::from_json_bytes(type_bytes)?;
            let inner = decode(&parts[1], &concrete)?;
            Ok(Value::typed(concrete, inner))
        }
        (MessagePack::Boolean(boolean), Type::Boolean) => Ok(Value::Boolean(*boolean)),
        (MessagePack::Integer(integer), Type::Number) => integer
            .as_i64()
            .map(Number::from)
            .or_else(|| integer.as_u64().map(Number::from))
            .map(Value::Number)
            .ok_or_else(|| InfrastructureError::codec("integer out of range")),
        (MessagePack::F64(float), Type::Number) => float_number(*float),
        (MessagePack::F32(float), Type::Number) => float_number(f64::from(*float)),
        (MessagePack::String(text), Type::Number) => text
            .as_str()
            .and_then(|string| string.parse::<Number>().ok())
            .ok_or_else(|| InfrastructureError::codec("invalid number string"))
            .and_then(checked_number)
            .map(Value::Number),
        (MessagePack::String(text), Type::String) => text
            .as_str()
            .map(|string| Value::String(string.to_string()))
            .ok_or_else(|| InfrastructureError::codec("string is not valid UTF-8")),
        (MessagePack::Array(items), Type::List(element_type) | Type::Set(element_type)) => {
            Ok(Value::List(
                items
                    .iter()
                    .map(|item| decode(item, element_type))
                    .collect::<Result<_>>()?,
            ))
        }
        (MessagePack::Array(items), Type::Tuple(element_types))
            if items.len() == element_types.len() =>
        {
            Ok(Value::List(
                items
                    .iter()
                    .zip(element_types)
                    .map(|(item, element_type)| decode(item, element_type))
                    .collect::<Result<_>>()?,
            ))
        }
        (MessagePack::Map(entries), Type::Map(element_type)) => Ok(Value::Object(
            entries
                .iter()
                .map(|(raw_key, raw_element)| {
                    Ok((map_key(raw_key)?, decode(raw_element, element_type)?))
                })
                .collect::<Result<_>>()?,
        )),
        (MessagePack::Map(entries), Type::Object(attribute_types)) => {
            let mut decoded: BTreeMap<String, Value> = attribute_types
                .keys()
                .map(|name| (name.clone(), Value::Null))
                .collect();
            for (raw_key, raw_attribute) in entries {
                let key = map_key(raw_key)?;
                let attribute_type = attribute_types.get(&key).ok_or_else(|| {
                    InfrastructureError::codec("unexpected attribute '<hidden field>'")
                })?;
                decoded.insert(key, decode(raw_attribute, attribute_type)?);
            }
            Ok(Value::Object(decoded))
        }
        (other, expected_type) => Err(InfrastructureError::codec(format!(
            "a MessagePack {} does not match type {}",
            message_pack_kind(other),
            expected_type.diagnostic_kind()
        ))),
    }
}

/// Name the kind of a JSON value without revealing it (it may be a secret).
fn json_kind(json: &serde_json::Value) -> &'static str {
    match json {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "a list",
        serde_json::Value::Object(_) => "an object",
    }
}

/// Name the kind of a MessagePack value without revealing it.
const fn message_pack_kind(value: &rmpv::Value) -> &'static str {
    match value {
        rmpv::Value::Nil => "nil",
        rmpv::Value::Boolean(_) => "boolean",
        rmpv::Value::Integer(_) | rmpv::Value::F32(_) | rmpv::Value::F64(_) => "number",
        rmpv::Value::String(_) => "string",
        rmpv::Value::Binary(_) => "binary",
        rmpv::Value::Array(_) => "array",
        rmpv::Value::Map(_) => "map",
        rmpv::Value::Ext(..) => "extension",
    }
}

/// One step of an attribute path, as providers report it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PathStep {
    /// Object attribute.
    Attribute(String),
    /// Map element.
    Key(String),
    /// List or tuple element.
    Index(i64),
}

/// Look up a nested value. Returns `None` when the path does not exist, and
/// `Some(Value::Unknown)` when an unknown value is reached on the way.
#[must_use]
pub fn value_at_path<'value>(value: &'value Value, path: &[PathStep]) -> Option<&'value Value> {
    let Some((first, rest)) = path.split_first() else {
        return Some(value);
    };
    match (value, first) {
        (Value::Typed(typed), _) => value_at_path(&typed.value, path),
        (Value::Unknown, _) => Some(value),
        (Value::Object(attributes), PathStep::Attribute(name) | PathStep::Key(name)) => attributes
            .get(name)
            .and_then(|next| value_at_path(next, rest)),
        (Value::List(elements), PathStep::Index(index)) => usize::try_from(*index)
            .ok()
            .and_then(|index| elements.get(index))
            .and_then(|next| value_at_path(next, rest)),
        _ => None,
    }
}

/// Decode a provider value that arrived as JSON rather than MessagePack
/// (Terraform accepts both encodings).
///
/// # Errors
///
/// Returns [`InfrastructureError::Codec`] when the JSON does not fit `value_type`.
pub fn from_json_bytes(bytes: &[u8], value_type: &Type) -> Result<Value> {
    let raw: &RawValue = serde_json::from_slice(bytes).map_err(|error| {
        InfrastructureError::codec(format!(
            "invalid JSON value ({} at line {}, column {})",
            crate::error::json_error_category(&error),
            error.line(),
            error.column()
        ))
    })?;
    let paths: Vec<_> = scan_json_numbers(raw)?
        .into_iter()
        .map(|token| token.path)
        .collect();
    let json = parse_json_preserving_numbers(raw, &paths)?;
    from_state_json(&json, value_type)
}

/// Decode cty JSON (the state format) against `value_type`, unwrapping the
/// `{"value": ..., "type": ...}` wrapper of every `dynamic` value at any
/// depth.
fn from_state_json(json: &serde_json::Value, value_type: &Type) -> Result<Value> {
    use serde_json::Value as Json;
    if json.is_null() {
        return Ok(Value::Null);
    }
    let mismatch = || {
        InfrastructureError::codec(format!(
            "found {} in JSON where type {} was expected",
            json_kind(json),
            value_type.diagnostic_kind()
        ))
    };
    match (value_type, json) {
        (Type::Dynamic, Json::Object(wrapper)) => {
            let (Some(inner), Some(inner_type)) = (wrapper.get("value"), wrapper.get("type"))
            else {
                return Err(InfrastructureError::codec(
                    "a dynamic value in JSON is missing its value and type wrapper",
                ));
            };
            let concrete = Type::from_json(inner_type)?;
            let value = from_state_json(inner, &concrete)?;
            Ok(Value::typed(concrete, value))
        }
        (Type::Dynamic, _) => Err(InfrastructureError::codec(
            "a dynamic value in JSON is missing its value and type wrapper",
        )),
        (Type::List(element_type) | Type::Set(element_type), Json::Array(items)) => items
            .iter()
            .map(|item| from_state_json(item, element_type))
            .collect::<Result<_>>()
            .map(Value::List),
        (Type::Tuple(element_types), Json::Array(items)) if items.len() == element_types.len() => {
            items
                .iter()
                .zip(element_types)
                .map(|(item, element_type)| from_state_json(item, element_type))
                .collect::<Result<_>>()
                .map(Value::List)
        }
        (Type::Map(element_type), Json::Object(entries)) => entries
            .iter()
            .map(|(key, element)| Ok((key.clone(), from_state_json(element, element_type)?)))
            .collect::<Result<_>>()
            .map(Value::Object),
        (Type::Object(attribute_types), Json::Object(entries)) => {
            if entries.keys().any(|key| !attribute_types.contains_key(key)) {
                return Err(InfrastructureError::codec(
                    "unexpected attribute '<hidden field>'",
                ));
            }
            attribute_types
                .iter()
                .map(|(name, attribute_type)| {
                    let value = entries.get(name).map_or(Ok(Value::Null), |attribute| {
                        from_state_json(attribute, attribute_type)
                    })?;
                    Ok((name.clone(), value))
                })
                .collect::<Result<_>>()
                .map(Value::Object)
        }
        (Type::Boolean | Type::Number | Type::String, _) => {
            from_configuration_json(json, value_type, &mut Vec::new()).map_err(|_| mismatch())
        }
        _ => Err(mismatch()),
    }
}

impl Type {
    /// The type found by following `path` into a value of this type, or
    /// `None` when the path does not fit the type. Paths into `dynamic`
    /// values stay `dynamic`.
    #[must_use]
    pub fn at_path(&self, path: &[PathStep]) -> Option<Self> {
        let Some((first, rest)) = path.split_first() else {
            return Some(self.clone());
        };
        match (self, first) {
            (Self::Dynamic, _) => Some(Self::Dynamic),
            (Self::Object(attributes), PathStep::Attribute(name) | PathStep::Key(name)) => {
                attributes.get(name).and_then(|next| next.at_path(rest))
            }
            (Self::Map(element_type), PathStep::Key(_)) => element_type.at_path(rest),
            (Self::List(element_type) | Self::Set(element_type), PathStep::Index(_)) => {
                element_type.at_path(rest)
            }
            (Self::Tuple(element_types), PathStep::Index(index)) => usize::try_from(*index)
                .ok()
                .and_then(|index| element_types.get(index))
                .and_then(|next| next.at_path(rest)),
            _ => None,
        }
    }
}

fn float_number(float: f64) -> Result<Value> {
    exact_float_number(float).map(Value::Number)
}

fn map_key(key: &rmpv::Value) -> Result<String> {
    key.as_str().map(str::to_string).ok_or_else(|| {
        InfrastructureError::codec(format!(
            "map key must be a string, got {}",
            message_pack_kind(key)
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn raw_json_numbers_preserve_only_explicit_numeric_paths_and_reserved_objects() -> Result<()> {
        let raw: &RawValue = serde_json::from_str(r#"{"items":[{"count":18446744073709551617,"text":"18446744073709551617"}],"ordinary":{"$serde_json::private::Number":"42"},"half":0.5}"#)
            .map_err(|_| json_number_error())?;
        let tokens = scan_json_numbers(raw)?;
        assert_eq!(tokens.len(), 2);
        let path = vec![
            ConfigurationSelector::Field("items".into()),
            ConfigurationSelector::Index(0),
            ConfigurationSelector::Field("count".into()),
        ];
        assert!(
            tokens
                .iter()
                .any(|token| token.path == path && token.number.as_str() == "18446744073709551617")
        );
        assert!(parse_json_preserving_numbers(raw, &[]).is_err());
        let parsed = parse_json_preserving_numbers(raw, std::slice::from_ref(&path))?;
        assert_eq!(parsed["items"][0]["count"], json!("18446744073709551617"));
        assert_eq!(parsed["items"][0]["text"], json!("18446744073709551617"));
        assert_eq!(
            parsed["ordinary"],
            json!({"$serde_json::private::Number":"42"})
        );
        assert_eq!(parsed["half"], json!(0.5));
        let legacy: serde_json::Value =
            serde_json::from_str(r#"{"$serde_json::private::Number":"42"}"#)
                .map_err(|_| json_number_error())?;
        assert!(legacy.is_object());
        let rendered = format!("{tokens:?}");
        assert!(!rendered.contains("18446744073709551617"));
        Ok(())
    }

    #[test]
    fn raw_json_traversals_refuse_excessive_depth_without_rendering_input() -> Result<()> {
        let text = format!("{}123{}", "[".repeat(256), "]".repeat(256));
        let raw = RawValue::from_string(text).map_err(|_| json_number_error())?;
        assert!(scan_json_numbers(&raw).is_err());
        assert!(parse_json_preserving_numbers(&raw, &[]).is_err());
        Ok(())
    }

    #[test]
    fn raw_json_traversals_refuse_duplicate_keys_without_rendering_input() -> Result<()> {
        let raw: &RawValue = serde_json::from_str(r#"{"secret":18446744073709551617,"secret":1}"#)
            .map_err(|_| json_number_error())?;
        let scan = scan_json_numbers(raw).unwrap_err();
        let parse = parse_json_preserving_numbers(raw, &[]).unwrap_err();
        for error in [scan, parse] {
            let rendered = error.to_string();
            assert!(!rendered.contains("secret"));
            assert!(!rendered.contains("18446744073709551617"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn unencodable_returned_numbers_remain_recordable_and_recoverable()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::state::{
            ConditionalPut, ManagedResource, MemoryStateStore, RecordVersion, ResourceAddress,
            StateStore,
        };
        use crate::tenant::TenantKey;
        use crate::unrecorded::{RecoverOptions, RecoverOverrides, UnrecordedStore};

        let text = "12345678901234567890123456789".repeat(400);
        let returned = decode(&rmpv::Value::String(text.clone().into()), &Type::Number)?;
        assert!(to_message_pack(&returned, &Type::Number).is_err());
        let root = tempfile::tempdir()?;
        let unrecorded = UnrecordedStore::at(root.path());
        let resource = ManagedResource {
            address: ResourceAddress::new("example_number", "created"),
            provider: "example".into(),
            provider_source: "registry.terraform.io/example/number".into(),
            schema_version: 0,
            state: json!({"value": returned.to_state_json(&Type::Number)?}),
            private: Vec::new(),
            dependencies: Vec::new(),
            sensitive_attributes: vec!["value".into()],
            tainted: false,
            identity: None,
            serial: 0,
            generation: uuid::Uuid::nil(),
        };
        let tenant = TenantKey::try_new("example.com/numeric-state", "recovery")?;
        unrecorded.save(
            &tenant,
            &ConditionalPut {
                resource: &resource,
                expected: RecordVersion::Absent,
            },
        )?;
        let state = MemoryStateStore::new();
        let lock = state.lock(&tenant, "numeric recovery").await?;
        unrecorded
            .recover(
                &state,
                &tenant,
                &RecoverOptions {
                    lock: &lock,
                    overrides: RecoverOverrides::default(),
                },
            )
            .await?;
        let restored = state.list(&tenant).await?;
        assert_eq!(restored.len(), 1);
        assert!(
            restored[0].state["value"].as_str() == Some(text.as_str()),
            "recorded numeric content changed"
        );
        let decoded = from_json_bytes(
            &serde_json::to_vec(&restored[0].state["value"])?,
            &Type::Number,
        )?;
        assert_eq!(decoded, returned);
        assert!(to_message_pack(&decoded, &Type::Number).is_err());
        state.unlock(&tenant, &lock).await?;
        Ok(())
    }

    #[test]
    fn provider_precision_admission_refuses_large_neighbors_and_extreme_exponents() -> Result<()> {
        let power = decimal_coefficient(1, 600);
        let mut neighbor = power.clone();
        // 2^600 ends in 6, so its distinct integer neighbor needs no carry.
        assert!(neighbor.ends_with('6'));
        neighbor.pop();
        neighbor.push('7');
        assert!(to_message_pack(&Value::Number(power.parse()?), &Type::Number).is_err());
        for text in [
            neighbor,
            "1e1000000000".into(),
            "1e-1000000000".into(),
            "1e400".into(),
            "1e-400".into(),
        ] {
            let result = text
                .parse::<Number>()
                .and_then(|number| to_message_pack(&Value::Number(number), &Type::Number));
            assert!(result.is_err());
        }
        assert!(to_message_pack(&Value::Number("0.1".parse()?), &Type::Number).is_ok());
        Ok(())
    }

    #[test]
    #[ignore = "requires Go and the pinned upstream Terraform protocol modules"]
    fn numbers_interoperate_with_real_protocol_five_and_six_codecs()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let fixture =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/numeric_interop");
        let mut cases = Vec::new();
        for text in [
            "0.1".to_string(),
            "0.5".to_string(),
            "18446744073709551617".to_string(),
            "0.123456789012345678901234567890123456789".to_string(),
            "1.000000000000000000000000000000000000001".to_string(),
            "1.000000000000000000000000000000000000002".to_string(),
            format!("{}e70", "9".repeat(100)),
            format!("{}e-190", "9".repeat(100)),
        ] {
            let number: Number = text.parse()?;
            let value = Value::Number(number.clone());
            cases.push(json!({"literal": number.as_str(), "messagePack": hex::encode(to_message_pack(&value, &Type::Number)?), "state": value.to_state_json(&Type::Number)?}));
        }
        for float in [
            0.1,
            -0.1,
            1.0,
            -0.0,
            f64::from_bits(1),
            f64::MIN_POSITIVE,
            1e20,
        ] {
            let number = exact_float_number(float)?;
            let value = Value::Number(number.clone());
            cases.push(json!({"literal": number.as_str(), "floatBits": float.to_bits(), "messagePack": hex::encode(to_message_pack(&value, &Type::Number)?), "state": value.to_state_json(&Type::Number)?}));
        }
        for float in [f64::MAX, 2_f64.powi(600)] {
            let number = exact_float_number(float)?;
            let value = Value::Number(number.clone());
            assert!(to_message_pack(&value, &Type::Number).is_err());
            cases.push(json!({"literal": number.as_str(), "floatBits": float.to_bits(), "state": value.to_state_json(&Type::Number)?}));
        }
        let expected_cases = cases.len();
        let mut command =
            if let Some(binary) = std::env::var_os("CUENV_INFRASTRUCTURE_TEST_NUMERIC_INTEROP") {
                Command::new(binary)
            } else {
                let mut command = Command::new("go");
                command
                    .args(["run", "-mod=readonly", "."])
                    .current_dir(fixture);
                command
            };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("missing protocol input"))?
            .write_all(&serde_json::to_vec(&cases)?)?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            // The Go harness uses value-free diagnostic categories exclusively.
            return Err(std::io::Error::other(format!(
                "upstream numeric interoperability failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ))
            .into());
        }
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains(&format!("{expected_cases} numeric cases passed"))
        );
        Ok(())
    }

    #[test]
    fn exact_decimals_and_integers_larger_than_u64_round_trip_without_float_conversion()
    -> Result<()> {
        for text in [
            "18446744073709551617",
            "900719925474099312345678901234567890123456789",
            "0.123456789012345678901234567890123456789",
            "-0.000000000000000000000000000000000000000001234567890123456789",
        ] {
            let value = from_json_bytes(text.as_bytes(), &Type::Number)?;
            assert_eq!(
                value.to_state_json(&Type::Number)?,
                serde_json::Value::String(text.to_string())
            );
            let bytes = to_message_pack(&value, &Type::Number)?;
            assert!(matches!(
                encode(&value, &Type::Number)?,
                rmpv::Value::String(_)
            ));
            let decoded = from_message_pack(&bytes, &Type::Number)?;
            assert!(raw_equal(&decoded, &value, &Type::Number));
            assert_eq!(
                decoded.to_state_json(&Type::Number)?,
                serde_json::Value::String(text.to_string())
            );
        }
        Ok(())
    }

    #[test]
    fn numeric_equality_compares_exact_values_across_decimal_spellings() -> Result<()> {
        for spellings in [
            ["1", "1.0", "1e0"],
            ["1000", "10e2", "100000e-2"],
            ["-12", "-1200.00e-2", "-1.20e1"],
            ["0", "-0.0", "0e999"],
        ] {
            let expected = from_json_bytes(spellings[0].as_bytes(), &Type::Number)?;
            for spelling in spellings {
                let value = from_json_bytes(spelling.as_bytes(), &Type::Number)?;
                assert_eq!(expected, value);
                assert!(semantically_equal(&expected, &value, &Type::Number));
                assert!(raw_equal(&expected, &value, &Type::Number));
            }
        }
        Ok(())
    }

    #[test]
    fn different_decimals_that_share_a_float_never_compare_equal() -> Result<()> {
        for (left, right) in [
            (
                "1.000000000000000000000000000000000000001",
                "1.000000000000000000000000000000000000002",
            ),
            ("9007199254740992", "9007199254740993"),
            ("18446744073709551616", "18446744073709551617"),
            ("1e-400", "2e-400"),
        ] {
            let left_number = left
                .parse::<Number>()
                .map_err(|_| InfrastructureError::codec("invalid test number"))?;
            let right_number = right
                .parse::<Number>()
                .map_err(|_| InfrastructureError::codec("invalid test number"))?;
            assert_eq!(
                left_number.as_f64().map(f64::to_bits),
                right_number.as_f64().map(f64::to_bits)
            );
            let left = Value::Number(left_number);
            let right = Value::Number(right_number);
            assert_ne!(left, right);
            assert!(!semantically_equal(&left, &right, &Type::Number));
            assert!(!raw_equal(&left, &right, &Type::Number));
        }
        Ok(())
    }

    #[test]
    fn supported_provider_floats_keep_their_exact_binary_value_across_state_json() -> Result<()> {
        for float in [
            0.1,
            -0.1,
            1.0,
            -0.0,
            f64::MIN_POSITIVE,
            f64::from_bits(1),
            f64::from_bits(0x3ff0_0000_0000_0001),
            std::f64::consts::PI,
            1e20,
        ] {
            let decoded = decode(&rmpv::Value::F64(float), &Type::Number)?;
            let state = decoded.to_state_json(&Type::Number)?.to_string();
            let restored = from_json_bytes(state.as_bytes(), &Type::Number)?;
            let encoded = encode(&restored, &Type::Number)?;
            let Value::Number(round_trip) = decode(&encoded, &Type::Number)? else {
                return Err(InfrastructureError::codec(
                    "provider float lost its exact representation",
                ));
            };
            assert_eq!(round_trip.as_f64().map(f64::to_bits), Some(float.to_bits()));
            assert_eq!(Value::Number(round_trip), restored);
        }
        let binary_tenth = decode(&rmpv::Value::F64(0.1), &Type::Number)?;
        let decimal_tenth = from_json_bytes(b"0.1", &Type::Number)?;
        assert!(!semantically_equal(
            &binary_tenth,
            &decimal_tenth,
            &Type::Number
        ));
        let exact_tenth = from_json_bytes(
            b"0.1000000000000000055511151231257827021181583404541015625",
            &Type::Number,
        )?;
        assert!(raw_equal(&binary_tenth, &exact_tenth, &Type::Number));
        let single = decode(&rmpv::Value::F32(0.1_f32), &Type::Number)?;
        let rmpv::Value::F64(promoted) = encode(&single, &Type::Number)? else {
            return Err(InfrastructureError::codec(
                "single precision value was not preserved",
            ));
        };
        assert_eq!(promoted.to_bits(), f64::from(0.1_f32).to_bits());
        Ok(())
    }

    #[test]
    fn structural_equality_normalizes_nested_numbers_without_ignoring_collection_order()
    -> Result<()> {
        let integer = Value::Number(Number::from(1));
        let decimal = from_json_bytes(b"1.0", &Type::Number)?;
        let object = |number: Value| {
            Value::Object(BTreeMap::from([(
                "items".into(),
                Value::List(vec![number, Value::Unknown]),
            )]))
        };
        assert_eq!(object(integer.clone()), object(decimal.clone()));
        assert_ne!(
            Value::List(vec![integer.clone(), Value::Unknown]),
            Value::List(vec![Value::Unknown, decimal.clone()])
        );
        assert_eq!(
            Value::typed(Type::Number, integer.clone()),
            Value::typed(Type::Number, decimal.clone())
        );
        assert_ne!(
            Value::typed(
                Type::List(Box::new(Type::Number)),
                Value::List(vec![integer.clone()])
            ),
            Value::typed(Type::Tuple(vec![Type::Number]), Value::List(vec![decimal])),
        );
        assert_ne!(integer.clone(), Value::typed(Type::Number, integer));
        Ok(())
    }

    #[test]
    fn unsupported_numbers_fail_without_disclosing_their_values() -> Result<()> {
        for float in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(decode(&rmpv::Value::F64(float), &Type::Number).is_err());
        }
        let text = "1e9223372036854775808";
        let number = Number {
            text: text.to_string(),
        };
        let value = Value::Number(number);
        let error = to_message_pack(&value, &Type::Number)
            .err()
            .ok_or_else(|| InfrastructureError::codec("unsupported number was sent"))?
            .to_string();
        assert!(!error.contains(text));
        let state = value.to_state_json(&Type::Number)?;
        assert_eq!(state, json!(text));
        assert_eq!(
            from_json_bytes(
                &serde_json::to_vec(&state).map_err(|_| json_number_error())?,
                &Type::Number
            )?,
            value
        );
        Ok(())
    }

    fn object_type() -> Type {
        Type::from_json(&json!(["object", {
            "id": "string",
            "length": "number",
            "keepers": ["map", "string"],
            "tags": ["set", "string"],
            "extra": "dynamic",
        }]))
        .unwrap()
    }

    #[test]
    fn type_json_round_trips() {
        let value_type = object_type();
        assert_eq!(Type::from_json(&value_type.to_json()).unwrap(), value_type);
    }

    #[test]
    fn configuration_json_fills_missing_attributes_with_null() {
        let value = Value::from_configuration_json(&json!({"length": 2}), &object_type()).unwrap();
        assert_eq!(value.attribute("length"), Some(&Value::Number(2.into())));
        assert_eq!(value.attribute("id"), Some(&Value::Null));
        assert_eq!(value.attribute("keepers"), Some(&Value::Null));
    }

    #[test]
    fn configuration_json_rejects_unknown_arguments() {
        let error =
            Value::from_configuration_json(&json!({"nope": 1}), &object_type()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported argument '<hidden field>'"),
            "{error}"
        );
    }

    #[test]
    fn unclassified_configuration_diagnostics_hide_map_keys_and_unknown_fields() {
        use rmpv::Value as MessagePack;
        let key = format!("unregistered-key-{}", uuid::Uuid::new_v4());
        let value_type = Type::Object(BTreeMap::from([(
            "config".into(),
            Type::Map(Box::new(Type::Object(BTreeMap::from([(
                "leaf".into(),
                Type::Number,
            )])))),
        )]));
        let invalid = json!({"config": {&key: {"leaf": []}}});
        let error = Value::from_configuration_json(&invalid, &value_type)
            .unwrap_err()
            .to_string();
        assert!(error.contains("config.<hidden key>.leaf"), "{error}");
        assert!(!error.contains(&key), "{error}");
        let extra = json!({"config": {"ordinary": {&key: 1}}});
        let error = Value::from_configuration_json(&extra, &value_type)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported argument"), "{error}");
        assert!(!error.contains(&key), "{error}");
        assert!(!error.contains("ordinary"), "{error}");
        let object_type = Type::Object(BTreeMap::new());
        let object = Value::Object(BTreeMap::from([(key.clone(), Value::Null)]));
        let packed = MessagePack::Map(vec![(
            MessagePack::String(key.clone().into()),
            MessagePack::Nil,
        )]);
        for error in [
            encode(&object, &object_type).unwrap_err(),
            decode(&packed, &object_type).unwrap_err(),
            from_state_json(&json!({&key: null}), &object_type).unwrap_err(),
        ] {
            assert!(!error.to_string().contains(&key), "{error}");
        }
    }

    #[test]
    fn configuration_json_converts_primitives_like_terraform() {
        let value =
            Value::from_configuration_json(&json!({"id": 5, "length": "3"}), &object_type())
                .unwrap();
        assert_eq!(value.attribute("id"), Some(&Value::String("5".into())));
        assert_eq!(value.attribute("length"), Some(&Value::Number(3.into())));
    }

    #[test]
    fn message_pack_round_trips_including_dynamic_and_unknown() {
        let value_type = object_type();
        let value = Value::from_configuration_json(
            &json!({
                "id": "abc",
                "length": 2.5,
                "keepers": {"owner": "platform"},
                "tags": ["blue", "green"],
                "extra": {"nested": [1, true]},
            }),
            &value_type,
        )
        .unwrap();
        let bytes = to_message_pack(&value, &value_type).unwrap();
        let decoded = from_message_pack(&bytes, &value_type).unwrap();
        // Configuration values carry no type; decoded dynamic values do.
        assert!(raw_equal(&decoded, &value, &value_type));
        assert!(matches!(decoded.attribute("extra"), Some(Value::Typed(_))));
        assert_eq!(to_message_pack(&decoded, &value_type).unwrap(), bytes);

        let mut with_unknown = decoded;
        if let Value::Object(attributes) = &mut with_unknown {
            attributes.insert("id".into(), Value::Unknown);
        }
        let bytes = to_message_pack(&with_unknown, &value_type).unwrap();
        let decoded = from_message_pack(&bytes, &value_type).unwrap();
        assert!(decoded.contains_unknown());
        assert_eq!(decoded, with_unknown);
    }

    /// cty MessagePack of `value` in a `dynamic` slot holding `concrete`.
    fn dynamic_message_pack(concrete: &serde_json::Value, value: rmpv::Value) -> Vec<u8> {
        let mut bytes = Vec::new();
        rmpv::encode::write_value(
            &mut bytes,
            &rmpv::Value::Array(vec![
                rmpv::Value::Binary(serde_json::to_vec(concrete).unwrap()),
                value,
            ]),
        )
        .unwrap();
        bytes
    }

    #[test]
    fn dynamic_type_errors_do_not_expose_provider_supplied_labels() {
        let secret = format!("private-dynamic-key-{}", uuid::Uuid::new_v4());
        let concrete = json!(["object", {&secret: "string"}]);
        let invalid_json = json!({"type": concrete, "value": true});
        let invalid_pack = dynamic_message_pack(&concrete, rmpv::Value::Boolean(true));
        let invalid_value = Value::typed(
            Type::Object(BTreeMap::from([(secret.clone(), Type::String)])),
            Value::List(Vec::new()),
        );
        let errors = [
            from_json_bytes(&serde_json::to_vec(&invalid_json).unwrap(), &Type::Dynamic)
                .unwrap_err(),
            from_message_pack(&invalid_pack, &Type::Dynamic).unwrap_err(),
            invalid_value.to_state_json(&Type::Dynamic).unwrap_err(),
            to_message_pack(&invalid_value, &Type::Dynamic).unwrap_err(),
        ];
        for error in errors {
            let text = error.to_string();
            assert!(text.contains("object"), "{text}");
            assert!(!text.contains(&secret), "{text}");
        }
        for specification in [
            json!(secret),
            json!([secret, "string"]),
            json!([secret]),
            json!({"unexpected": secret}),
        ] {
            let error = Type::from_json(&specification).unwrap_err().to_string();
            assert!(!error.contains(&secret), "{error}");
        }
    }

    #[test]
    fn dynamic_values_keep_their_concrete_type_through_every_encoding() {
        let strings = || {
            rmpv::Value::Array(vec![
                rmpv::Value::String("a".into()),
                rmpv::Value::String("b".into()),
            ])
        };
        let cases = [
            (json!(["list", "string"]), strings()),
            (json!(["set", "string"]), strings()),
            (
                json!(["map", "string"]),
                rmpv::Value::Map(vec![(
                    rmpv::Value::String("k".into()),
                    rmpv::Value::String("v".into()),
                )]),
            ),
            (json!("string"), rmpv::Value::Nil),
            (
                json!("number"),
                rmpv::Value::Ext(UNKNOWN_EXTENSION, Vec::new()),
            ),
        ];
        for (concrete, value) in cases {
            let bytes = dynamic_message_pack(&concrete, value);
            let decoded = from_message_pack(&bytes, &Type::Dynamic).unwrap();
            let Value::Typed(typed) = &decoded else {
                panic!("{concrete}: not typed: {decoded:?}");
            };
            assert_eq!(typed.value_type.to_json(), concrete);
            // MessagePack goes back byte for byte: a list stays a list.
            assert_eq!(
                to_message_pack(&decoded, &Type::Dynamic).unwrap(),
                bytes,
                "{concrete}"
            );
            if decoded.contains_unknown() {
                continue;
            }
            // State JSON records the same type, and reads back the same.
            let state = decoded.to_state_json(&Type::Dynamic).unwrap();
            assert_eq!(state["type"], concrete);
            let reread =
                from_json_bytes(&serde_json::to_vec(&state).unwrap(), &Type::Dynamic).unwrap();
            assert_eq!(reread, decoded, "{concrete}");
            assert_eq!(
                to_message_pack(&reread, &Type::Dynamic).unwrap(),
                bytes,
                "{concrete}"
            );
        }
    }

    #[test]
    fn dynamic_values_compare_by_type_and_value() {
        let list = Value::typed(
            Type::List(Box::new(Type::String)),
            Value::List(vec![Value::String("a".into())]),
        );
        let tuple = Value::typed(
            Type::Tuple(vec![Type::String]),
            Value::List(vec![Value::String("a".into())]),
        );
        assert!(raw_equal(&list, &list.clone(), &Type::Dynamic));
        assert!(!raw_equal(&list, &tuple, &Type::Dynamic));
        // Configuration infers a tuple for a JSON list.
        let configured = Value::from_configuration_json(&json!(["a"]), &Type::Dynamic).unwrap();
        assert!(raw_equal(&tuple, &configured, &Type::Dynamic));
        assert!(!raw_equal(&list, &configured, &Type::Dynamic));
        // Nulls are equal whatever type they carry.
        assert!(raw_equal(
            &Value::typed(Type::String, Value::Null),
            &Value::Null,
            &Type::Dynamic
        ));
        let unknown = Value::typed(Type::String, Value::Unknown);
        assert!(raw_equal(&unknown, &unknown.clone(), &Type::Dynamic));
        assert!(!semantically_equal(
            &unknown,
            &unknown.clone(),
            &Type::Dynamic
        ));
        assert!(unknown.is_unknown());
        assert!(unknown.contains_unknown());
        assert_eq!(
            unknown.unknown_as_null(),
            Value::typed(Type::String, Value::Null)
        );
    }

    #[test]
    fn set_configuration_holds_each_value_once() {
        let value_type = Type::from_json(&json!(["object", {
            "tags": ["set", "string"],
            "order": ["list", "string"],
        }]))
        .unwrap();
        let value = Value::from_configuration_json(
            &json!({"tags": ["a", "b", "a"], "order": ["a", "b", "a"]}),
            &value_type,
        )
        .unwrap();
        assert_eq!(
            value.attribute("tags"),
            Some(&Value::List(vec![
                Value::String("a".into()),
                Value::String("b".into())
            ]))
        );
        let Some(Value::List(order)) = value.attribute("order") else {
            panic!("order is not a list");
        };
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn message_pack_objects_encode_every_attribute_in_sorted_order() {
        let value_type =
            Type::from_json(&json!(["object", {"beta": "string", "alpha": "string"}])).unwrap();
        let bytes = to_message_pack(&Value::Object(BTreeMap::new()), &value_type).unwrap();
        // fixmap(2), fixstr(5) "alpha" -> nil, fixstr(4) "beta" -> nil
        let expected: Vec<u8> =
            [&[0x82, 0xa5][..], b"alpha", &[0xc0, 0xa4], b"beta", &[0xc0]].concat();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn semantic_equality_ignores_set_order_but_not_list_order() {
        let value_type = Type::from_json(&json!(["object", {
            "tags": ["set", "string"],
            "order": ["list", "string"],
            "count": "number",
        }]))
        .unwrap();
        let original = Value::from_configuration_json(
            &json!({"tags": ["blue", "green", "green"], "order": ["first", "second"], "count": 1}),
            &value_type,
        )
        .unwrap();
        let reordered_set = Value::from_configuration_json(
            &json!({"tags": ["green", "blue", "green"], "order": ["first", "second"], "count": 1.0}),
            &value_type,
        )
        .unwrap();
        let reordered_list = Value::from_configuration_json(
            &json!({"tags": ["blue", "green", "green"], "order": ["second", "first"], "count": 1}),
            &value_type,
        )
        .unwrap();
        assert!(semantically_equal(&original, &reordered_set, &value_type));
        assert!(!semantically_equal(&original, &reordered_list, &value_type));
        // Configuration never holds duplicates in a set, but a provider's
        // value might: element counts must match.
        let strings = |items: &[&str]| {
            Value::List(
                items
                    .iter()
                    .map(|item| Value::String((*item).into()))
                    .collect(),
            )
        };
        let set_type = Type::Set(Box::new(Type::String));
        assert!(!semantically_equal(
            &strings(&["blue", "green", "green"]),
            &strings(&["blue", "blue", "green"]),
            &set_type
        ));
        assert!(semantically_equal(
            &strings(&["blue", "green", "green"]),
            &strings(&["green", "blue", "green"]),
            &set_type
        ));
        assert!(!semantically_equal(
            &Value::Unknown,
            &Value::Unknown,
            &value_type
        ));
    }

    #[test]
    fn empty_message_pack_is_null() {
        assert_eq!(from_message_pack(&[], &Type::String).unwrap(), Value::Null);
    }

    #[test]
    fn state_json_wraps_dynamic_values() {
        let value_type = object_type();
        let value = Value::from_configuration_json(
            &json!({"id": "example", "extra": "hello"}),
            &value_type,
        )
        .unwrap();
        let state = value.to_state_json(&value_type).unwrap();
        assert_eq!(state["extra"], json!({"value": "hello", "type": "string"}));
        assert_eq!(state["length"], serde_json::Value::Null);
    }

    #[test]
    fn state_json_refuses_unknowns() {
        assert!(Value::Unknown.to_state_json(&Type::String).is_err());
    }

    #[test]
    fn raw_equality_matches_unknowns_that_semantic_equality_does_not() {
        let value_type = Type::from_json(&json!(["object", {
            "id": "string",
            "tags": ["set", "number"],
        }]))
        .unwrap();
        let left = Value::Object(BTreeMap::from([
            ("id".to_string(), Value::Unknown),
            (
                "tags".to_string(),
                Value::List(vec![Value::Number(1.into()), Value::Number(2.into())]),
            ),
        ]));
        let right = Value::Object(BTreeMap::from([
            ("id".to_string(), Value::Unknown),
            (
                "tags".to_string(),
                Value::List(vec![
                    Value::Number(2.into()),
                    Value::Number(Number::from_f64(1.0).unwrap()),
                ]),
            ),
        ]));
        assert!(raw_equal(&left, &right, &value_type));
        assert!(!semantically_equal(&left, &right, &value_type));
        assert!(!raw_equal(&Value::Unknown, &Value::Null, &Type::String));
    }

    #[test]
    fn json_values_unwrap_dynamic_wrappers_at_any_depth() {
        let value_type = Type::from_json(&json!(["object", {
            "settings": ["object", {"extra": "dynamic"}],
            "items": ["list", "dynamic"],
        }]))
        .unwrap();
        let decoded = from_json_bytes(
            br#"{
                "settings": {"extra": {"value": {"a": [1, true]}, "type": ["object", {"a": ["tuple", ["number", "bool"]]}]}},
                "items": [{"value": "x", "type": "string"}]
            }"#,
            &value_type,
        )
        .unwrap();
        assert_eq!(
            decoded
                .attribute("settings")
                .and_then(|settings| settings.attribute("extra"))
                .map(Value::untyped),
            Some(&Value::Object(BTreeMap::from([(
                "a".to_string(),
                Value::List(vec![Value::Number(1.into()), Value::Boolean(true)])
            )])))
        );
        assert_eq!(
            decoded.attribute("items"),
            Some(&Value::List(vec![Value::typed(
                Type::String,
                Value::String("x".into())
            )]))
        );
        // Round trip through state JSON, which wraps them again.
        let state = decoded.to_state_json(&value_type).unwrap();
        let bytes = serde_json::to_vec(&state).unwrap();
        assert_eq!(from_json_bytes(&bytes, &value_type).unwrap(), decoded);
    }

    #[test]
    fn json_values_without_a_dynamic_wrapper_are_refused_without_their_content() {
        let error = from_json_bytes(br#"{"extra": "hunter2"}"#, &object_type())
            .unwrap_err()
            .to_string();
        assert!(error.contains("wrapper"), "{error}");
        assert!(!error.contains("hunter2"), "{error}");
        let error = from_json_bytes(br#"{"length": "hunter2"}"#, &object_type())
            .unwrap_err()
            .to_string();
        assert!(!error.contains("hunter2"), "{error}");
    }

    #[test]
    fn types_resolve_along_attribute_paths() {
        let value_type = object_type();
        assert_eq!(
            value_type.at_path(&[PathStep::Attribute("tags".into()), PathStep::Index(0)]),
            Some(Type::String)
        );
        assert_eq!(
            value_type.at_path(&[
                PathStep::Attribute("keepers".into()),
                PathStep::Key("a".into())
            ]),
            Some(Type::String)
        );
        assert_eq!(
            value_type.at_path(&[PathStep::Attribute("extra".into()), PathStep::Index(3)]),
            Some(Type::Dynamic)
        );
        assert_eq!(
            value_type.at_path(&[PathStep::Attribute("nope".into())]),
            None
        );
    }
}
