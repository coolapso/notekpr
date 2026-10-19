//! Base64 (RFC 4648, standard alphabet, padded) helpers for binary fields in JSON.

use data_encoding::BASE64;
use serde::{Deserialize, Deserializer, Serializer};

pub fn encode(b: &[u8]) -> String {
    BASE64.encode(b)
}

pub fn decode(s: &str) -> Result<Vec<u8>, data_encoding::DecodeError> {
    BASE64.decode(s.as_bytes())
}

pub fn decode32(s: &str) -> Option<[u8; 32]> {
    decode(s).ok()?.try_into().ok()
}

pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&BASE64.encode(v))
}

pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let s = String::deserialize(d)?;
    BASE64.decode(s.as_bytes()).map_err(serde::de::Error::custom)
}

/// `Option<Vec<u8>>` variant. Use with `#[serde(default, with = "b64::opt")]`.
pub mod opt {
    use super::*;

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => s.serialize_some(&BASE64.encode(b)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        s.map(|s| BASE64.decode(s.as_bytes()).map_err(serde::de::Error::custom)).transpose()
    }
}
