// SPDX-License-Identifier: Apache-2.0
//! The `pgoutput` logical-replication protocol, version 1.
//!
//! # Why this exists rather than `test_decoding`
//!
//! Postgres ships two ways to read a logical replication slot through plain
//! SQL. `test_decoding` produces text a person can read and **honours nothing**:
//! measured on a real PostgreSQL 16 on 2026-08-30, a role restricted to EMEA by
//! a row policy and denied the `secret` column entirely saw, through the slot,
//! the AMER row, the APAC row, and `secret[text]:'topsecret'`. Logical decoding
//! reads WAL, and WAL is written before any policy is consulted.
//!
//! `pgoutput` is the plugin the built-in replication uses, and it applies the
//! **publication's** row filter and column list while decoding. The same
//! measurement through `pgoutput` with
//! `FOR TABLE crm.opportunities (id, name, amount, region) WHERE (region = 'EMEA')`
//! returned only the EMEA rows, and `secret` did not appear even in the
//! Relation message. That is the whole reason this decoder exists: it is the
//! only Postgres CDC path on which the ENGINE, not Matrix, applies the policy.
//!
//! # What the protocol does not let you have all at once
//!
//! Also measured, and both are engine refusals rather than opinions:
//!
//! * A publication **column list must cover the replica identity**, so a table
//!   with `REPLICA IDENTITY FULL` cannot have a column list that withholds
//!   anything — "Column list used by the publication does not cover the replica
//!   identity".
//! * Every column a publication's `WHERE` names **must be part of the replica
//!   identity**, or updates and deletes on the table are refused outright —
//!   "Column used in the publication WHERE expression is not part of the
//!   replica identity".
//!
//! So a source that needs a row filter on a non-key column AND a column list
//! that withholds one needs `REPLICA IDENTITY USING INDEX` over a unique index
//! covering the key and the filter's columns. That combination works; the
//! adapter refuses the ones that do not, naming what to change.
//!
//! # Two decode decisions
//!
//! A message tag this build does not model is a REFUSAL, not a skip. A
//! `TRUNCATE` in particular means every row of a table went away, and silently
//! ignoring it would leave a collection reporting rows the source no longer
//! has.
//!
//! An `unchanged TOAST` datum (`'u'`) is also a refusal. It means "this column
//! did not change and is not in the stream" — the value is genuinely absent,
//! and sealing a NULL in its place would put a value in evidence that the
//! source never held.

use munarium_matrix_core::{Refusal, RefusalClass};

type Result<T> = std::result::Result<T, Refusal>;

/// One column, as a `Relation` message describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelColumn {
    /// Bit 1 means the column is part of the replica identity key.
    pub flags: u8,
    pub name: String,
    pub type_oid: u32,
    pub type_modifier: i32,
}

impl RelColumn {
    pub fn is_key(&self) -> bool {
        self.flags & 1 == 1
    }
}

/// One cell in a tuple.
///
/// The three states are genuinely different and the difference is load-bearing:
/// `Null` is a value the source holds, `Text` is a value the source holds, and
/// `UnchangedToast` is a value the source holds and did NOT send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Datum {
    Null,
    UnchangedToast,
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tuple(pub Vec<Datum>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Begin {
        final_lsn: u64,
        xid: u32,
    },
    Commit {
        commit_lsn: u64,
        end_lsn: u64,
    },
    Relation {
        id: u32,
        namespace: String,
        name: String,
        columns: Vec<RelColumn>,
    },
    Insert {
        relation: u32,
        new: Tuple,
    },
    Update {
        relation: u32,
        /// The old tuple, when the replica identity changed or the identity is
        /// FULL. Absent when the identity columns did not move, which is the
        /// common case and is why an update's key comes from `new`.
        old: Option<Tuple>,
        new: Tuple,
    },
    Delete {
        relation: u32,
        /// The replica identity of the row that went away. With
        /// `REPLICA IDENTITY DEFAULT` or `USING INDEX` this carries ONLY the
        /// identity columns; every other column is `Null` because the engine
        /// did not send it, not because the row held one.
        key: Tuple,
    },
    /// Recognised and carried so the caller can refuse it by name rather than
    /// failing on an unknown tag.
    Truncate {
        relations: Vec<u32>,
    },
    /// `Origin` and `Type` carry no row data. They are modelled so a stream
    /// containing one is not mistaken for a corrupt stream.
    Origin,
    Type,
}

fn malformed(what: &str) -> Refusal {
    Refusal::new(
        RefusalClass::Unavailable,
        "cdc_stream_malformed",
        format!("the replication stream ended mid-{what}; this build cannot decode it"),
    )
}

