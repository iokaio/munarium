// SPDX-License-Identifier: Apache-2.0
//! `munarium-matrix-types` — the asset grammar, the vendored contract types,
//! and the REST DTOs. Pure serde; no I/O, no runtime.
//!
//! The split matters: [`assets`] is **strict** (`deny_unknown_fields`, because
//! a typo in a security key must fail) and [`contract`] is **tolerant** (an
//! unknown field from a newer peer is ignored, because that is what keeps a
//! minor contract bump additive).

#![forbid(unsafe_code)]
#![allow(clippy::result_large_err)]

pub mod assets;
pub mod contract;
pub mod dto;
pub mod validate;

pub use assets::{
    ClaimMappingDoc, DataSourceDoc, DataViewDoc, Metadata, MetricViewDoc, QueryContractDoc,
    API_VERSION,
};
pub use contract::{
    ArtifactKind, EvidenceBlock, EvidenceManifest, ObservationBatch, QueryIntent, TypedValueDto,
};
pub use validate::{is_valid, Finding};

/// Any of the three asset kinds, sniffed by parsing rather than by grepping
/// the text for `kind:`. (The server's authoring work learned this the hard
/// way: substring sniffing misrouted a runbook that merely *mentioned*
/// `kind: Shape` in a comment.)
#[derive(Debug, Clone, PartialEq)]
pub enum Asset {
    DataSource(Box<DataSourceDoc>),
    QueryContract(Box<QueryContractDoc>),
    ClaimMapping(Box<ClaimMappingDoc>),
    MetricView(Box<MetricViewDoc>),
    DataView(Box<DataViewDoc>),
}

