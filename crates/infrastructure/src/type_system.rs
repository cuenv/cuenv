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

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Number;

use crate::error::{InfrastructureError, Result};

/// MessagePack extension code cty uses for unknown values.
const UNKNOWN_EXTENSION: i8 = 0;

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
                other => Err(InfrastructureError::codec(format!(
                    "unknown cty primitive '{other}'"
                ))),
            },
            Json::Array(parts) => {
                let kind = parts.first().and_then(Json::as_str).ok_or_else(|| {
                    InfrastructureError::codec("cty type array must start with a kind")
                })?;
                let argument = parts.get(1).ok_or_else(|| {
                    InfrastructureError::codec(format!("cty '{kind}' type missing argument"))
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
                    other => Err(InfrastructureError::codec(format!(
                        "unknown cty type kind '{other}'"
                    ))),
                }
            }
            other => Err(InfrastructureError::codec(format!(
                "unsupported cty type specification: {other}"
            ))),
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
#[derive(Debug, Clone, PartialEq)]
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
}

impl Value {
    /// Whether this value is null.
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Whether this value, or anything nested inside it, is unknown.
    #[must_use]
    pub fn contains_unknown(&self) -> bool {
        match self {
            Self::Unknown => true,
            Self::List(items) => items.iter().any(Self::contains_unknown),
            Self::Object(attributes) => attributes.values().any(Self::contains_unknown),
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
            other => other.clone(),
        }
    }

    /// Name this value's kind without revealing its content.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Unknown => "unknown",
            Self::Boolean(_) => "boolean",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::List(_) => "list",
            Self::Object(_) => "object",
        }
    }

    /// Look up an attribute of an object value.
    #[must_use]
    pub fn attribute(&self, name: &str) -> Option<&Self> {
        match self {
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
            Self::Number(number) => Json::Number(number.clone()),
            Self::String(text) => Json::String(text.clone()),
            Self::List(items) => Json::Array(items.iter().map(Self::to_display_json).collect()),
            Self::Object(attributes) => Json::Object(
                attributes
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_display_json()))
                    .collect(),
            ),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => formatter.write_str("(known after apply)"),
            Self::Null => formatter.write_str("null"),
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
    values_equal(left, right, value_type, UnknownComparison::NeverEqual)
}

/// Compare two values of type `value_type` the way cty's `RawEquals` does:
/// like [`semantically_equal`], except that an unknown value equals another
/// unknown value in the same position.
#[must_use]
pub fn raw_equal(left: &Value, right: &Value, value_type: &Type) -> bool {
    values_equal(left, right, value_type, UnknownComparison::EqualToUnknown)
}

/// How [`values_equal`] treats unknown values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnknownComparison {
    /// Unknown values are never equal to anything (cty `Equals`).
    NeverEqual,
    /// Two unknown values are equal to each other (cty `RawEquals`).
    EqualToUnknown,
}

