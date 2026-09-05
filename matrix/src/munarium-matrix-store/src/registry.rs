// SPDX-License-Identifier: Apache-2.0
//! The asset registry: apply, resolve, list.
//!
//! The apply rule is the same one the server uses for shapes, and for the same
//! reason: **the same version with different bytes is refused.** Assets are
//! provenance — a sealed artifact names `open-pipeline-by-region@2`, and if
//! that name could come to mean different SQL next week the citation would be
//! a lie. Publishing new content means publishing a new version.

use crate::{MatrixStore, Result, StoreError};

/// `(kind, name, version, yaml, yaml_hash, source_name, created_at)` — the
/// asset row, named so the two read paths below agree on its shape by
/// construction rather than by matching tuple arity at a glance.
type AssetRow = (String, String, i32, String, String, Option<String>, String);
use munarium_matrix_types::{parse_asset, Asset};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq)]
pub struct StoredAsset {
    pub tenant_id: String,
    pub kind: String,
    pub name: String,
    pub version: u32,
    pub yaml: String,
    pub yaml_hash: String,
    pub source_name: Option<String>,
    pub created_at: String,
}

impl StoredAsset {
    pub fn asset_ref(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }

    pub fn parse(&self) -> std::result::Result<Asset, munarium_matrix_types::ParseError> {
        parse_asset(&self.yaml)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyOutcome {
    pub asset_ref: String,
    pub kind: String,
    /// True when the applied bytes were already there. An idempotent re-apply
    /// is a normal part of GitOps, so it is a success, not a conflict.
    pub unchanged: bool,
}

fn hash(yaml: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(yaml.as_bytes())))
}

/// The source an asset belongs to, denormalized for listing and joins.
fn source_of(asset: &Asset) -> Option<String> {
    match asset {
        Asset::DataSource(_) => None,
        Asset::QueryContract(c) => Some(c.spec.source.clone()),
        Asset::ClaimMapping(m) => Some(m.spec.source.clone()),
        Asset::MetricView(m) => Some(m.spec.source.clone()),
        Asset::DataView(m) => Some(m.spec.source.clone()),
    }
}

impl MatrixStore {
    /// Apply one asset. Idempotent for identical bytes; a conflict otherwise.
    pub async fn apply_asset(
        &self,
        tenant: &str,
        asset: &Asset,
        yaml: &str,
    ) -> Result<ApplyOutcome> {
        let meta = asset.metadata();
        let kind = asset.kind();
        let h = hash(yaml);

        let existing: Option<(String,)> = sqlx::query_as(
            "SELECT yaml_hash FROM matrix.assets
              WHERE tenant_id = $1 AND kind = $2 AND name = $3 AND version = $4",
        )
        .bind(tenant)
        .bind(kind)
        .bind(&meta.name)
        .bind(meta.version as i32)
        .fetch_optional(self.pool())
        .await?;

        if let Some((stored,)) = existing {
            if stored == h {
                return Ok(ApplyOutcome {
                    asset_ref: meta.asset_ref(),
                    kind: kind.to_string(),
                    unchanged: true,
                });
            }
            return Err(StoreError::Conflict(format!(
                "{kind} {} is already applied with different content; bump the version \
                 (a version is provenance — sealed evidence cites it)",
                meta.asset_ref()
            )));
        }

        // Two appliers of the same NEW version at once — two conformance
        // scenarios calling `ensure_contract`, or two GitOps agents — both
        // pass the read above and both reach this insert. Until 2026-08-29 the
        // second died on `assets_pkey` as a 500, which the dev smoke measured
        // the first time a contract version was new to a long-lived registry.
        // The loser now inserts nothing and is judged by what the winner
        // stored: identical bytes are "unchanged", different bytes a conflict.
        let inserted = sqlx::query(
            "INSERT INTO matrix.assets (tenant_id, kind, name, version, yaml, yaml_hash, source_name)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT DO NOTHING",
        )
        .bind(tenant)
        .bind(kind)
        .bind(&meta.name)
        .bind(meta.version as i32)
        .bind(yaml)
        .bind(&h)
        .bind(source_of(asset))
        .execute(self.pool())
        .await?
        .rows_affected();
        if inserted == 0 {
            let stored: Option<(String,)> = sqlx::query_as(
                "SELECT yaml_hash FROM matrix.assets
                  WHERE tenant_id = $1 AND kind = $2 AND name = $3 AND version = $4",
            )
            .bind(tenant)
            .bind(kind)
            .bind(&meta.name)
            .bind(meta.version as i32)
            .fetch_optional(self.pool())
            .await?;
            return match stored {
                Some((stored,)) if stored == h => Ok(ApplyOutcome {
                    asset_ref: meta.asset_ref(),
                    kind: kind.to_string(),
                    unchanged: true,
                }),
                _ => Err(StoreError::Conflict(format!(
                    "{kind} {} was applied concurrently with different content; bump the version",
                    meta.asset_ref()
                ))),
            };
        }

        // Earlier versions stay resolvable but stop being "latest".
        sqlx::query(
            "UPDATE matrix.assets SET status = 'superseded', updated_at = now()
              WHERE tenant_id = $1 AND kind = $2 AND name = $3 AND version < $4",
        )
        .bind(tenant)
        .bind(kind)
        .bind(&meta.name)
        .bind(meta.version as i32)
        .execute(self.pool())
        .await?;

        Ok(ApplyOutcome {
            asset_ref: meta.asset_ref(),
            kind: kind.to_string(),
            unchanged: false,
        })
    }

