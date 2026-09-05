// SPDX-License-Identifier: Apache-2.0
//! `record-documents@1` — turning one source record into one document.
//!
//! Mode A's whole promise rests on this being a *function*: the same record
//! must produce the same bytes on every machine, in every process, forever.
//! Two disciplines make that true, and both are tested:
//!
//! - **LF only.** The renderer writes `\n` and never `\r\n`, whatever the host
//!   platform thinks. (Learned the expensive way: 25,575 artifact
//!   files had to be renormalized because a writer inherited the platform's
//!   line ending and every hash moved.)
//! - **Declared field order.** Fields render in the order the projection
//!   declares, never in the order a driver happened to return them.
//!
//! The document's logical path is the record's stable key, so a re-sync of an
//! unchanged record produces a byte-identical document at the same path, which
//! is what makes the server's bulk-upload manifest diff report "nothing to do".

use crate::result::Column;
use crate::value::Value;

pub const RENDER_VERSION: &str = "record-documents@1";

/// One rendered record document, ready for the server's bulk upload plane.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordDocument {
    /// The logical path: identity AND blob name on the server side.
    pub path: String,
    /// The document body — Markdown, because the server's extractors and the
    /// retrieval chunkers already treat it as first-class text.
    pub body: String,
    /// Static metadata indexed straight into the collection's `doc_meta`.
    pub metadata: Vec<(String, String)>,
}

impl RecordDocument {
    pub fn bytes(&self) -> &[u8] {
        self.body.as_bytes()
    }
}

/// What a record needs to render. Everything here is declared by the
/// `DataSource`; nothing is inferred from the data.
#[derive(Debug, Clone)]
pub struct RenderSpec<'a> {
    /// The entity this record belongs to, e.g. `opportunities`.
    pub entity: &'a str,
    /// Path prefix, normally the source name, so one collection can hold
    /// records from several entities without collision.
    pub prefix: &'a str,
    /// The projection, in declared order. Denied columns are absent by
    /// construction — they were never selected.
    pub columns: &'a [Column],
    /// Which columns form the stable key.
    pub key_columns: &'a [String],
    /// The authorization equivalence class this record's collection carries.
    pub authorization_class: &'a str,
    /// The snapshot marker of the run that produced it.
    pub snapshot_marker: Option<&'a str>,
}

