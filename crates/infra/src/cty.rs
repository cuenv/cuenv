//! Minimal implementation of Terraform's `cty` type system.
//!
//! Terraform providers exchange values as msgpack (and state as JSON)
//! using a *type-directed* encoding: the same bytes mean different things
//! depending on the schema type they are decoded against. This module
//! implements exactly enough of `github.com/zclconf/go-cty` to:
//!
//! - parse the JSON type specifications found in provider schemas,
//! - convert CUE-evaluated JSON configuration into typed values,
//! - encode/decode values to and from cty msgpack, including unknown
//!   values and `DynamicPseudoType` wrappers, and
//! - render values as cty JSON for durable state storage.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Number;

use crate::error::{InfraError, Result};

/// msgpack extension code cty uses for unknown values.
const UNKNOWN_EXT: i8 = 0;

/// A cty type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// `bool`
    Bool,
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
    /// Returns [`InfraError::Codec`] when the specification is malformed.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self> {
        let json: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| InfraError::codec(format!("invalid cty type JSON: {e}")))?;
        Self::from_json(&json)
    }

    /// Parse a cty JSON type specification from an already-decoded value.
    ///
    /// # Errors
    ///
    /// Returns [`InfraError::Codec`] when the specification is malformed.
    pub fn from_json(json: &serde_json::Value) -> Result<Self> {
        use serde_json::Value as J;
        match json {
            J::String(s) => match s.as_str() {
                "bool" => Ok(Self::Bool),
                "number" => Ok(Self::Number),
                "string" => Ok(Self::String),
                "dynamic" => Ok(Self::Dynamic),
                other => Err(InfraError::codec(format!(
                    "unknown cty primitive '{other}'"
                ))),
            },
            J::Array(parts) => {
                let kind = parts
                    .first()
                    .and_then(J::as_str)
                    .ok_or_else(|| InfraError::codec("cty type array must start with a kind"))?;
                let arg = parts.get(1).ok_or_else(|| {
                    InfraError::codec(format!("cty '{kind}' type missing argument"))
                })?;
                match kind {
                    "list" => Ok(Self::List(Box::new(Self::from_json(arg)?))),
                    "set" => Ok(Self::Set(Box::new(Self::from_json(arg)?))),
                    "map" => Ok(Self::Map(Box::new(Self::from_json(arg)?))),
                    "object" => {
                        let attrs = arg.as_object().ok_or_else(|| {
                            InfraError::codec("cty object type attributes must be a JSON object")
                        })?;
                        let attrs = attrs
                            .iter()
                            .map(|(k, v)| Ok((k.clone(), Self::from_json(v)?)))
                            .collect::<Result<_>>()?;
                        Ok(Self::Object(attrs))
                    }
                    "tuple" => {
                        let elems = arg.as_array().ok_or_else(|| {
                            InfraError::codec("cty tuple type elements must be a JSON array")
                        })?;
                        Ok(Self::Tuple(
                            elems.iter().map(Self::from_json).collect::<Result<_>>()?,
                        ))
                    }
                    other => Err(InfraError::codec(format!(
                        "unknown cty type kind '{other}'"
                    ))),
                }
            }
            other => Err(InfraError::codec(format!(
                "unsupported cty type specification: {other}"
            ))),
        }
    }

    /// Render this type as a cty JSON type specification.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::{Value as J, json};
        match self {
            Self::Bool => J::from("bool"),
            Self::Number => J::from("number"),
            Self::String => J::from("string"),
            Self::Dynamic => J::from("dynamic"),
            Self::List(t) => json!(["list", t.to_json()]),
            Self::Set(t) => json!(["set", t.to_json()]),
            Self::Map(t) => json!(["map", t.to_json()]),
            Self::Object(attrs) => {
                let attrs: serde_json::Map<String, J> = attrs
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_json()))
                    .collect();
                json!(["object", attrs])
            }
            Self::Tuple(elems) => {
                json!(["tuple", elems.iter().map(Self::to_json).collect::<Vec<_>>()])
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
    Bool(bool),
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
            Self::Object(attrs) => attrs.values().any(Self::contains_unknown),
            _ => false,
        }
    }

    /// Look up an attribute of an object value.
    #[must_use]
    pub fn attr(&self, name: &str) -> Option<&Self> {
        match self {
            Self::Object(attrs) => attrs.get(name),
            _ => None,
        }
    }

    /// Convert CUE-evaluated JSON into a value conforming to `ty`.
    ///
    /// Object attributes missing from the JSON become null, mirroring how
    /// Terraform decodes a configuration block with unset arguments.
    ///
    /// # Errors
    ///
    /// Returns [`InfraError::Config`] when the JSON cannot be converted.
    pub fn from_config_json(json: &serde_json::Value, ty: &Type) -> Result<Self> {
        from_config_json(json, ty, &mut Vec::new())
    }

    /// Render this value as cty JSON (the format Terraform stores state in).
    ///
    /// # Errors
    ///
    /// Returns [`InfraError::Codec`] if the value contains unknowns, which
    /// can never be persisted.
    pub fn to_state_json(&self, ty: &Type) -> Result<serde_json::Value> {
        to_state_json(self, ty)
    }

    /// Render this value as plain JSON for display. Unknown values render
    /// as the string `(known after apply)`.
    #[must_use]
    pub fn to_display_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Self::Null => J::Null,
            Self::Unknown => J::from("(known after apply)"),
            Self::Bool(b) => J::Bool(*b),
            Self::Number(n) => J::Number(n.clone()),
            Self::String(s) => J::String(s.clone()),
            Self::List(items) => J::Array(items.iter().map(Self::to_display_json).collect()),
            Self::Object(attrs) => J::Object(
                attrs
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_display_json()))
                    .collect(),
            ),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => f.write_str("(known after apply)"),
            Self::Null => f.write_str("null"),
            other => write!(f, "{}", other.to_display_json()),
        }
    }
}