impl Asset {
    pub fn kind(&self) -> &'static str {
        match self {
            Asset::DataSource(_) => "DataSource",
            Asset::QueryContract(_) => "QueryContract",
            Asset::ClaimMapping(_) => "ClaimMapping",
            Asset::MetricView(_) => "MetricView",
            Asset::DataView(_) => "DataView",
        }
    }

    pub fn metadata(&self) -> &Metadata {
        match self {
            Asset::DataSource(d) => &d.metadata,
            Asset::QueryContract(d) => &d.metadata,
            Asset::ClaimMapping(d) => &d.metadata,
            Asset::MetricView(d) => &d.metadata,
            Asset::DataView(d) => &d.metadata,
        }
    }

    pub fn asset_ref(&self) -> String {
        self.metadata().asset_ref()
    }

    pub fn validate(&self) -> Vec<Finding> {
        match self {
            Asset::DataSource(d) => validate::validate_data_source(d),
            Asset::QueryContract(d) => validate::validate_query_contract(d),
            Asset::ClaimMapping(d) => validate::validate_claim_mapping(d),
            Asset::MetricView(d) => validate::validate_metric_view(d),
            Asset::DataView(d) => validate::validate_data_view(d),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("not valid YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("missing `kind`")]
    NoKind,
    #[error("unknown kind '{0}' (expected DataSource, QueryContract, ClaimMapping, MetricView or DataView)")]
    UnknownKind(String),
    #[error("{kind} did not parse: {source}")]
    Typed {
        kind: String,
        #[source]
        source: serde_yaml::Error,
    },
}

/// Parse one asset document, dispatching on its declared `kind`.
pub fn parse_asset(yaml: &str) -> Result<Asset, ParseError> {
    // One cheap pass to read the discriminator, then a typed parse. The cheap
    // pass is a real YAML parse, not a substring search.
    let probe: serde_yaml::Value = serde_yaml::from_str(yaml)?;
    let kind = probe
        .get("kind")
        .and_then(|k| k.as_str())
        .ok_or(ParseError::NoKind)?
        .to_string();
    let typed = |e: serde_yaml::Error| ParseError::Typed {
        kind: kind.clone(),
        source: e,
    };
    match kind.as_str() {
        "DataSource" => Ok(Asset::DataSource(Box::new(
            serde_yaml::from_str(yaml).map_err(typed)?,
        ))),
        "QueryContract" => Ok(Asset::QueryContract(Box::new(
            serde_yaml::from_str(yaml).map_err(typed)?,
        ))),
        "ClaimMapping" => Ok(Asset::ClaimMapping(Box::new(
            serde_yaml::from_str(yaml).map_err(typed)?,
        ))),
        "MetricView" => Ok(Asset::MetricView(Box::new(
            serde_yaml::from_str(yaml).map_err(typed)?,
        ))),
        "DataView" => Ok(Asset::DataView(Box::new(
            serde_yaml::from_str(yaml).map_err(typed)?,
        ))),
        other => Err(ParseError::UnknownKind(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATASOURCE: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: DataSource
metadata: { name: crm, version: 1 }
spec:
  adapter: postgres
  connection: { host: crm.internal.example.com, database: crm, sslmode: verify-full }
  credentialRef: matrix-crm
  egress: { allowHosts: [crm.internal.example.com] }
  role:
    mustBe: { readOnly: true, subjectToRowSecurity: true, notOwner: true }
  authorization:
    strategy: source_native
  limits: { maxRows: 10000, maxBytes: 8388608, statementTimeoutMs: 8000 }
  sync:
    mode: watermark
    watermark: { column: updated_at, inclusive: false, tieBreak: id }
    entity: { table: opportunities, key: [id] }
    projection: [id, name, stage, amount, region, updated_at]
"#;

    const CONTRACT: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: QueryContract
metadata: { name: open-pipeline-by-region, version: 2 }
spec:
  source: crm
  parameters:
    as_of: { type: date, required: true }
  statementByDialect:
    postgres: { inline: "SELECT region, SUM(amount) AS pipeline_amount FROM opportunities GROUP BY region" }
  reads:
    tables: [opportunities]
    columns: [region, amount]
  result:
    columns:
      region: { type: string, key: true }
      pipeline_amount: { type: decimal, scale: 2, unit: USD, additivity: additive }
    columnOrder: [region, pipeline_amount]
    orderBy: [region]
    derivations:
      total_pipeline: { op: sum, over: pipeline_amount }
  verifiedQuestions:
    - question: "pipeline by region as of 2026-06-30?"
      parameters: { as_of: "2026-06-30" }
      expect: { rows: 4 }
"#;

    const MAPPING: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: ClaimMapping
metadata: { name: captable-holdings, version: 1 }
spec:
  source: captable
  mode: shadow
  entity:
    table: holdings
    key: [holder_id, company_id]
    subjectTemplate: "shareholder.{holder_id}"
    scopeTemplate: "company.{company_id}.captable"
  properties:
    shares_outstanding: { column: shares, type: decimal, scale: 0 }
  temporal:
    validTime: { column: effective_date }
  changes:
    shares_outstanding: { onUpdate: update, onBackdated: requires_review }
"#;

    #[test]
    fn the_three_kinds_parse_and_validate() {
        for (yaml, kind) in [
            (DATASOURCE, "DataSource"),
            (CONTRACT, "QueryContract"),
            (MAPPING, "ClaimMapping"),
        ] {
            let a = parse_asset(yaml).unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert_eq!(a.kind(), kind);
            let findings = a.validate();
            let errors: Vec<_> = findings.iter().filter(|f| validate::is_error(f)).collect();
            assert!(errors.is_empty(), "{kind} findings: {errors:?}");
        }
    }

    #[test]
    fn kind_is_read_by_parsing_not_by_grepping() {
        // A description that MENTIONS another kind must not reroute the parse.
        let yaml = CONTRACT.replace(
            "  source: crm",
            "  source: crm\n  description: \"kind: DataSource is a different asset\"",
        );
        let a = parse_asset(&yaml).expect("parses");
        assert_eq!(a.kind(), "QueryContract");
    }

    #[test]
    fn an_unknown_field_is_an_error_not_a_shrug() {
        // The exact failure mode this rule exists for: a typo in a security key.
        let yaml = DATASOURCE.replace("subjectToRowSecurity", "subjectToRowSecurty");
        let err = parse_asset(&yaml).expect_err("must not parse");
        let msg = err.to_string();
        assert!(msg.contains("DataSource"), "{msg}");
    }

    #[test]
    fn asset_refs_are_name_at_version() {
        assert_eq!(
            parse_asset(CONTRACT).unwrap().asset_ref(),
            "open-pipeline-by-region@2"
        );
    }

    #[test]
    fn an_unknown_kind_is_named_in_the_error() {
        let err = parse_asset("kind: Runbook\napiVersion: x\n").expect_err("must fail");
        assert!(err.to_string().contains("Runbook"));
    }
}
