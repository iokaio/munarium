// SPDX-License-Identifier: Apache-2.0
//! `artifact@1` — canonical encoding and hashing.
//!
//! The Rust half of `server/contract/datastore/canonicalization.schema.json`.
//! The contract's identity vectors are the arbiter: where this and
//! `server/contract/datastore/canonicalize.py` disagree, the vectors are right and
//! both implementations are suspect until one is shown to violate the schema.
//!
//! RFC 8785 (JCS), with one restriction: **floating-point numbers are refused**
//! rather than formatted. JCS number canonicalization is ES6
//! `Number::toString`, and it is the part implementations get wrong — shortest
//! round-trip formatting, exponent thresholds, negative zero. Nothing in a
//! `BuildSpec`, a plan or a manifest needs a float: dimensions, byte lengths,
//! counts and positions are integers, and a genuine ratio is carried as a
//! decimal STRING at a declared scale. Removing a whole class of divergence is
//! worth more than the convenience it costs.

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::Error;

/// Canonical UTF-8 bytes of a value under `artifact@1`.
pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    let json = serde_json::to_value(value).map_err(|e| Error::Canonical(e.to_string()))?;
    let mut out = String::new();
    write_canonical(&json, &mut out, "$")?;
    Ok(out.into_bytes())
}

/// Lowercase-hex SHA-256 of the canonical bytes. No prefix, no trailing
/// newline, no length prefix, no domain separator — see the schema's
/// `hashing.input`.
pub fn canonical_sha256<T: Serialize>(value: &T) -> Result<String, Error> {
    let bytes = canonical_bytes(value)?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

fn write_canonical(v: &serde_json::Value, out: &mut String, path: &str) -> Result<(), Error> {
    use serde_json::Value as V;
    match v {
        V::Null => out.push_str("null"),
        V::Bool(true) => out.push_str("true"),
        V::Bool(false) => out.push_str("false"),
        V::Number(n) => {
            // The one place this profile is stricter than JCS.
            let i = n.as_i64().ok_or_else(|| {
                Error::Canonical(format!(
                    "{path}: artifact@1 forbids non-integer and out-of-range numbers (got {n}); \
                     carry a ratio as a decimal STRING at a declared scale"
                ))
            })?;
            out.push_str(&i.to_string());
        }
        V::String(s) => write_string(s, out),
        V::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out, &format!("{path}[{i}]"))?;
            }
            out.push(']');
        }
        V::Object(map) => {
            // JCS sorts members by UTF-16 code unit. Comparing big-endian
            // UTF-16 byte sequences is exactly that order, and it differs from
            // Rust's native `str` (code-point) ordering above the BMP — which
            // is why this is not a plain sort_by_key on the &str.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by_key(|k| {
                k.encode_utf16()
                    .flat_map(u16::to_be_bytes)
                    .collect::<Vec<u8>>()
            });
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_canonical(&map[*k], out, &format!("{path}.{k}"))?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// RFC 8785 string escaping: only `"`, `\` and C0 controls are escaped, with
/// the short forms where they exist and lowercase `\u00XX` otherwise. Non-ASCII
/// is emitted literally — the schema forbids Unicode normalization, so the
/// UTF-8 bytes of a string are hashed as given.
fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{09}' => out.push_str("\\t"),
            '\u{0a}' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{0d}' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn canon(v: serde_json::Value) -> String {
        String::from_utf8(canonical_bytes(&v).unwrap()).unwrap()
    }

    #[test]
    fn object_members_sort_and_whitespace_disappears() {
        assert_eq!(canon(json!({"b": 1, "a": 2})), r#"{"a":2,"b":1}"#);
        assert_eq!(
            canon(json!({"z": {"y": 1, "x": 2}})),
            r#"{"z":{"x":2,"y":1}}"#
        );
    }

    /// The property the whole scheme rests on: the same content written in a
    /// different member order is the same bytes, hence the same artifact_id.
    #[test]
    fn member_order_does_not_change_the_hash() {
        let a = json!({"kind": "collection", "id": "col-1"});
        let b = json!({"id": "col-1", "kind": "collection"});
        assert_eq!(canonical_sha256(&a).unwrap(), canonical_sha256(&b).unwrap());
    }

    /// ARRAY order is content, unlike member order. Canonicalization must not
    /// sort it: a probe list or a source list is a sequence.
    #[test]
    fn array_order_does_change_the_hash() {
        let a = json!(["x", "y"]);
        let b = json!(["y", "x"]);
        assert_ne!(canonical_sha256(&a).unwrap(), canonical_sha256(&b).unwrap());
    }

    #[test]
    fn floats_are_refused_and_the_message_says_what_to_do() {
        let err = canonical_bytes(&json!({"ratio": 0.5})).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("forbids"), "{msg}");
        assert!(msg.contains("decimal STRING"), "{msg}");
    }

    #[test]
    fn integers_pass_and_keep_their_sign() {
        assert_eq!(
            canon(json!({"n": -12, "m": 0, "p": 148213})),
            r#"{"m":0,"n":-12,"p":148213}"#
        );
    }

    #[test]
    fn strings_escape_minimally_and_do_not_normalize_unicode() {
        assert_eq!(canon(json!("a\"b\\c")), r#""a\"b\\c""#);
        assert_eq!(canon(json!("tab\there")), r#""tab\there""#);
        // A C0 control IS escaped, as lowercase \u00XX. RFC 8785 escapes the
        // control range even though it leaves everything above it literal, so
        // "minimal escaping" means minimal, not absent.
        assert_eq!(canon(json!("bell\u{7}")), r#""bell\u0007""#);
        // Accented and CJK text is emitted literally, not escaped and not
        // normalized -- the same refusal to fold that canon@1 makes.
        assert_eq!(canon(json!("Café 東京")), "\"Café 東京\"");
    }

    #[test]
    fn nested_empties_are_stable() {
        assert_eq!(
            canon(json!({"a": [], "b": {}, "c": null})),
            r#"{"a":[],"b":{},"c":null}"#
        );
    }
}
