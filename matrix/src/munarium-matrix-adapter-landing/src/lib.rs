// SPDX-License-Identifier: Apache-2.0
//! The landing-export adapter.
//!
//! This is the first adapter on purpose: **an immutable export is a real
//! snapshot.** There is no isolation level to reason about, no time-travel
//! window to expire, and no row policy to delegate — the manifest names the
//! files, the files do not change, and the snapshot marker is the manifest id.
//! Every hard question mode A has is answered by construction here, which
//! makes it the right place to prove the sealing and rendering pipeline before
//! a live database is involved.
//!
//! What it deliberately does NOT do: guess. A column whose declared type does
//! not parse is a `schema_drift` refusal, not a best-effort string.

#![forbid(unsafe_code)]
#![allow(clippy::result_large_err)]

use async_trait::async_trait;
use munarium_matrix_adapter::*;
use munarium_matrix_core::checkpoint::{Checkpoint, SyncMode};
use munarium_matrix_core::value::{ColumnType, Value};
use munarium_matrix_core::{Column, Refusal};
use munarium_matrix_types::contract::ChangeKind;
use object_store::ObjectStoreExt as _;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const ADAPTER_VERSION: &str = "landing@0.1.0";

/// The manifest that describes one immutable export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    /// The snapshot marker. Whatever the exporter calls this run.
    pub snapshot_id: String,
    /// Declared column shape. The export does not carry types; the manifest
    /// does, and a mismatch is drift rather than a coercion.
    pub schema: Vec<ManifestColumn>,
    /// Key columns — the record's stable identity.
    pub keys: Vec<String>,
    pub files: Vec<ManifestFile>,
    #[serde(default)]
    pub format: FileFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileFormat {
    #[default]
    Csv,
    /// One JSON object per line.
    Jsonl,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ColumnType,
    #[serde(default)]
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestFile {
    pub path: String,
    /// Declared content hash. Verified before the file is read: an export that
    /// changed under us is not the snapshot the manifest describes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u64>,
}

/// Where the export lives: the local filesystem, or an Azure Blob container
/// read through `object_store` (2026-08-30). The decode path is the same for
/// both — a store hands back bytes, and everything after that is the
/// manifest's business.
#[derive(Debug, Clone)]
pub enum LandingStore {
    File {
        root: PathBuf,
    },
    /// `az://<account>/<container>/<prefix>`, credentials from the ambient
    /// chain (`MicrosoftAzureBuilder::from_env`), which on Container Apps is
    /// the managed identity — the server's live-found rule, kept: `new()`
    /// black-holes against link-local IMDS that Container Apps do not have.
    Azure {
        store: std::sync::Arc<dyn object_store::ObjectStore>,
        account: String,
        container: String,
        prefix: String,
    },
}

pub struct LandingAdapter {
    source_id: String,
    store: LandingStore,
    /// Manifest path relative to the root.
    manifest_path: String,
}

impl LandingAdapter {
    pub fn new_file(source_id: &str, root: impl AsRef<Path>, manifest_path: &str) -> Self {
        Self {
            source_id: source_id.to_string(),
            store: LandingStore::File {
                root: root.as_ref().to_path_buf(),
            },
            manifest_path: manifest_path.to_string(),
        }
    }

    /// An export in an Azure Blob container, read with the process's ambient
    /// identity. `prefix` is the folder inside the container (`crm/`), and the
    /// manifest path is relative to it, exactly as with a file root.
    ///
    /// `AZURE_CLIENT_ID` selects the user-assigned identity when the host has
    /// several; a deployment sets it on the container. No key, no SAS, no
    /// connection string: a landing export is read under the identity that
    /// was GRANTED `Storage Blob Data Reader`, or not at all.
    pub fn new_azure(
        source_id: &str,
        account: &str,
        container: &str,
        prefix: &str,
        manifest_path: &str,
    ) -> Result<Self> {
        if account.trim().is_empty() || container.trim().is_empty() {
            return Err(Refusal::invalid(
                "not_covered",
                "an Azure landing source names its storage account and container",
            ));
        }
        let mut builder = object_store::azure::MicrosoftAzureBuilder::from_env()
            .with_account(account)
            .with_container_name(container);
        if let Ok(id) = std::env::var("AZURE_CLIENT_ID") {
            if !id.trim().is_empty() {
                builder = builder.with_client_id(id);
            }
        }
        let store = builder
            .build()
            .map_err(|e| Refusal::source_unavailable(format!("azure landing store: {e}")))?;
        let prefix = prefix.trim_start_matches('/').to_string();
        let prefix = if prefix.is_empty() || prefix.ends_with('/') {
            prefix
        } else {
            format!("{prefix}/")
        };
        Ok(Self {
            source_id: source_id.to_string(),
            store: LandingStore::Azure {
                store: std::sync::Arc::new(store),
                account: account.to_string(),
                container: container.to_string(),
                prefix,
            },
            manifest_path: manifest_path.to_string(),
        })
    }

    /// Where this adapter reads from, for a probe's detail line. Never a
    /// credential — there is none to print.
    pub fn location(&self) -> String {
        match &self.store {
            LandingStore::File { root } => format!("file://{}", root.display()),
            LandingStore::Azure {
                account,
                container,
                prefix,
                ..
            } => format!("az://{account}/{container}/{prefix}"),
        }
    }

    /// Path traversal defense, shared by both stores: a manifest is data, and
    /// data does not get to name `../../etc/passwd` — or, in a container, a
    /// sibling prefix another source was granted.
    fn checked(rel: &str) -> Result<&str> {
        if rel.contains("..") || Path::new(rel).is_absolute() || rel.starts_with('/') {
            return Err(Refusal::invalid(
                "schema_drift",
                format!("manifest names an unsafe path '{rel}'"),
            ));
        }
        Ok(rel)
    }

    async fn read_bytes(&self, rel: &str) -> Result<Vec<u8>> {
        let rel = Self::checked(rel)?;
        match &self.store {
            LandingStore::File { root } => std::fs::read(root.join(rel))
                .map_err(|e| Refusal::source_unavailable(format!("cannot read '{rel}': {e}"))),
            LandingStore::Azure { store, prefix, .. } => {
                let path = object_store::path::Path::from(format!("{prefix}{rel}"));
                let got = store.get(&path).await.map_err(|e| match e {
                    object_store::Error::NotFound { .. } => {
                        Refusal::source_unavailable(format!("'{rel}' is not in the container"))
                    }
                    other => Refusal::source_unavailable(format!("cannot read '{rel}': {other}")),
                })?;
                let bytes = got.bytes().await.map_err(|e| {
                    Refusal::source_unavailable(format!("cannot read '{rel}': {e}"))
                })?;
                Ok(bytes.to_vec())
            }
        }
    }

    pub async fn load_manifest(&self) -> Result<Manifest> {
        let bytes = self.read_bytes(&self.manifest_path).await?;
        serde_json::from_slice(&bytes)
            .map_err(|e| Refusal::invalid("schema_drift", format!("manifest did not parse: {e}")))
    }

    /// Decode one value under its declared type. No coercion, no guessing.
    fn decode(col: &ManifestColumn, raw: &str) -> Result<Value> {
        // The empty field is NULL for a nullable column and an error for a
        // non-nullable one. CSV cannot distinguish "" from NULL, so the
        // manifest's nullability is what decides — which is exactly why the
        // manifest declares it.
        if raw.is_empty() {
            return if col.nullable {
                Ok(Value::Null)
            } else if col.ty == ColumnType::String {
                Ok(Value::String(String::new()))
            } else {
                Err(Refusal::schema_drift(format!(
                    "column '{}' is not nullable but the export has an empty field",
                    col.name
                )))
            };
        }
        let bad = |what: &str| {
            Refusal::schema_drift(format!(
                "column '{}' declared {} but the export has '{raw}' ({what})",
                col.name, col.ty
            ))
        };
        Ok(match col.ty {
            ColumnType::Bool => match raw.to_lowercase().as_str() {
                "true" | "t" | "1" | "yes" => Value::Bool(true),
                "false" | "f" | "0" | "no" => Value::Bool(false),
                _ => return Err(bad("not a boolean")),
            },
            ColumnType::Int64 => Value::Int64(raw.parse().map_err(|_| bad("not an int64"))?),
            ColumnType::Decimal => {
                let scale = col.scale.ok_or_else(|| {
                    Refusal::schema_drift(format!(
                        "decimal column '{}' has no declared scale",
                        col.name
                    ))
                })?;
                Value::Decimal {
                    value: Decimal::from_str(raw).map_err(|_| bad("not a decimal"))?,
                    scale,
                }
            }
            ColumnType::Float64 => Value::Float64(raw.parse().map_err(|_| bad("not a float"))?),
            ColumnType::String => Value::String(raw.to_string()),
            ColumnType::Date => Value::Date(
                chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
                    .map_err(|_| bad("not a date"))?,
            ),
            ColumnType::TimestampTz => Value::TimestampTz(
                chrono::DateTime::parse_from_rfc3339(raw)
                    .map_err(|_| bad("not RFC 3339"))?
                    .with_timezone(&chrono::Utc),
            ),
            ColumnType::TimestampNaive => Value::TimestampNaive(
                chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
                    .or_else(|_| chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f"))
                    .map_err(|_| bad("not a naive timestamp"))?,
            ),
            ColumnType::Uuid => Value::Uuid(raw.to_lowercase()),
            ColumnType::Json => {
                Value::Json(serde_json::from_str(raw).map_err(|_| bad("not JSON"))?)
            }
            ColumnType::Bytes | ColumnType::Interval | ColumnType::Array => {
                return Err(Refusal::not_covered(format!(
                    "the landing adapter does not decode {} columns",
                    col.ty
                )))
            }
        })
    }

    fn columns(manifest: &Manifest, projection: &[String]) -> Vec<Column> {
        let wanted: Vec<&ManifestColumn> = if projection.is_empty() {
            manifest.schema.iter().collect()
        } else {
            projection
                .iter()
                .filter_map(|p| manifest.schema.iter().find(|c| &c.name == p))
                .collect()
        };
        wanted
            .into_iter()
            .enumerate()
            .map(|(i, c)| Column {
                id: format!("c{i}"),
                name: c.name.clone(),
                ty: c.ty,
                nullable: c.nullable,
                scale: c.scale,
                unit: c.unit.clone(),
                additivity: None,
                key: manifest.keys.contains(&c.name),
                element_type: None,
            })
            .collect()
    }

    /// Read every file in the manifest into typed records.
    async fn read_records(
        &self,
        manifest: &Manifest,
        projection: &[String],
    ) -> Result<Vec<SourceRecord>> {
        let cols = Self::columns(manifest, projection);
        let mut out = Vec::new();

        for file in &manifest.files {
            let bytes = self.read_bytes(&file.path).await?;
            // Integrity first: a file whose bytes moved is not the snapshot the
            // manifest describes, and reading it would seal a lie.
            if let Some(declared) = &file.sha256 {
                use sha2::Digest;
                let actual = hex::encode(sha2::Sha256::digest(&bytes));
                let declared = declared.trim_start_matches("sha256:");
                if actual != declared {
                    return Err(Refusal::schema_drift(format!(
                        "'{}' does not match its declared sha256 — the export changed under the \
                         manifest",
                        file.path
                    )));
                }
            }

            let rows = match manifest.format {
                FileFormat::Csv => Self::rows_from_csv(&bytes, manifest)?,
                FileFormat::Jsonl => Self::rows_from_jsonl(&bytes, manifest)?,
            };

            for row in rows {
                let mut cells = Vec::with_capacity(cols.len());
                for col in &cols {
                    let mc = manifest
                        .schema
                        .iter()
                        .find(|c| c.name == col.name)
                        .expect("column came from the manifest");
                    let raw = row.get(&col.name).map(String::as_str).unwrap_or("");
                    cells.push(Self::decode(mc, raw)?);
                }
                let row_key = manifest
                    .keys
                    .iter()
                    .map(|k| row.get(k).cloned().unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("|");
                out.push(SourceRecord {
                    cells,
                    row_key,
                    event_position: Some(manifest.snapshot_id.clone()),
                    change_kind: ChangeKind::Snapshot,
                });
            }
        }
        Ok(out)
    }

    fn rows_from_csv(
        bytes: &[u8],
        manifest: &Manifest,
    ) -> Result<Vec<std::collections::BTreeMap<String, String>>> {
        let mut rdr = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_reader(bytes);
        let headers: Vec<String> = rdr
            .headers()
            .map_err(|e| Refusal::schema_drift(format!("csv header: {e}")))?
            .iter()
            .map(String::from)
            .collect();
        // Every declared column must be present. A missing column is drift,
        // not an empty value: silently reading NULLs would hide a dropped
        // column behind a plausible-looking result.
        for c in &manifest.schema {
            if !headers.contains(&c.name) {
                return Err(Refusal::schema_drift(format!(
                    "export is missing declared column '{}'",
                    c.name
                )));
            }
        }
        let mut out = Vec::new();
        for rec in rdr.records() {
            let rec = rec.map_err(|e| Refusal::schema_drift(format!("csv row: {e}")))?;
            let mut map = std::collections::BTreeMap::new();
            for (i, h) in headers.iter().enumerate() {
                map.insert(h.clone(), rec.get(i).unwrap_or("").to_string());
            }
            out.push(map);
        }
        Ok(out)
    }

    fn rows_from_jsonl(
        bytes: &[u8],
        manifest: &Manifest,
    ) -> Result<Vec<std::collections::BTreeMap<String, String>>> {
        let text = String::from_utf8_lossy(bytes);
        let mut out = Vec::new();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(line)
                .map_err(|e| Refusal::schema_drift(format!("line {}: {e}", n + 1)))?;
            let mut map = std::collections::BTreeMap::new();
            for c in &manifest.schema {
                let raw = match v.get(&c.name) {
                    None | Some(serde_json::Value::Null) => String::new(),
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                };
                map.insert(c.name.clone(), raw);
            }
            out.push(map);
        }
        Ok(out)
    }
}

#[async_trait]
impl SourceAdapter for LandingAdapter {
    fn kind(&self) -> &'static str {
        "landing"
    }

    fn adapter_version(&self) -> &'static str {
        ADAPTER_VERSION
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // Manifest and snapshot only. There is no watermark on an
            // immutable export and no change feed — declaring either would be
            // a support claim we cannot honour.
            sync_modes: vec![SyncMode::Manifest, SyncMode::Snapshot],
            // The export carries no per-principal policy, so classes must come
            // from separate exports with separate principals.
            policy_strategies: vec![PolicyStrategy::PerClassPrincipals],
            query_contracts: false,
            metric_views: false,
            data_views: false,
            semantic_provider: None,
            dialect: None,
            snapshot_marker: Some("manifest".into()),
            // The customer retains the export; we can always return the bytes,
            // but we cannot re-run a query against a past state.
            replay_level: "sealed_result".into(),
            cancellation: false,
            source_side_limits: false,
        }
    }

    async fn probe(&self) -> Result<ProbeResult> {
        let started = std::time::Instant::now();
        match self.load_manifest().await {
            Ok(m) => Ok(ProbeResult {
                reachable: true,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                detail: Some(format!(
                    "manifest {} with {} file(s) at {}",
                    m.snapshot_id,
                    m.files.len(),
                    self.location()
                )),
            }),
            Err(r) => Ok(ProbeResult {
                reachable: false,
                latency_ms: None,
                detail: Some(r.message),
            }),
        }
    }

    async fn introspect(&self) -> Result<(RolePosture, SchemaFingerprint)> {
        let manifest = self.load_manifest().await?;
        // A landing zone has no role to interrogate: the posture is "read-only
        // by construction", and saying so honestly beats inventing checks that
        // would always pass.
        let posture = RolePosture {
            principal: "landing-export".into(),
            checks: vec![
                PostureCheck::new("read_only", true, true)
                    .with_detail("an immutable export cannot be written through this adapter"),
                PostureCheck::new("not_owner", true, true)
                    .with_detail("no database role is involved"),
            ],
        };
        let tables = vec![TableShape {
            name: self.manifest_path.clone(),
            columns: manifest
                .schema
                .iter()
                .map(|c| ColumnShape {
                    name: c.name.clone(),
                    source_type: c.ty.as_str().to_string(),
                    logical_type: Some(c.ty),
                    nullable: c.nullable,
                })
                .collect(),
            row_security_enabled: false,
        }];
        let fingerprint = SchemaFingerprint::compute(&tables);
        Ok((
            posture,
            SchemaFingerprint {
                fingerprint,
                tables,
            },
        ))
    }

    async fn read_batch(
        &self,
        _entity: &str,
        projection: &[String],
        checkpoint: &Checkpoint,
        read: ReadMode<'_>,
        _identity: &EffectiveIdentity,
        limits: Limits,
    ) -> Result<RecordBatch> {
        let mode = read.mode;
        self.capabilities().require_sync(mode)?;
        let manifest = self.load_manifest().await?;

        // An immutable export is read once per snapshot id. A checkpoint that
        // already names this snapshot means there is nothing new — which is
        // what makes a re-run of an unchanged landing zone free.
        if checkpoint.event_position.as_deref() == Some(manifest.snapshot_id.as_str()) {
            return Ok(RecordBatch {
                records: vec![],
                columns: Self::columns(&manifest, projection),
                next_checkpoint: None,
                excluded: 0,
                snapshot_marker: Some(manifest.snapshot_id.clone()),
            });
        }

        let mut records = self.read_records(&manifest, projection).await?;
        let mut excluded = 0;
        if records.len() as u64 > limits.max_rows {
            excluded = records.len() as u64 - limits.max_rows;
            records.truncate(limits.max_rows as usize);
        }

        let next = Checkpoint {
            source_id: self.source_id.clone(),
            entity: checkpoint.entity.clone(),
            version: checkpoint.version.clone(),
            watermark: None,
            tie_break: None,
            event_position: Some(manifest.snapshot_id.clone()),
            schema_fingerprint: Some(SchemaFingerprint::compute(
                &self.introspect().await?.1.tables,
            )),
        };
        Ok(RecordBatch {
            records,
            columns: Self::columns(&manifest, projection),
            next_checkpoint: Some(next),
            excluded,
            snapshot_marker: Some(manifest.snapshot_id),
        })
    }

    async fn execute(
        &self,
        _statement: &str,
        _parameters: &BoundParameters,
        _identity: &EffectiveIdentity,
        _limits: Limits,
    ) -> Result<ExecutedResult> {
        Err(Refusal::not_covered(
            "the landing adapter serves materialization, not query contracts",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("matrix-landing-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
        fn write(&self, rel: &str, content: &str) {
            std::fs::write(self.dir.join(rel), content).unwrap();
        }
        fn adapter(&self) -> LandingAdapter {
            LandingAdapter::new_file("crm", &self.dir, "manifest.json")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn manifest_json(sha: Option<&str>) -> String {
        format!(
            r#"{{
  "snapshotId": "2026-08-28T00:00:00Z",
  "format": "csv",
  "keys": ["id"],
  "schema": [
    {{ "name": "id", "type": "int64" }},
    {{ "name": "region", "type": "string" }},
    {{ "name": "amount", "type": "decimal", "scale": 2, "unit": "USD" }},
    {{ "name": "closed_at", "type": "date", "nullable": true }}
  ],
  "files": [{{ "path": "opportunities.csv"{} }}]
}}"#,
            sha.map(|s| format!(", \"sha256\": \"{s}\""))
                .unwrap_or_default()
        )
    }

    const CSV: &str = "id,region,amount,closed_at\n1,EMEA,1500.00,2026-06-30\n2,AMER,250.5,\n";

    fn identity() -> EffectiveIdentity {
        EffectiveIdentity {
            class: None,
            credential_ref: None,
            principal: "landing-export".into(),
        }
    }

    fn limits() -> Limits {
        Limits {
            max_rows: 1000,
            max_bytes: 1 << 20,
            timeout_ms: 5000,
        }
    }

    #[tokio::test]
    async fn reads_typed_records_from_an_export() {
        let f = Fixture::new("read");
        f.write("manifest.json", &manifest_json(None));
        f.write("opportunities.csv", CSV);

        let a = f.adapter();
        let cp = Checkpoint::start("crm", "opportunities", "record-documents@1");
        let batch = a
            .read_batch(
                "opportunities",
                &[],
                &cp,
                ReadMode::of(SyncMode::Manifest),
                &identity(),
                limits(),
            )
            .await
            .unwrap();

        assert_eq!(batch.records.len(), 2);
        assert_eq!(
            batch.snapshot_marker.as_deref(),
            Some("2026-08-28T00:00:00Z")
        );
        // Decimal keeps its declared scale; 250.5 becomes 250.50.
        assert_eq!(
            batch.records[1].cells[2].canonical_text().unwrap(),
            "250.50"
        );
        // An empty nullable field is NULL, not the empty string.
        assert!(batch.records[1].cells[3].is_null());
        assert_eq!(batch.records[0].row_key, "1");
    }

    #[tokio::test]
    async fn re_reading_the_same_snapshot_returns_nothing() {
        let f = Fixture::new("resnap");
        f.write("manifest.json", &manifest_json(None));
        f.write("opportunities.csv", CSV);
        let a = f.adapter();

        let cp = Checkpoint::start("crm", "opportunities", "record-documents@1");
        let first = a
            .read_batch(
                "opportunities",
                &[],
                &cp,
                ReadMode::of(SyncMode::Manifest),
                &identity(),
                limits(),
            )
            .await
            .unwrap();
        let next = first.next_checkpoint.expect("a first read advances");

        let second = a
            .read_batch(
                "opportunities",
                &[],
                &next,
                ReadMode::of(SyncMode::Manifest),
                &identity(),
                limits(),
            )
            .await
            .unwrap();
        assert!(
            second.records.is_empty(),
            "an immutable export is read once per snapshot"
        );
    }

    #[tokio::test]
    async fn a_file_that_changed_under_its_manifest_is_refused() {
        let f = Fixture::new("hash");
        let wrong = "0".repeat(64);
        f.write("manifest.json", &manifest_json(Some(&wrong)));
        f.write("opportunities.csv", CSV);

        let cp = Checkpoint::start("crm", "opportunities", "record-documents@1");
        let err = f
            .adapter()
            .read_batch(
                "opportunities",
                &[],
                &cp,
                ReadMode::of(SyncMode::Manifest),
                &identity(),
                limits(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "schema_drift");
        assert!(
            err.message.contains("changed under the manifest"),
            "{}",
            err.message
        );
    }

    #[tokio::test]
    async fn a_missing_declared_column_is_drift_not_an_empty_value() {
        let f = Fixture::new("missing");
        f.write("manifest.json", &manifest_json(None));
        f.write("opportunities.csv", "id,region,amount\n1,EMEA,1500.00\n");
        let cp = Checkpoint::start("crm", "opportunities", "record-documents@1");
        let err = f
            .adapter()
            .read_batch(
                "opportunities",
                &[],
                &cp,
                ReadMode::of(SyncMode::Manifest),
                &identity(),
                limits(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "schema_drift");
        assert!(err.message.contains("closed_at"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_value_that_does_not_match_its_declared_type_is_refused_not_coerced() {
        let f = Fixture::new("type");
        f.write("manifest.json", &manifest_json(None));
        f.write(
            "opportunities.csv",
            "id,region,amount,closed_at\nNOT_A_NUMBER,EMEA,1.00,\n",
        );
        let cp = Checkpoint::start("crm", "opportunities", "record-documents@1");
        let err = f
            .adapter()
            .read_batch(
                "opportunities",
                &[],
                &cp,
                ReadMode::of(SyncMode::Manifest),
                &identity(),
                limits(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "schema_drift");
        assert!(err.message.contains("not an int64"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_manifest_naming_a_path_outside_the_root_is_refused() {
        let f = Fixture::new("traversal");
        f.write(
            "manifest.json",
            &manifest_json(None).replace("opportunities.csv", "../../../etc/passwd"),
        );
        let cp = Checkpoint::start("crm", "opportunities", "record-documents@1");
        let err = f
            .adapter()
            .read_batch(
                "opportunities",
                &[],
                &cp,
                ReadMode::of(SyncMode::Manifest),
                &identity(),
                limits(),
            )
            .await
            .unwrap_err();
        assert!(err.message.contains("unsafe path"), "{}", err.message);
    }

    #[tokio::test]
    async fn an_undeclared_sync_mode_is_refused() {
        let f = Fixture::new("mode");
        f.write("manifest.json", &manifest_json(None));
        f.write("opportunities.csv", CSV);
        let cp = Checkpoint::start("crm", "opportunities", "record-documents@1");
        let err = f
            .adapter()
            // There is no watermark on an immutable export.
            .read_batch(
                "opportunities",
                &[],
                &cp,
                ReadMode::of(SyncMode::Watermark),
                &identity(),
                limits(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.class, munarium_matrix_core::RefusalClass::NotCovered);
    }

    #[tokio::test]
    async fn truncation_is_reported_as_exclusion_not_hidden() {
        let f = Fixture::new("limit");
        f.write("manifest.json", &manifest_json(None));
        f.write("opportunities.csv", CSV);
        let cp = Checkpoint::start("crm", "opportunities", "record-documents@1");
        let batch = f
            .adapter()
            .read_batch(
                "opportunities",
                &[],
                &cp,
                ReadMode::of(SyncMode::Manifest),
                &identity(),
                Limits {
                    max_rows: 1,
                    max_bytes: 1 << 20,
                    timeout_ms: 5000,
                },
            )
            .await
            .unwrap();
        assert_eq!(batch.records.len(), 1);
        assert_eq!(batch.excluded, 1, "the dropped row must be reported (G4)");
    }

    #[tokio::test]
    async fn jsonl_exports_decode_the_same_way() {
        let f = Fixture::new("jsonl");
        f.write(
            "manifest.json",
            &manifest_json(None)
                .replace("\"csv\"", "\"jsonl\"")
                .replace("opportunities.csv", "opportunities.jsonl"),
        );
        f.write(
            "opportunities.jsonl",
            "{\"id\":1,\"region\":\"EMEA\",\"amount\":\"1500.00\",\"closed_at\":\"2026-06-30\"}\n\
             {\"id\":2,\"region\":\"AMER\",\"amount\":\"250.5\",\"closed_at\":null}\n",
        );
        let cp = Checkpoint::start("crm", "opportunities", "record-documents@1");
        let batch = f
            .adapter()
            .read_batch(
                "opportunities",
                &[],
                &cp,
                ReadMode::of(SyncMode::Manifest),
                &identity(),
                limits(),
            )
            .await
            .unwrap();
        assert_eq!(batch.records.len(), 2);
        assert_eq!(
            batch.records[1].cells[2].canonical_text().unwrap(),
            "250.50"
        );
        assert!(batch.records[1].cells[3].is_null());
    }

    #[tokio::test]
    async fn introspect_is_honest_that_there_is_no_role_to_check() {
        let f = Fixture::new("introspect");
        f.write("manifest.json", &manifest_json(None));
        f.write("opportunities.csv", CSV);
        let (posture, fp) = f.adapter().introspect().await.unwrap();
        assert!(posture.ok());
        assert_eq!(posture.principal, "landing-export");
        assert!(fp.fingerprint.starts_with("sha256:"));
        assert_eq!(fp.tables[0].columns.len(), 4);
    }

    #[tokio::test]
    async fn query_contracts_are_not_covered_by_this_adapter() {
        let f = Fixture::new("exec");
        f.write("manifest.json", &manifest_json(None));
        let err = f
            .adapter()
            .execute(
                "SELECT 1",
                &BoundParameters::default(),
                &identity(),
                limits(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.class, munarium_matrix_core::RefusalClass::NotCovered);
    }
}