/// Compare two values of type `ty`, treating set elements as unordered.
///
/// Providers may return set elements in any order, so positional equality
/// would report perpetual diffs.
#[must_use]
pub fn semantically_equal(a: &Value, b: &Value, ty: &Type) -> bool {
    match (a, b, ty) {
        (Value::Unknown, _, _) | (_, Value::Unknown, _) => false,
        (Value::List(xs), Value::List(ys), Type::Set(elem)) => {
            xs.len() == ys.len()
                && xs.iter().all(|x| {
                    let want = xs.iter().filter(|o| semantically_equal(o, x, elem)).count();
                    let have = ys.iter().filter(|y| semantically_equal(y, x, elem)).count();
                    want == have
                })
        }
        (Value::List(xs), Value::List(ys), Type::List(elem)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|(x, y)| semantically_equal(x, y, elem))
        }
        (Value::List(xs), Value::List(ys), Type::Tuple(elems)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .zip(elems)
                    .all(|((x, y), t)| semantically_equal(x, y, t))
        }
        (Value::Object(xs), Value::Object(ys), Type::Map(elem)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .all(|(k, x)| ys.get(k).is_some_and(|y| semantically_equal(x, y, elem)))
        }
        (Value::Object(xs), Value::Object(ys), Type::Object(atys)) => atys.iter().all(|(k, t)| {
            semantically_equal(
                xs.get(k).unwrap_or(&Value::Null),
                ys.get(k).unwrap_or(&Value::Null),
                t,
            )
        }),
        (Value::Number(x), Value::Number(y), _) => {
            x == y
                || x.as_f64()
                    .zip(y.as_f64())
                    .is_some_and(|(x, y)| x.total_cmp(&y).is_eq())
        }
        (a, b, Type::Dynamic) => {
            let ty = infer_type(a);
            ty == infer_type(b) && semantically_equal(a, b, &ty)
        }
        (a, b, _) => a == b,
    }
}

fn path_string(path: &[String]) -> String {
    if path.is_empty() {
        "<root>".to_string()
    } else {
        path.join(".")
    }
}

