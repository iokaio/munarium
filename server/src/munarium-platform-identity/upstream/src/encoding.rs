// SPDX-License-Identifier: Apache-2.0
//! Bounded encoding for the hub's proposed decision JSON profile.

use crate::error::Error;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) const MAX_BODY: usize = 65536;

pub(crate) fn decode(input: &str) -> Result<Vec<u8>, Error> {
    if input.is_empty() || input.len() > MAX_BODY {
        return Err(Error::InvalidInput);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(input)
        .map_err(|_| Error::InvalidInput)?;
    if URL_SAFE_NO_PAD.encode(&bytes) != input {
        return Err(Error::InvalidInput);
    }
    Ok(bytes)
}

/// Hash exact bytes with SHA-256, using the hub's textual digest spelling.
pub fn digest(input: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(input))
}

pub(crate) fn valid_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value.as_bytes()[7..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

pub(crate) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":._/-".contains(&b))
}

pub(crate) fn canonical(raw: &[u8]) -> Result<Value, Error> {
    if raw.len() > MAX_BODY {
        return Err(Error::InvalidInput);
    }
    let value: Value = serde_json::from_slice(raw).map_err(|_| Error::InvalidInput)?;
    if !value.is_object() {
        return Err(Error::InvalidInput);
    }
    fn check(value: &Value, depth: usize) -> Result<(), Error> {
        match value {
            Value::Object(fields) => {
                if depth >= 16 || fields.keys().any(|k| !k.is_ascii()) {
                    return Err(Error::InvalidInput);
                }
                for child in fields.values() {
                    check(child, depth + 1)?;
                }
            }
            Value::Array(items) => {
                if depth >= 16 {
                    return Err(Error::InvalidInput);
                }
                for child in items {
                    check(child, depth + 1)?;
                }
            }
            Value::Number(number)
                if !number
                    .as_i64()
                    .is_some_and(|n| (-9007199254740991..=9007199254740991).contains(&n)) =>
            {
                return Err(Error::InvalidInput);
            }
            _ => {}
        }
        Ok(())
    }
    check(&value, 0)?;
    // A byte-for-byte comparison also rejects duplicate keys, unnecessary escapes,
    // whitespace and alternate number spellings; never verify normalized bytes.
    if serde_json::to_vec(&value).map_err(|_| Error::InvalidInput)? != raw {
        return Err(Error::InvalidInput);
    }
    Ok(value)
}

pub(crate) fn current(nbf: i64, exp: i64, now: i64) -> bool {
    nbf >= 0
        && now.checked_sub(2).is_some_and(|lower| nbf <= lower)
        && now.checked_add(2).is_some_and(|upper| upper < exp)
}
