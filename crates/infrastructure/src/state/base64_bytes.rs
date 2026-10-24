use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Deserializer, Serializer};

pub fn serialize<Output: Serializer>(
    bytes: &[u8],
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    serializer.serialize_str(&STANDARD.encode(bytes))
}

pub fn deserialize<'input, Input: Deserializer<'input>>(
    deserializer: Input,
) -> Result<Vec<u8>, Input::Error> {
    let encoded = String::deserialize(deserializer)?;
    STANDARD.decode(encoded).map_err(serde::de::Error::custom)
}