fn from_config_json(json: &serde_json::Value, ty: &Type, path: &mut Vec<String>) -> Result<Value> {
    use serde_json::Value as J;
    if json.is_null() {
        return Ok(Value::Null);
    }
    let mismatch = |expected: &str, path: &[String]| {
        InfraError::config(format!(
            "{}: expected {expected}, got {json}",
            path_string(path)
        ))
    };
    match ty {
        Type::Dynamic => Ok(infer_from_json(json)),
        Type::Bool => match json {
            J::Bool(b) => Ok(Value::Bool(*b)),
            J::String(s) if s == "true" => Ok(Value::Bool(true)),
            J::String(s) if s == "false" => Ok(Value::Bool(false)),
            _ => Err(mismatch("bool", path)),
        },
        Type::Number => match json {
            J::Number(n) => Ok(Value::Number(n.clone())),
            J::String(s) => s
                .parse::<Number>()
                .map(Value::Number)
                .map_err(|_| mismatch("number", path)),
            _ => Err(mismatch("number", path)),
        },
        Type::String => match json {
            J::String(s) => Ok(Value::String(s.clone())),
            J::Number(n) => Ok(Value::String(n.to_string())),
            J::Bool(b) => Ok(Value::String(b.to_string())),
            _ => Err(mismatch("string", path)),
        },
        Type::List(elem) | Type::Set(elem) => {
            let items = json.as_array().ok_or_else(|| mismatch("list", path))?;
            let mut out = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                path.push(i.to_string());
                out.push(from_config_json(item, elem, path)?);
                path.pop();
            }
            Ok(Value::List(out))
        }
        Type::Tuple(elems) => {
            let items = json.as_array().ok_or_else(|| mismatch("tuple", path))?;
            if items.len() != elems.len() {
                return Err(mismatch(
                    &format!("tuple of {} elements", elems.len()),
                    path,
                ));
            }
            let mut out = Vec::with_capacity(items.len());
            for (i, (item, ety)) in items.iter().zip(elems).enumerate() {
                path.push(i.to_string());
                out.push(from_config_json(item, ety, path)?);
                path.pop();
            }
            Ok(Value::List(out))
        }
        Type::Map(elem) => {
            let obj = json.as_object().ok_or_else(|| mismatch("map", path))?;
            let mut out = BTreeMap::new();
            for (k, v) in obj {
                path.push(k.clone());
                out.insert(k.clone(), from_config_json(v, elem, path)?);
                path.pop();
            }
            Ok(Value::Object(out))
        }
        Type::Object(attrs) => {
            let obj = json.as_object().ok_or_else(|| mismatch("object", path))?;
            if let Some(extra) = obj.keys().find(|k| !attrs.contains_key(*k)) {
                return Err(InfraError::config(format!(
                    "{}: unsupported argument '{extra}'",
                    path_string(path)
                )));
            }
            let mut out = BTreeMap::new();
            for (name, aty) in attrs {
                let value = match obj.get(name) {
                    Some(v) => {
                        path.push(name.clone());
                        let value = from_config_json(v, aty, path)?;
                        path.pop();
                        value
                    }
                    None => Value::Null,
                };
                out.insert(name.clone(), value);
            }
            Ok(Value::Object(out))
        }
    }
}

fn infer_from_json(json: &serde_json::Value) -> Value {
    use serde_json::Value as J;
    match json {
        J::Null => Value::Null,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => Value::Number(n.clone()),
        J::String(s) => Value::String(s.clone()),
        J::Array(items) => Value::List(items.iter().map(infer_from_json).collect()),
        J::Object(obj) => Value::Object(
            obj.iter()
                .map(|(k, v)| (k.clone(), infer_from_json(v)))
                .collect(),
        ),
    }
}

/// Infer the concrete type of a value held in a `dynamic` slot.
fn infer_type(value: &Value) -> Type {
    match value {
        Value::Null | Value::Unknown => Type::Dynamic,
        Value::Bool(_) => Type::Bool,
        Value::Number(_) => Type::Number,
        Value::String(_) => Type::String,
        Value::List(items) => Type::Tuple(items.iter().map(infer_type).collect()),
        Value::Object(attrs) => Type::Object(
            attrs
                .iter()
                .map(|(k, v)| (k.clone(), infer_type(v)))
                .collect(),
        ),
    }
}