/// A big-endian cursor. Every read is bounds-checked and returns a typed
/// refusal, because a panic here would be a panic inside a database read.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn u8(&mut self, what: &str) -> Result<u8> {
        let v = *self.bytes.get(self.at).ok_or_else(|| malformed(what))?;
        self.at += 1;
        Ok(v)
    }
    fn u16(&mut self, what: &str) -> Result<u16> {
        let s = self.take(2, what)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self, what: &str) -> Result<u32> {
        let s = self.take(4, what)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn i32(&mut self, what: &str) -> Result<i32> {
        Ok(self.u32(what)? as i32)
    }
    fn u64(&mut self, what: &str) -> Result<u64> {
        let s = self.take(8, what)?;
        let mut b = [0u8; 8];
        b.copy_from_slice(s);
        Ok(u64::from_be_bytes(b))
    }
    fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or_else(|| malformed(what))?;
        let s = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| malformed(what))?;
        self.at = end;
        Ok(s)
    }
    /// A NUL-terminated string.
    fn cstr(&mut self, what: &str) -> Result<String> {
        let start = self.at;
        while *self.bytes.get(self.at).ok_or_else(|| malformed(what))? != 0 {
            self.at += 1;
        }
        let s = String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned();
        self.at += 1;
        Ok(s)
    }

    fn tuple(&mut self) -> Result<Tuple> {
        let n = self.u16("tuple")?;
        let mut cells = Vec::with_capacity(n as usize);
        for _ in 0..n {
            cells.push(match self.u8("tuple cell")? {
                b'n' => Datum::Null,
                b'u' => Datum::UnchangedToast,
                b't' => {
                    let len = self.u32("cell length")? as usize;
                    let bytes = self.take(len, "cell value")?;
                    // Proto v1 sends every value in its TEXT representation, so
                    // `900000.50` arrives with its trailing zero and no float
                    // is ever constructed. That is the property the whole
                    // evidence identity rests on.
                    Datum::Text(String::from_utf8_lossy(bytes).into_owned())
                }
                other => {
                    return Err(Refusal::new(
                        RefusalClass::Unavailable,
                        "cdc_stream_malformed",
                        format!(
                            "the replication stream used tuple datum kind '{}', which this \
                             build does not model",
                            other as char
                        ),
                    ))
                }
            });
        }
        Ok(Tuple(cells))
    }
}

/// Decode one `pgoutput` protocol-version-1 message.
pub fn decode(bytes: &[u8]) -> Result<Message> {
    let mut c = Cursor::new(bytes);
    let tag = c.u8("message tag")?;
    Ok(match tag {
        b'B' => {
            let final_lsn = c.u64("begin lsn")?;
            let _commit_ts = c.u64("begin timestamp")?;
            let xid = c.u32("begin xid")?;
            Message::Begin { final_lsn, xid }
        }
        b'C' => {
            let _flags = c.u8("commit flags")?;
            let commit_lsn = c.u64("commit lsn")?;
            let end_lsn = c.u64("commit end lsn")?;
            let _commit_ts = c.u64("commit timestamp")?;
            Message::Commit {
                commit_lsn,
                end_lsn,
            }
        }
        b'R' => {
            let id = c.u32("relation id")?;
            let namespace = c.cstr("relation namespace")?;
            let name = c.cstr("relation name")?;
            let _replica_identity = c.u8("replica identity")?;
            let n = c.u16("relation column count")?;
            let mut columns = Vec::with_capacity(n as usize);
            for _ in 0..n {
                columns.push(RelColumn {
                    flags: c.u8("column flags")?,
                    name: c.cstr("column name")?,
                    type_oid: c.u32("column type")?,
                    type_modifier: c.i32("column type modifier")?,
                });
            }
            Message::Relation {
                id,
                namespace,
                name,
                columns,
            }
        }
        b'I' => {
            let relation = c.u32("insert relation")?;
            let marker = c.u8("insert tuple marker")?;
            if marker != b'N' {
                return Err(malformed("insert"));
            }
            Message::Insert {
                relation,
                new: c.tuple()?,
            }
        }
        b'U' => {
            let relation = c.u32("update relation")?;
            let mut old = None;
            let mut marker = c.u8("update tuple marker")?;
            // `K` is the old replica identity, `O` the whole old row. Either
            // may precede the new tuple; neither is present when the identity
            // columns did not change, which is the ordinary case.
            if marker == b'K' || marker == b'O' {
                old = Some(c.tuple()?);
                marker = c.u8("update tuple marker")?;
            }
            if marker != b'N' {
                return Err(malformed("update"));
            }
            Message::Update {
                relation,
                old,
                new: c.tuple()?,
            }
        }
        b'D' => {
            let relation = c.u32("delete relation")?;
            let marker = c.u8("delete tuple marker")?;
            if marker != b'K' && marker != b'O' {
                return Err(malformed("delete"));
            }
            Message::Delete {
                relation,
                key: c.tuple()?,
            }
        }
        b'T' => {
            let n = c.u32("truncate count")?;
            let _options = c.u8("truncate options")?;
            let mut relations = Vec::with_capacity(n as usize);
            for _ in 0..n {
                relations.push(c.u32("truncate relation")?);
            }
            Message::Truncate { relations }
        }
        b'O' => Message::Origin,
        b'Y' => Message::Type,
        other => {
            return Err(Refusal::new(
                RefusalClass::NotCovered,
                "cdc_unsupported_message",
                format!(
                    "the replication stream carried a '{}' message, which this build does not \
                     model. It is refused rather than skipped: a message this decoder cannot \
                     read may be a change the collection would otherwise miss",
                    other as char
                ),
            ))
        }
    })
}