fn values_equal(
    left: &Value,
    right: &Value,
    value_type: &Type,
    unknowns: UnknownComparison,
) -> bool {
    let equal = |left: &Value, right: &Value, value_type: &Type| {
        values_equal(left, right, value_type, unknowns)
    };
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
                && left_elements
                    .iter()
                    .zip(right_elements)
                    .all(|(left_element, right_element)| {
                        equal(left_element, right_element, element_type)
                    })
        }
        (Value::List(left_elements), Value::List(right_elements), Type::Tuple(element_types)) => {
            left_elements.len() == right_elements.len()
                && left_elements
                    .iter()
                    .zip(right_elements)
                    .zip(element_types)
                    .all(|((left_element, right_element), element_type)| {
                        equal(left_element, right_element, element_type)
                    })
        }
        (Value::Object(left_entries), Value::Object(right_entries), Type::Map(element_type)) => {
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
            left_number == right_number
                || left_number.as_f64().zip(right_number.as_f64()).is_some_and(
                    |(left_float, right_float)| left_float.total_cmp(&right_float).is_eq(),
                )
        }
        (left, right, Type::Dynamic) => {
            let value_type = infer_type(left);
            value_type == infer_type(right) && equal(left, right, &value_type)
        }
        (left, right, _) => left == right,
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
            Json::Number(number) => Ok(Value::Number(number.clone())),
            Json::String(text) => text
                .parse::<Number>()
                .map(Value::Number)
                .map_err(|_| mismatch("number", path)),
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
            for (index, (item, element_type)) in items.iter().zip(element_types).enumerate() {
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
                path.push(key.clone());
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
            if let Some(extra) = object.keys().find(|key| !attributes.contains_key(*key)) {
                return Err(InfrastructureError::configuration(format!(
                    "{}: unsupported argument '{extra}'",
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
        Json::Number(number) => Value::Number(number.clone()),
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

/// Infer the concrete type of a value held in a `dynamic` slot.
fn infer_type(value: &Value) -> Type {
    match value {
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
        (Value::Number(number), _) => Ok(Json::Number(number.clone())),
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
            expected_type.to_json()
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
    match (value, value_type) {
        (Value::Null, _) => Ok(MessagePack::Nil),
        (Value::Unknown, _) => Ok(MessagePack::Ext(UNKNOWN_EXTENSION, Vec::new())),
        (dynamic_value, Type::Dynamic) => {
            let concrete = infer_type(dynamic_value);
            let type_json = serde_json::to_vec(&concrete.to_json()).map_err(|error| {
                InfrastructureError::codec(format!("encode dynamic type: {error}"))
            })?;
            Ok(MessagePack::Array(vec![
                MessagePack::Binary(type_json),
                encode(dynamic_value, &concrete)?,
            ]))
        }
        (Value::Boolean(boolean), Type::Boolean) => Ok(MessagePack::Boolean(*boolean)),
        (Value::Number(number), Type::Number) => Ok(encode_number(number)),
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
            if let Some(extra) = attributes
                .keys()
                .find(|name| !attribute_types.contains_key(*name))
            {
                return Err(InfrastructureError::codec(format!(
                    "unexpected attribute '{extra}'"
                )));
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
            expected_type.to_json()
        ))),
    }
}

fn encode_number(number: &Number) -> rmpv::Value {
    if let Some(signed) = number.as_i64() {
        rmpv::Value::from(signed)
    } else if let Some(unsigned) = number.as_u64() {
        rmpv::Value::from(unsigned)
    } else if let Some(float) = number.as_f64() {
        rmpv::Value::F64(float)
    } else {
        rmpv::Value::String(number.to_string().into())
    }
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
            decode(&parts[1], &concrete)
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
            .map(Value::Number)
            .ok_or_else(|| InfrastructureError::codec("invalid number string")),
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
                    InfrastructureError::codec(format!("unexpected attribute '{key}'"))
                })?;
                decoded.insert(key, decode(raw_attribute, attribute_type)?);
            }
            Ok(Value::Object(decoded))
        }
        (other, expected_type) => Err(InfrastructureError::codec(format!(
            "a MessagePack {} does not match type {}",
            message_pack_kind(other),
            expected_type.to_json()
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    let json: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
        InfrastructureError::codec(format!(
            "invalid JSON value ({} at line {}, column {})",
            crate::error::json_error_category(&error),
            error.line(),
            error.column()
        ))
    })?;
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
            value_type.to_json()
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
            from_state_json(inner, &Type::from_json(inner_type)?)
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
            if let Some(extra) = entries
                .keys()
                .find(|key| !attribute_types.contains_key(*key))
            {
                return Err(InfrastructureError::codec(format!(
                    "unexpected attribute '{extra}'"
                )));
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
    Number::from_f64(float)
        .map(Value::Number)
        .ok_or_else(|| InfrastructureError::codec(format!("non-finite number {float}")))
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
            error.to_string().contains("unsupported argument 'nope'"),
            "{error}"
        );
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
        assert_eq!(from_message_pack(&bytes, &value_type).unwrap(), value);

        let mut with_unknown = value;
        if let Value::Object(attributes) = &mut with_unknown {
            attributes.insert("id".into(), Value::Unknown);
        }
        let bytes = to_message_pack(&with_unknown, &value_type).unwrap();
        let decoded = from_message_pack(&bytes, &value_type).unwrap();
        assert!(decoded.contains_unknown());
        assert_eq!(decoded, with_unknown);
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
        let different_multiset = Value::from_configuration_json(
            &json!({"tags": ["blue", "blue", "green"], "order": ["first", "second"], "count": 1}),
            &value_type,
        )
        .unwrap();
        assert!(semantically_equal(&original, &reordered_set, &value_type));
        assert!(!semantically_equal(&original, &reordered_list, &value_type));
        assert!(!semantically_equal(
            &original,
            &different_multiset,
            &value_type
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
                .and_then(|settings| settings.attribute("extra")),
            Some(&Value::Object(BTreeMap::from([(
                "a".to_string(),
                Value::List(vec![Value::Number(1.into()), Value::Boolean(true)])
            )])))
        );
        assert_eq!(
            decoded.attribute("items"),
            Some(&Value::List(vec![Value::String("x".into())]))
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