fn to_state_json(value: &Value, ty: &Type) -> Result<serde_json::Value> {
    use serde_json::Value as J;
    match (value, ty) {
        (Value::Null, _) => Ok(J::Null),
        (Value::Unknown, _) => Err(InfraError::codec(
            "cannot persist a value that is unknown after apply",
        )),
        (v, Type::Dynamic) => {
            let concrete = infer_type(v);
            Ok(serde_json::json!({
                "value": to_state_json(v, &concrete)?,
                "type": concrete.to_json(),
            }))
        }
        (Value::Bool(b), _) => Ok(J::Bool(*b)),
        (Value::Number(n), _) => Ok(J::Number(n.clone())),
        (Value::String(s), _) => Ok(J::String(s.clone())),
        (Value::List(items), Type::List(elem) | Type::Set(elem)) => Ok(J::Array(
            items
                .iter()
                .map(|v| to_state_json(v, elem))
                .collect::<Result<_>>()?,
        )),
        (Value::List(items), Type::Tuple(elems)) => Ok(J::Array(
            items
                .iter()
                .zip(elems)
                .map(|(v, t)| to_state_json(v, t))
                .collect::<Result<_>>()?,
        )),
        (Value::Object(attrs), Type::Map(elem)) => Ok(J::Object(
            attrs
                .iter()
                .map(|(k, v)| Ok((k.clone(), to_state_json(v, elem)?)))
                .collect::<Result<_>>()?,
        )),
        (Value::Object(attrs), Type::Object(atys)) => Ok(J::Object(
            atys.iter()
                .map(|(k, t)| {
                    let v = attrs.get(k).unwrap_or(&Value::Null);
                    Ok((k.clone(), to_state_json(v, t)?))
                })
                .collect::<Result<_>>()?,
        )),
        (v, t) => Err(InfraError::codec(format!(
            "value {v} does not conform to type {}",
            t.to_json()
        ))),
    }
}

// ---------------------------------------------------------------------------
// msgpack
// ---------------------------------------------------------------------------

/// Encode a value as cty msgpack against `ty`.
///
/// # Errors
///
/// Returns [`InfraError::Codec`] if the value does not conform to the type.
pub fn to_msgpack(value: &Value, ty: &Type) -> Result<Vec<u8>> {
    let encoded = encode(value, ty)?;
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &encoded)
        .map_err(|e| InfraError::codec(format!("msgpack encode failed: {e}")))?;
    Ok(buf)
}

/// Decode cty msgpack against `ty`. Empty input decodes to null.
///
/// # Errors
///
/// Returns [`InfraError::Codec`] if the bytes are not valid for the type.
pub fn from_msgpack(bytes: &[u8], ty: &Type) -> Result<Value> {
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    let mut cursor = bytes;
    let raw = rmpv::decode::read_value(&mut cursor)
        .map_err(|e| InfraError::codec(format!("msgpack decode failed: {e}")))?;
    decode(&raw, ty)
}

fn encode(value: &Value, ty: &Type) -> Result<rmpv::Value> {
    use rmpv::Value as M;
    match (value, ty) {
        (Value::Null, _) => Ok(M::Nil),
        (Value::Unknown, _) => Ok(M::Ext(UNKNOWN_EXT, Vec::new())),
        (v, Type::Dynamic) => {
            let concrete = infer_type(v);
            let type_json = serde_json::to_vec(&concrete.to_json())
                .map_err(|e| InfraError::codec(format!("encode dynamic type: {e}")))?;
            Ok(M::Array(vec![M::Binary(type_json), encode(v, &concrete)?]))
        }
        (Value::Bool(b), Type::Bool) => Ok(M::Boolean(*b)),
        (Value::Number(n), Type::Number) => Ok(encode_number(n)),
        (Value::String(s), Type::String) => Ok(M::String(s.clone().into())),
        (Value::List(items), Type::List(elem) | Type::Set(elem)) => Ok(M::Array(
            items
                .iter()
                .map(|v| encode(v, elem))
                .collect::<Result<_>>()?,
        )),
        (Value::List(items), Type::Tuple(elems)) if items.len() == elems.len() => Ok(M::Array(
            items
                .iter()
                .zip(elems)
                .map(|(v, t)| encode(v, t))
                .collect::<Result<_>>()?,
        )),
        (Value::Object(attrs), Type::Map(elem)) => Ok(M::Map(
            attrs
                .iter()
                .map(|(k, v)| Ok((M::String(k.clone().into()), encode(v, elem)?)))
                .collect::<Result<_>>()?,
        )),
        (Value::Object(attrs), Type::Object(atys)) => {
            if let Some(extra) = attrs.keys().find(|k| !atys.contains_key(*k)) {
                return Err(InfraError::codec(format!("unexpected attribute '{extra}'")));
            }
            Ok(M::Map(
                atys.iter()
                    .map(|(k, t)| {
                        let v = attrs.get(k).unwrap_or(&Value::Null);
                        Ok((M::String(k.clone().into()), encode(v, t)?))
                    })
                    .collect::<Result<_>>()?,
            ))
        }
        (v, t) => Err(InfraError::codec(format!(
            "value {v} does not conform to type {}",
            t.to_json()
        ))),
    }
}