/// `0/1565A78` → the 64-bit LSN.
///
/// Postgres prints an LSN as two 32-bit halves in hex separated by a slash, and
/// the halves are NOT the same width in text — `0/1565A78` has a seven-digit
/// low half. Parsing it as a string comparison would order `0/9` after
/// `0/10`, which is why every comparison in the adapter goes through this.
pub fn parse_lsn(text: &str) -> Option<u64> {
    let (hi, lo) = text.trim().split_once('/')?;
    let hi = u64::from_str_radix(hi, 16).ok()?;
    let lo = u64::from_str_radix(lo, 16).ok()?;
    Some((hi << 32) | lo)
}

/// The `X/Y` form Postgres accepts back as a `pg_lsn`.
pub fn format_lsn(lsn: u64) -> String {
    format!("{:X}/{:X}", lsn >> 32, lsn & 0xFFFF_FFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every message in this test comes from a REAL PostgreSQL 16, captured on
    /// 2026-08-30. A decoder tested against its own author's idea of the
    /// protocol tests the author.
    fn captured() -> Vec<(u64, Vec<u8>)> {
        include_str!("../tests/captured-pgoutput.txt")
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
            .map(|l| {
                let (lsn, hex) = l.split_once('|').expect("lsn|hex");
                let bytes = (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                    .collect();
                (parse_lsn(lsn).expect("an lsn"), bytes)
            })
            .collect()
    }

    #[test]
    fn an_lsn_round_trips_and_orders_numerically() {
        assert_eq!(parse_lsn("0/1565A78"), Some(0x1565A78));
        assert_eq!(format_lsn(0x1565A78), "0/1565A78");
        assert_eq!(parse_lsn("1/0"), Some(1u64 << 32));
        assert_eq!(format_lsn(1u64 << 32), "1/0");
        // The reason this is not a string comparison anywhere in the adapter.
        assert!(parse_lsn("0/10").unwrap() > parse_lsn("0/9").unwrap());
        assert!("0/10" < "0/9", "which the string comparison gets backwards");
    }

    #[test]
    fn the_captured_relation_carries_only_the_published_columns() {
        let msgs: Vec<Message> = captured()
            .iter()
            .map(|(_, b)| decode(b).expect("decodes"))
            .collect();
        let relation = msgs
            .iter()
            .find_map(|m| match m {
                Message::Relation {
                    namespace,
                    name,
                    columns,
                    ..
                } => Some((namespace.clone(), name.clone(), columns.clone())),
                _ => None,
            })
            .expect("a relation message");
        assert_eq!(relation.0, "crm");
        assert_eq!(relation.1, "opportunities");
        let names: Vec<&str> = relation.2.iter().map(|c| c.name.as_str()).collect();
        // The denied column is not merely filtered out of the rows — it is not
        // in the SHAPE. That is what makes the publication's column list a
        // policy rather than a suggestion.
        assert_eq!(names, vec!["id", "name", "amount", "region"]);
        assert!(!names.contains(&"secret"));
        // `id` and `region` are the replica identity (a unique index over
        // both), which is what lets the publication filter on `region` while
        // still permitting updates and deletes.
        assert!(relation.2[0].is_key(), "id is part of the identity");
        assert!(relation.2[3].is_key(), "region is part of the identity");
        assert!(!relation.2[1].is_key());
    }

    #[test]
    fn an_exact_decimal_arrives_as_text_with_its_trailing_zero() {
        let inserts: Vec<Tuple> = captured()
            .iter()
            .filter_map(|(_, b)| match decode(b) {
                Ok(Message::Insert { new, .. }) => Some(new),
                _ => None,
            })
            .collect();
        assert_eq!(inserts.len(), 2, "two inserts in the capture");
        assert_eq!(inserts[0].0[0], Datum::Text("30".into()));
        assert_eq!(
            inserts[0].0[2],
            Datum::Text("900000.50".into()),
            "proto v1 sends values as text, so no float is ever constructed"
        );
        // NULL is not the empty string, and the protocol distinguishes them by
        // a datum kind rather than by an empty payload.
        assert_eq!(inserts[1].0[2], Datum::Null);
        assert_ne!(inserts[1].0[2], Datum::Text(String::new()));
    }

    #[test]
    fn an_update_carries_the_new_row_and_a_delete_carries_only_the_identity() {
        let msgs: Vec<Message> = captured()
            .iter()
            .map(|(_, b)| decode(b).expect("decodes"))
            .collect();

        let update = msgs
            .iter()
            .find_map(|m| match m {
                Message::Update { old, new, .. } => Some((old.clone(), new.clone())),
                _ => None,
            })
            .expect("an update");
        assert!(
            update.0.is_none(),
            "the identity columns did not move, so no old tuple is sent"
        );
        assert_eq!(update.1 .0[2], Datum::Text("900000.75".into()));

        let deletes: Vec<Tuple> = msgs
            .iter()
            .filter_map(|m| match m {
                Message::Delete { key, .. } => Some(key.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deletes.len(), 2);
        // Only `id` and `region` — the replica identity. `name` and `amount`
        // are NULL because the engine did not send them, which is a different
        // fact from the row having held nulls, and the adapter must not render
        // them as values.
        assert_eq!(deletes[0].0[0], Datum::Text("30".into()));
        assert_eq!(deletes[0].0[1], Datum::Null);
        assert_eq!(deletes[0].0[2], Datum::Null);
        assert_eq!(deletes[0].0[3], Datum::Text("EMEA".into()));
    }

    #[test]
    fn the_row_the_publications_filter_excludes_is_not_in_the_stream_at_all() {
        // The AMER row was UPDATEd while the capture was open. The engine
        // applied the publication's WHERE during decoding, so there is no
        // message for it — which is the entire reason this path can carry a
        // policy and `test_decoding` cannot.
        let texts: Vec<String> = captured()
            .iter()
            .flat_map(|(_, b)| match decode(b) {
                Ok(Message::Insert { new, .. }) | Ok(Message::Update { new, .. }) => new.0,
                Ok(Message::Delete { key, .. }) => key.0,
                _ => vec![],
            })
            .filter_map(|d| match d {
                Datum::Text(t) => Some(t),
                _ => None,
            })
            .collect();
        assert!(!texts.iter().any(|t| t == "AMER"), "{texts:?}");
        assert!(!texts.iter().any(|t| t == "IgnoredAmer"), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains("topsecret")), "{texts:?}");
    }

    #[test]
    fn a_truncate_is_decoded_so_it_can_be_refused_by_name() {
        // Built by hand: the capture has no TRUNCATE, and staging one would
        // destroy the fixture the other tests read. What matters is that the
        // tag is MODELLED — an unmodelled tag would fail as a malformed stream
        // and read like corruption rather than like an unsupported operation.
        let bytes = [b'T', 0, 0, 0, 1, 0, 0, 0, 0x40, 0x01];
        assert_eq!(
            decode(&bytes).expect("decodes"),
            Message::Truncate {
                relations: vec![0x4001]
            }
        );
    }

    #[test]
    fn an_unknown_message_is_refused_rather_than_skipped() {
        // A message this decoder cannot read may be a change the collection
        // would otherwise miss, so it is never stepped over.
        let err = decode(b"Z\x00\x00\x00\x01").expect_err("an unknown tag refuses");
        assert_eq!(err.code, "cdc_unsupported_message");
        assert_eq!(err.class, RefusalClass::NotCovered);
        assert!(err.message.contains('Z'), "{}", err.message);
    }

    #[test]
    fn a_truncated_stream_refuses_instead_of_panicking() {
        // Every read in the cursor is bounds-checked, because a panic here
        // would be a panic inside a database read.
        for cut in 1..12 {
            let full = &captured()[1].1;
            let short = &full[..cut.min(full.len())];
            let err = decode(short).expect_err("a short message cannot decode");
            assert_eq!(err.code, "cdc_stream_malformed");
        }
    }
}