    /// Resolve `name` (latest) or `name@version` (exact).
    pub async fn get_asset(
        &self,
        tenant: &str,
        kind: &str,
        name_or_ref: &str,
    ) -> Result<StoredAsset> {
        let (name, version) = match name_or_ref.split_once('@') {
            Some((n, v)) => (n, v.parse::<i32>().ok()),
            None => (name_or_ref, None),
        };
        let row: Option<AssetRow> = match version {
            Some(v) => {
                sqlx::query_as(
                    "SELECT kind, name, version, yaml, yaml_hash, source_name, created_at::text
                           FROM matrix.assets
                          WHERE tenant_id = $1 AND kind = $2 AND name = $3 AND version = $4",
                )
                .bind(tenant)
                .bind(kind)
                .bind(name)
                .bind(v)
                .fetch_optional(self.pool())
                .await?
            }
            None => {
                sqlx::query_as(
                    "SELECT kind, name, version, yaml, yaml_hash, source_name, created_at::text
                           FROM matrix.assets
                          WHERE tenant_id = $1 AND kind = $2 AND name = $3
                          ORDER BY version DESC LIMIT 1",
                )
                .bind(tenant)
                .bind(kind)
                .bind(name)
                .fetch_optional(self.pool())
                .await?
            }
        };
        let (kind, name, version, yaml, yaml_hash, source_name, created_at) =
            row.ok_or_else(|| StoreError::NotFound {
                kind: "asset",
                id: name_or_ref.to_string(),
            })?;
        Ok(StoredAsset {
            tenant_id: tenant.to_string(),
            kind,
            name,
            version: version as u32,
            yaml,
            yaml_hash,
            source_name,
            created_at,
        })
    }

    /// Every asset of a kind. `latest_only` collapses to one row per name.
    pub async fn list_assets(
        &self,
        tenant: &str,
        kind: Option<&str>,
        latest_only: bool,
    ) -> Result<Vec<StoredAsset>> {
        let sql = if latest_only {
            "SELECT DISTINCT ON (kind, name)
                    kind, name, version, yaml, yaml_hash, source_name, created_at::text
               FROM matrix.assets
              WHERE tenant_id = $1 AND ($2::text IS NULL OR kind = $2)
              ORDER BY kind, name, version DESC"
        } else {
            "SELECT kind, name, version, yaml, yaml_hash, source_name, created_at::text
               FROM matrix.assets
              WHERE tenant_id = $1 AND ($2::text IS NULL OR kind = $2)
              ORDER BY kind, name, version DESC"
        };
        let rows: Vec<AssetRow> = sqlx::query_as(sql)
            .bind(tenant)
            .bind(kind)
            .fetch_all(self.pool())
            .await?;
        Ok(rows
            .into_iter()
            .map(
                |(kind, name, version, yaml, yaml_hash, source_name, created_at)| StoredAsset {
                    tenant_id: tenant.to_string(),
                    kind,
                    name,
                    version: version as u32,
                    yaml,
                    yaml_hash,
                    source_name,
                    created_at,
                },
            )
            .collect())
    }

    /// Pin the allowed values for a parameter at introspect time.
    pub async fn pin_parameter_domain(
        &self,
        tenant: &str,
        contract: &str,
        version: u32,
        parameter: &str,
        values: &[String],
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO matrix.parameter_domains
               (tenant_id, contract_name, contract_version, parameter, values_json)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (tenant_id, contract_name, contract_version, parameter)
             DO UPDATE SET values_json = EXCLUDED.values_json, pinned_at = now()",
        )
        .bind(tenant)
        .bind(contract)
        .bind(version as i32)
        .bind(parameter)
        .bind(serde_json::to_value(values).unwrap_or(serde_json::Value::Null))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn parameter_domain(
        &self,
        tenant: &str,
        contract: &str,
        version: u32,
        parameter: &str,
    ) -> Result<Option<Vec<String>>> {
        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT values_json FROM matrix.parameter_domains
              WHERE tenant_id = $1 AND contract_name = $2 AND contract_version = $3
                AND parameter = $4",
        )
        .bind(tenant)
        .bind(contract)
        .bind(version as i32)
        .bind(parameter)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.and_then(|(v,)| serde_json::from_value(v).ok()))
    }

    /// Record the observed schema fingerprint for an entity.
    pub async fn record_fingerprint(
        &self,
        tenant: &str,
        source: &str,
        entity: &str,
        fingerprint: &str,
        columns: &serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO matrix.schema_fingerprints
               (tenant_id, source_name, entity, fingerprint, columns_json)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (tenant_id, source_name, entity)
             DO UPDATE SET fingerprint = EXCLUDED.fingerprint,
                           columns_json = EXCLUDED.columns_json,
                           observed_at = now()",
        )
        .bind(tenant)
        .bind(source)
        .bind(entity)
        .bind(fingerprint)
        .bind(columns)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn known_fingerprint(
        &self,
        tenant: &str,
        source: &str,
        entity: &str,
    ) -> Result<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT fingerprint FROM matrix.schema_fingerprints
              WHERE tenant_id = $1 AND source_name = $2 AND entity = $3",
        )
        .bind(tenant)
        .bind(source)
        .bind(entity)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|(f,)| f))
    }
}