fn encode_number(n: &Number) -> rmpv::Value {
    if let Some(i) = n.as_i64() {
        rmpv::Value::from(i)
    } else if let Some(u) = n.as_u64() {
        rmpv::Value::from(u)
    } else if let Some(f) = n.as_f64() {
        rmpv::Value::F64(f)
    } else {
        rmpv::Value::String(n.to_string().into())
    }
}

fn decode(raw: &rmpv::Value, ty: &Type) -> Result<Value> {
    use rmpv::Value as M;
    match (raw, ty) {
        (M::Nil, _) => Ok(Value::Null),
        (M::Ext(..), _) => Ok(Value::Unknown),
        (M::Array(parts), Type::Dynamic) if parts.len() == 2 => {
            let type_bytes: &[u8] = match &parts[0] {
                M::Binary(b) => b,
                M::String(s) => s.as_bytes(),
                other => {
                    return Err(InfraError::codec(format!(
                        "dynamic value type must be bytes, got {other}"
                    )));
                }
            };
            let concrete = Type::from_json_bytes(type_bytes)?;
            decode(&parts[1], &concrete)
        }
        (M::Boolean(b), Type::Bool) => Ok(Value::Bool(*b)),
        (M::Integer(i), Type::Number) => i
            .as_i64()
            .map(Number::from)
            .or_else(|| i.as_u64().map(Number::from))
            .map(Value::Number)
            .ok_or_else(|| InfraError::codec("integer out of range")),
        (M::F64(f), Type::Number) => float_number(*f),
        (M::F32(f), Type::Number) => float_number(f64::from(*f)),
        (M::String(s), Type::Number) => s
            .as_str()
            .and_then(|s| s.parse::<Number>().ok())
            .map(Value::Number)
            .ok_or_else(|| InfraError::codec("invalid number string")),
        (M::String(s), Type::String) => s
            .as_str()
            .map(|s| Value::String(s.to_string()))
            .ok_or_else(|| InfraError::codec("string is not valid UTF-8")),
        (M::Array(items), Type::List(elem) | Type::Set(elem)) => Ok(Value::List(
            items
                .iter()
                .map(|v| decode(v, elem))
                .collect::<Result<_>>()?,
        )),
        (M::Array(items), Type::Tuple(elems)) if items.len() == elems.len() => Ok(Value::List(
            items
                .iter()
                .zip(elems)
                .map(|(v, t)| decode(v, t))
                .collect::<Result<_>>()?,
        )),
        (M::Map(entries), Type::Map(elem)) => Ok(Value::Object(
            entries
                .iter()
                .map(|(k, v)| Ok((map_key(k)?, decode(v, elem)?)))
                .collect::<Result<_>>()?,
        )),
        (M::Map(entries), Type::Object(atys)) => {
            let mut out: BTreeMap<String, Value> =
                atys.keys().map(|k| (k.clone(), Value::Null)).collect();
            for (k, v) in entries {
                let key = map_key(k)?;
                let aty = atys
                    .get(&key)
                    .ok_or_else(|| InfraError::codec(format!("unexpected attribute '{key}'")))?;
                out.insert(key, decode(v, aty)?);
            }
            Ok(Value::Object(out))
        }
        (other, t) => Err(InfraError::codec(format!(
            "msgpack value {other} does not match type {}",
            t.to_json()
        ))),
    }
}

fn float_number(f: f64) -> Result<Value> {
    Number::from_f64(f)
        .map(Value::Number)
        .ok_or_else(|| InfraError::codec(format!("non-finite number {f}")))
}