/// Encode one key component so a path is stable and filesystem-safe.
///
/// Percent-encoding rather than a hash: an operator looking at a data room
/// should be able to see *which* record a document is, and a hashed path makes
/// every support question start with a lookup.
fn encode_key_part(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The logical path for a record: `<prefix>/<entity>/<key parts joined by ~>.md`.
pub fn record_path(spec: &RenderSpec<'_>, key_values: &[String]) -> String {
    let key = key_values
        .iter()
        .map(|k| encode_key_part(k))
        .collect::<Vec<_>>()
        .join("~");
    format!("{}/{}/{}.md", spec.prefix, spec.entity, key)
}

/// Render one record.
///
/// Body shape, fixed by `record-documents@1`:
///
/// ```text
/// # <entity> <key>
///
/// | field | value |
/// | --- | --- |
/// | <name> | <canonical text or the empty cell for NULL> |
/// ```
///
/// A NULL renders as an empty cell and a literal `(null)` never appears — a
/// document that says "null" is a document a retrieval query can match on the
/// word.
pub fn render_record(spec: &RenderSpec<'_>, cells: &[Value]) -> RecordDocument {
    let key_values: Vec<String> = spec
        .key_columns
        .iter()
        .filter_map(|k| spec.columns.iter().position(|c| &c.name == k || &c.id == k))
        .map(|i| {
            cells
                .get(i)
                .and_then(|v| v.canonical_text())
                .unwrap_or_default()
        })
        .collect();
    let key_display = key_values.join(" / ");
    let path = record_path(spec, &key_values);

    let mut body = String::new();
    body.push_str("# ");
    body.push_str(spec.entity);
    if !key_display.is_empty() {
        body.push(' ');
        body.push_str(&key_display);
    }
    body.push('\n');
    body.push('\n');
    body.push_str("| field | value |\n| --- | --- |\n");
    for (i, col) in spec.columns.iter().enumerate() {
        let text = cells
            .get(i)
            .and_then(|v| v.canonical_text())
            .unwrap_or_default();
        // A pipe or newline inside a value would break the table; escape both
        // rather than dropping data.
        let safe = text.replace('|', "\\|").replace('\n', "<br>");
        body.push_str("| ");
        body.push_str(&col.name);
        body.push_str(" | ");
        body.push_str(&safe);
        if let Some(u) = &col.unit {
            if !safe.is_empty() {
                body.push(' ');
                body.push_str(u);
            }
        }
        body.push_str(" |\n");
    }

    let mut metadata = vec![
        ("doctype".to_string(), "record".to_string()),
        ("entity".to_string(), spec.entity.to_string()),
        ("class".to_string(), spec.authorization_class.to_string()),
        ("render_version".to_string(), RENDER_VERSION.to_string()),
    ];
    if let Some(m) = spec.snapshot_marker {
        metadata.push(("snapshot_marker".to_string(), m.to_string()));
    }

    RecordDocument {
        path,
        body,
        metadata,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::ColumnType;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn spec_columns() -> Vec<Column> {
        vec![
            Column::new("c0", "id", ColumnType::Int64).key(),
            Column::new("c1", "name", ColumnType::String),
            Column::new("c2", "amount", ColumnType::Decimal)
                .scale(2)
                .unit("USD"),
            Column::new("c3", "closed_at", ColumnType::Date).nullable(),
        ]
    }

    fn spec<'a>(cols: &'a [Column], keys: &'a [String]) -> RenderSpec<'a> {
        RenderSpec {
            entity: "opportunities",
            prefix: "crm",
            columns: cols,
            key_columns: keys,
            authorization_class: "sales-emea",
            snapshot_marker: Some("1024:1024:"),
        }
    }

    fn cells() -> Vec<Value> {
        vec![
            Value::Int64(42),
            Value::String("Acme renewal".into()),
            Value::Decimal {
                value: Decimal::from_str("1500").unwrap(),
                scale: 2,
            },
            Value::Null,
        ]
    }

    #[test]
    fn rendering_is_a_function_of_the_record() {
        let cols = spec_columns();
        let keys = vec!["id".to_string()];
        let a = render_record(&spec(&cols, &keys), &cells());
        let b = render_record(&spec(&cols, &keys), &cells());
        assert_eq!(a, b);
        assert_eq!(a.path, "crm/opportunities/42.md");
    }

    #[test]
    fn the_body_uses_lf_only_whatever_the_host_platform_thinks() {
        let cols = spec_columns();
        let keys = vec!["id".to_string()];
        let doc = render_record(&spec(&cols, &keys), &cells());
        assert!(
            !doc.body.contains('\r'),
            "CRLF would move every artifact hash"
        );
        assert!(doc.body.contains('\n'));
    }

    #[test]
    fn a_null_renders_as_an_empty_cell_never_as_the_word_null() {
        let cols = spec_columns();
        let keys = vec!["id".to_string()];
        let doc = render_record(&spec(&cols, &keys), &cells());
        assert!(doc.body.contains("| closed_at |  |"), "got:\n{}", doc.body);
        assert!(!doc.body.to_lowercase().contains("null"));
    }

    #[test]
    fn units_travel_with_the_value_so_a_number_is_never_bare() {
        let cols = spec_columns();
        let keys = vec!["id".to_string()];
        let doc = render_record(&spec(&cols, &keys), &cells());
        assert!(
            doc.body.contains("| amount | 1500.00 USD |"),
            "got:\n{}",
            doc.body
        );
    }

    #[test]
    fn fields_render_in_declared_order_not_driver_order() {
        let cols = spec_columns();
        let keys = vec!["id".to_string()];
        let doc = render_record(&spec(&cols, &keys), &cells());
        let id_at = doc.body.find("| id |").unwrap();
        let name_at = doc.body.find("| name |").unwrap();
        let amount_at = doc.body.find("| amount |").unwrap();
        assert!(id_at < name_at && name_at < amount_at);
    }

    #[test]
    fn composite_keys_make_one_stable_path() {
        let cols = spec_columns();
        let keys = vec!["id".to_string(), "name".to_string()];
        let doc = render_record(&spec(&cols, &keys), &cells());
        assert_eq!(doc.path, "crm/opportunities/42~Acme%20renewal.md");
    }

    #[test]
    fn a_pipe_in_a_value_cannot_break_the_table() {
        let cols = spec_columns();
        let keys = vec!["id".to_string()];
        let mut c = cells();
        c[1] = Value::String("a | b\nsecond line".into());
        let doc = render_record(&spec(&cols, &keys), &c);
        assert!(
            doc.body.contains("| name | a \\| b<br>second line |"),
            "got:\n{}",
            doc.body
        );
        // Still exactly one table row per column, plus the header row and the
        // `| --- | --- |` separator: 2 + 4. An unescaped pipe or a raw newline
        // would add a row here, which is the whole point of the escaping.
        assert_eq!(
            doc.body.lines().filter(|l| l.starts_with("| ")).count(),
            2 + 4
        );
    }

    #[test]
    fn metadata_carries_the_class_and_the_render_version() {
        let cols = spec_columns();
        let keys = vec!["id".to_string()];
        let doc = render_record(&spec(&cols, &keys), &cells());
        assert!(doc
            .metadata
            .contains(&("class".into(), "sales-emea".into())));
        assert!(doc
            .metadata
            .contains(&("render_version".into(), RENDER_VERSION.into())));
        assert!(doc.metadata.contains(&("doctype".into(), "record".into())));
    }
}