fn map_key(key: &rmpv::Value) -> Result<String> {
    key.as_str()
        .map(str::to_string)
        .ok_or_else(|| InfraError::codec(format!("map key must be a string, got {key}")))
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
        let ty = object_type();
        assert_eq!(Type::from_json(&ty.to_json()).unwrap(), ty);
    }

    #[test]
    fn config_json_fills_missing_attributes_with_null() {
        let value = Value::from_config_json(&json!({"length": 2}), &object_type()).unwrap();
        assert_eq!(value.attr("length"), Some(&Value::Number(2.into())));
        assert_eq!(value.attr("id"), Some(&Value::Null));
        assert_eq!(value.attr("keepers"), Some(&Value::Null));
    }

    #[test]
    fn config_json_rejects_unknown_arguments() {
        let err = Value::from_config_json(&json!({"nope": 1}), &object_type()).unwrap_err();
        assert!(
            err.to_string().contains("unsupported argument 'nope'"),
            "{err}"
        );
    }

    #[test]
    fn config_json_converts_primitives_like_terraform() {
        let value =
            Value::from_config_json(&json!({"id": 5, "length": "3"}), &object_type()).unwrap();
        assert_eq!(value.attr("id"), Some(&Value::String("5".into())));
        assert_eq!(value.attr("length"), Some(&Value::Number(3.into())));
    }

    #[test]
    fn msgpack_round_trips_including_dynamic_and_unknown() {
        let ty = object_type();
        let value = Value::from_config_json(
            &json!({
                "id": "abc",
                "length": 2.5,
                "keepers": {"a": "b"},
                "tags": ["x", "y"],
                "extra": {"nested": [1, true]},
            }),
            &ty,
        )
        .unwrap();
        let bytes = to_msgpack(&value, &ty).unwrap();
        assert_eq!(from_msgpack(&bytes, &ty).unwrap(), value);

        let mut with_unknown = value;
        if let Value::Object(attrs) = &mut with_unknown {
            attrs.insert("id".into(), Value::Unknown);
        }
        let bytes = to_msgpack(&with_unknown, &ty).unwrap();
        let decoded = from_msgpack(&bytes, &ty).unwrap();
        assert!(decoded.contains_unknown());
        assert_eq!(decoded, with_unknown);
    }

    #[test]
    fn msgpack_objects_encode_every_attribute_in_sorted_order() {
        let ty = Type::from_json(&json!(["object", {"b": "string", "a": "string"}])).unwrap();
        let bytes = to_msgpack(&Value::Object(BTreeMap::new()), &ty).unwrap();
        // fixmap(2), "a" -> nil, "b" -> nil
        assert_eq!(bytes, vec![0x82, 0xa1, b'a', 0xc0, 0xa1, b'b', 0xc0]);
    }

    #[test]
    fn semantic_equality_ignores_set_order_but_not_list_order() {
        let ty = Type::from_json(&json!(["object", {
            "tags": ["set", "string"],
            "order": ["list", "string"],
            "n": "number",
        }]))
        .unwrap();
        let a = Value::from_config_json(
            &json!({"tags": ["a", "b", "b"], "order": ["x", "y"], "n": 1}),
            &ty,
        )
        .unwrap();
        let reordered_set = Value::from_config_json(
            &json!({"tags": ["b", "a", "b"], "order": ["x", "y"], "n": 1.0}),
            &ty,
        )
        .unwrap();
        let reordered_list = Value::from_config_json(
            &json!({"tags": ["a", "b", "b"], "order": ["y", "x"], "n": 1}),
            &ty,
        )
        .unwrap();
        let different_multiset = Value::from_config_json(
            &json!({"tags": ["a", "a", "b"], "order": ["x", "y"], "n": 1}),
            &ty,
        )
        .unwrap();
        assert!(semantically_equal(&a, &reordered_set, &ty));
        assert!(!semantically_equal(&a, &reordered_list, &ty));
        assert!(!semantically_equal(&a, &different_multiset, &ty));
        assert!(!semantically_equal(&Value::Unknown, &Value::Unknown, &ty));
    }

    #[test]
    fn empty_msgpack_is_null() {
        assert_eq!(from_msgpack(&[], &Type::String).unwrap(), Value::Null);
    }

    #[test]
    fn state_json_wraps_dynamic_values() {
        let ty = object_type();
        let value = Value::from_config_json(&json!({"id": "a", "extra": "hello"}), &ty).unwrap();
        let state = value.to_state_json(&ty).unwrap();
        assert_eq!(state["extra"], json!({"value": "hello", "type": "string"}));
        assert_eq!(state["length"], serde_json::Value::Null);
    }

    #[test]
    fn state_json_refuses_unknowns() {
        assert!(Value::Unknown.to_state_json(&Type::String).is_err());
    }
}
