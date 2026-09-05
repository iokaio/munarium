// SPDX-License-Identifier: Apache-2.0
//! The shape registry.
//!
//! A shape is a versioned declarative bundle (architecture.md §6). Rules:
//! validation at the command gate; additive versioning (old versions stay
//! resolvable — they are provenance); shapes are data deployed through the
//! API, and their publication is itself a ledger event (the server appends a
//! `munarium-shapes.<name>=<version>@<hash>` claim in the tenant's system
//! lineage). Validation results are cacheable by (shape_ref, body_hash) —
//! the doc's named performance mitigation.

pub mod validate;

use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::collections::HashMap;
use std::sync::{Mutex, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShapeDoc {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub kind: String,
    pub metadata: ShapeMeta,
    pub spec: ShapeSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShapeMeta {
    pub name: String,
    pub version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShapeSpec {
    #[serde(default)]
    pub fact: Option<FactSpec>,
    #[serde(default)]
    pub chunking: Option<ChunkingSpec>,
    #[serde(default)]
    pub indexing: Option<IndexingSpec>,
    /// Intrinsic evidence semantics. Travels with `shape_ref@version`
    /// and is therefore part of provenance.
    #[serde(default)]
    pub evidence: Option<EvidenceSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactSpec {
    /// Inline JSON Schema (2020-12) for bodies bearing this shape_ref.
    pub schema: serde_json::Value,
    #[serde(default)]
    pub supersession: Option<SupersessionSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupersessionSpec {
    pub identity: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkingSpec {
    #[serde(default = "default_chunker")]
    pub strategy: String,
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
}

fn default_chunker() -> String {
    "para@1".into()
}
fn default_max_chars() -> usize {
    2000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexingSpec {
    /// RRF constant (default 60).
    #[serde(default = "default_rrf_k")]
    pub rrf_k: u32,
    #[serde(default = "default_top_n")]
    pub candidate_n: u32,
}

fn default_rrf_k() -> u32 {
    60
}
fn default_top_n() -> u32 {
    50
}

/// How much weight a runbook may give evidence of this shape.
///
/// Ordered, and the order is the whole point: a shape declares a **ceiling**
/// and a runbook may narrow below it but never climb above it. A
/// generated-summary shape that declares `supporting` cannot be talked into
/// being controlling by a runbook that would find that convenient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityRole {
    /// Corroborates; never decides on its own.
    Supporting = 0,
    /// The ordinary answer-bearing role.
    Primary = 1,
    /// Decides a conflict. A signed instrument, a system of record.
    Controlling = 2,
}

impl AuthorityRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Supporting => "supporting",
            Self::Primary => "primary",
            Self::Controlling => "controlling",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "supporting" => Some(Self::Supporting),
            "primary" => Some(Self::Primary),
            "controlling" => Some(Self::Controlling),
            _ => None,
        }
    }
}

/// What kind of thing this evidence is. **Closed** — a new member changes what
/// a consumer must handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceShapeKind {
    PrimaryDocument,
    SystemRecord,
    Observation,
    Commentary,
    GeneratedSummary,
}

/// How far this evidence sits from the thing it describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Derivation {
    Original,
    Transformed,
    Aggregate,
    Generated,
}

/// Temporal fields, so freshness is read from declared columns rather than
/// guessed at from whatever looks like a date.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TemporalSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_time_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_time_field: Option<String>,
}

/// What a citation to this evidence must be able to name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvidenceRequirements {
    /// e.g. `row_or_span`, `span`, `row`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citations: Option<String>,
    /// e.g. `source_and_section`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_identity: Option<String>,
}

/// `spec.evidence` — the intrinsic evidence semantics of a shape.
///
/// Note the field casing: **snake_case**, matching `max_chars` / `rrf_k` /
/// `candidate_n` in the shapes that already exist. The plan's §6.3 illustration
/// writes `maxAuthority`; following it would have produced shape files mixing
/// `max_chars` and `maxAuthority` in adjacent blocks, which is the kind of
/// inconsistency that costs an afternoon later. The plan carries a ✎ correction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceSpec {
    pub kind: EvidenceShapeKind,
    pub derivation: Derivation,
    /// The role a runbook gets when it does not narrow one. Must not exceed
    /// `max_authority`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_role: Option<AuthorityRole>,
    /// The ceiling. A runbook may narrow below this and never climb above it.
    pub max_authority: AuthorityRole,
    /// Inquiry operations this evidence can actually support — e.g. a system
    /// record supports exact lookup and aggregation but not verbatim
    /// quotation, and saying so is what stops an answer quoting a row.
    #[serde(default)]
    pub supports: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal: Option<TemporalSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirements: Option<EvidenceRequirements>,
}

#[derive(Debug)]
pub struct Shape {
    pub doc: ShapeDoc,
    pub yaml_hash: String,
    validator: Option<jsonschema::Validator>,
}

impl Shape {
    pub fn shape_ref(&self) -> String {
        format!("{}@{}", self.doc.metadata.name, self.doc.metadata.version)
    }
}

/// Parse + compile a shape from YAML. Errors are loader-grade messages.
pub fn parse_shape(yaml: &str) -> Result<Shape, String> {
    let doc: ShapeDoc = serde_yaml::from_str(yaml).map_err(|e| format!("shape yaml: {e}"))?;
    if doc.kind != "Shape" {
        return Err(format!("kind must be Shape, got '{}'", doc.kind));
    }
    if doc.api_version != "munarium.ioka.io/v1" {
        return Err(format!(
            "apiVersion must be munarium.ioka.io/v1, got '{}'",
            doc.api_version
        ));
    }
    if let Some(chunking) = &doc.spec.chunking {
        // A tiny max_chars is never intentional: it degrades the index to
        // per-character chunks (and 0 previously hung the chunker).
        if chunking.max_chars < 16 {
            return Err(format!(
                "chunking.max_chars must be >= 16, got {}",
                chunking.max_chars
            ));
        }
    }
    if let Some(ev) = &doc.spec.evidence {
        // The ceiling must bind its OWN default. A shape declaring
        // `default_role: controlling` under `max_authority: supporting` is
        // self-contradictory, and the contradiction has to be caught here
        // rather than at the runbook binding — otherwise every runbook that
        // simply accepted the default would inherit a role the shape says is
        // impossible, and the ceiling would mean nothing.
        if let Some(default_role) = ev.default_role {
            if default_role > ev.max_authority {
                return Err(format!(
                    "evidence.default_role '{}' exceeds evidence.max_authority '{}'; a shape's \
                     ceiling must bind its own default",
                    default_role.as_str(),
                    ev.max_authority.as_str()
                ));
            }
        }
        // A generated summary that could be controlling is the specific
        // mistake this whole block exists to prevent: model-written prose
        // deciding a conflict against a signed instrument.
        if matches!(ev.derivation, Derivation::Generated)
            && ev.max_authority == AuthorityRole::Controlling
        {
            return Err(
                "evidence.derivation 'generated' cannot declare max_authority 'controlling'; \
                 generated text may support or inform an answer, but must never be the thing \
                 that decides a conflict"
                    .to_string(),
            );
        }
    }
    let validator = match &doc.spec.fact {
        Some(fact) => Some(
            jsonschema::validator_for(&fact.schema)
                .map_err(|e| format!("fact.schema is not a valid JSON Schema: {e}"))?,
        ),
        None => None,
    };
    let yaml_hash = hex::encode(sha2::Sha256::digest(yaml.as_bytes()));
    Ok(Shape {
        doc,
        yaml_hash,
        validator,
    })
}

/// Per-tenant shape registry with a (shape_ref, body_hash) validation cache.
#[derive(Default)]
pub struct ShapeRegistry {
    shapes: RwLock<HashMap<(String, String), std::sync::Arc<Shape>>>,
    cache: Mutex<HashMap<(String, String), Result<(), String>>>,
}

impl Shape {
    /// The evidence role a runbook may bind this shape at, or a refusal.
    ///
    /// This is the evidence ceiling check in one place, so REST, the runbook
    /// loader and any future authoring surface cannot each implement it
    /// slightly differently. A shape with no `evidence` block has no ceiling
    /// and accepts any role — silence is not a constraint.
    pub fn check_authority(&self, requested: AuthorityRole) -> Result<(), String> {
        let Some(ev) = &self.doc.spec.evidence else {
            return Ok(());
        };
        if requested > ev.max_authority {
            return Err(format!(
                "shape {} declares max_authority '{}'; a runbook may narrow below it but \
                 cannot bind it as '{}'",
                self.shape_ref(),
                ev.max_authority.as_str(),
                requested.as_str()
            ));
        }
        Ok(())
    }

    /// The role to use when a runbook does not name one.
    pub fn default_authority(&self) -> Option<AuthorityRole> {
        self.doc.spec.evidence.as_ref().and_then(|e| e.default_role)
    }
}

impl ShapeRegistry {
    pub fn apply(&self, tenant: &str, yaml: &str) -> Result<std::sync::Arc<Shape>, String> {
        let shape = std::sync::Arc::new(parse_shape(yaml)?);
        let key = (tenant.to_string(), shape.shape_ref());
        let mut shapes = self.shapes.write().expect("registry lock");
        if let Some(existing) = shapes.get(&key) {
            if existing.yaml_hash != shape.yaml_hash {
                // Additive versioning: same version + different content is a
                // rejected mutation; publish a NEW version instead.
                return Err(format!(
                    "shape {} already published with different content; bump the version",
                    shape.shape_ref()
                ));
            }
            return Ok(existing.clone());
        }
        shapes.insert(key, shape.clone());
        Ok(shape)
    }

    pub fn get(&self, tenant: &str, shape_ref: &str) -> Option<std::sync::Arc<Shape>> {
        self.shapes
            .read()
            .expect("registry lock")
            .get(&(tenant.to_string(), shape_ref.to_string()))
            .cloned()
    }

    /// Every published (shape_ref, yaml_hash) for one tenant — the
    /// additive-versioning preflight input for set-level authoring checks.
    pub fn list(&self, tenant: &str) -> Vec<(String, String)> {
        self.shapes
            .read()
            .expect("registry lock")
            .iter()
            .filter(|((t, _), _)| t == tenant)
            .map(|((_, r), s)| (r.clone(), s.yaml_hash.clone()))
            .collect()
    }

    /// Validate a body against a shape's fact schema, cached by
    /// (shape_ref, body_hash). Unknown shape is an error — a claim naming an
    /// unpublished shape must not silently pass.
    pub fn validate(
        &self,
        tenant: &str,
        shape_ref: &str,
        body: &serde_json::Value,
    ) -> Result<(), String> {
        let Some(shape) = self.get(tenant, shape_ref) else {
            return Err(format!(
                "shape '{shape_ref}' is not published for this tenant"
            ));
        };
        let Some(validator) = &shape.validator else {
            return Ok(()); // shape with no fact schema constrains nothing
        };
        let body_text = body.to_string();
        let body_hash = hex::encode(sha2::Sha256::digest(body_text.as_bytes()));
        let cache_key = (shape_ref.to_string(), body_hash);
        if let Some(hit) = self.cache.lock().expect("cache lock").get(&cache_key) {
            return hit.clone();
        }
        let outcome = match validator.validate(body) {
            Ok(()) => Ok(()),
            Err(err) => Err(format!("{err}")),
        };
        self.cache
            .lock()
            .expect("cache lock")
            .insert(cache_key, outcome.clone());
        outcome
    }
}

/// The claim-body view a shape validates for ProposeClaim commands.
pub fn claim_body(
    subject: &str,
    key: &str,
    value: &str,
    evidence: Option<&serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "key": key,
        "value": value,
        "evidence": evidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPE: &str = r#"
apiVersion: munarium.ioka.io/v1
kind: Shape
metadata: { name: contract-clauses, version: 1 }
spec:
  fact:
    schema:
      type: object
      properties:
        subject: { type: string, pattern: "^contract-" }
        key: { type: string }
        value: { type: string, minLength: 1 }
      required: [subject, key, value]
  chunking: { max_chars: 512 }
"#;

    #[test]
    fn parse_apply_validate() {
        let reg = ShapeRegistry::default();
        let shape = reg.apply("t1", SHAPE).expect("apply");
        assert_eq!(shape.shape_ref(), "contract-clauses@1");

        // valid body
        assert!(reg
            .validate(
                "t1",
                "contract-clauses@1",
                &claim_body("contract-a12", "term", "5 years", None)
            )
            .is_ok());
        // violation: subject pattern
        assert!(reg
            .validate(
                "t1",
                "contract-clauses@1",
                &claim_body("patent-9", "term", "5 years", None)
            )
            .is_err());
        // unknown shape never silently passes
        assert!(reg
            .validate("t1", "nope@9", &claim_body("contract-a", "k", "v", None))
            .is_err());
        // tenant isolation
        assert!(reg
            .validate(
                "t2",
                "contract-clauses@1",
                &claim_body("contract-a", "k", "v", None)
            )
            .is_err());
    }

    #[test]
    fn same_version_different_content_rejected() {
        let reg = ShapeRegistry::default();
        reg.apply("t1", SHAPE).unwrap();
        let mutated = SHAPE.replace("minLength: 1", "minLength: 2");
        let err = reg.apply("t1", &mutated).unwrap_err();
        assert!(err.contains("bump the version"), "{err}");
        // identical re-apply is idempotent
        assert!(reg.apply("t1", SHAPE).is_ok());
    }

    // -- spec.evidence, the authority ceiling -----------------------

    /// A minimal shape carrying only the `evidence` block under test.
    ///
    /// Built by concatenation rather than one big literal: an escaped
    /// multi-line string is how the first draft of these tests produced YAML
    /// with stray indentation and a baffling "mapping values are not allowed"
    /// error three lines from the real mistake.
    fn shape_with_evidence(body: &str) -> String {
        let mut s = String::new();
        s.push_str(
            "apiVersion: munarium.ioka.io/v1
",
        );
        s.push_str(
            "kind: Shape
",
        );
        s.push_str(
            "metadata: { name: ev-test, version: 1 }
",
        );
        s.push_str(
            "spec:
",
        );
        s.push_str(
            "  evidence:
",
        );
        s.push_str(body);
        s
    }

    #[test]
    fn a_shape_may_omit_evidence_entirely() {
        // Silence is not a constraint: every shape that existed before evidence semantics
        // must keep parsing, and must accept any binding.
        let s = parse_shape(SHAPE).expect("parses");
        assert!(s.doc.spec.evidence.is_none());
        s.check_authority(AuthorityRole::Controlling)
            .expect("no evidence block means no ceiling");
        assert_eq!(s.default_authority(), None);
    }

    #[test]
    fn the_ceiling_refuses_a_binding_above_it() {
        let yaml = shape_with_evidence(
            "    kind: generated_summary
    derivation: generated
    max_authority: supporting
",
        );
        let s = parse_shape(&yaml).expect("parses");
        s.check_authority(AuthorityRole::Supporting)
            .expect("at the ceiling is fine");
        let err = s
            .check_authority(AuthorityRole::Controlling)
            .expect_err("above the ceiling must refuse");
        assert!(err.contains("max_authority 'supporting'"), "{err}");
        // Narrowing is always allowed; climbing never is.
        let err = s
            .check_authority(AuthorityRole::Primary)
            .expect_err("also above");
        assert!(err.contains("cannot bind it as 'primary'"), "{err}");
    }

    #[test]
    fn a_shape_cannot_default_above_its_own_ceiling() {
        // Otherwise every runbook that simply took the default would inherit a
        // role the shape says is impossible, and the ceiling would mean nothing.
        let yaml = shape_with_evidence(
            "    kind: system_record
    derivation: original
    default_role: controlling
    max_authority: primary
",
        );
        let err = parse_shape(&yaml).expect_err("must refuse");
        assert!(err.contains("exceeds evidence.max_authority"), "{err}");
    }

    #[test]
    fn generated_evidence_can_never_be_controlling() {
        // The specific mistake: model-written prose deciding a conflict
        // against a signed instrument.
        let yaml = shape_with_evidence(
            "    kind: generated_summary
    derivation: generated
    max_authority: controlling
",
        );
        let err = parse_shape(&yaml).expect_err("must refuse");
        assert!(err.contains("must never be the thing"), "{err}");

        // The same shape at a lower ceiling is fine.
        let ok = shape_with_evidence(
            "    kind: generated_summary
    derivation: generated
    max_authority: primary
",
        );
        parse_shape(&ok).expect("generated evidence may still be primary");
    }

    #[test]
    fn authority_roles_are_ordered_supporting_to_controlling() {
        assert!(AuthorityRole::Supporting < AuthorityRole::Primary);
        assert!(AuthorityRole::Primary < AuthorityRole::Controlling);
        assert_eq!(
            AuthorityRole::parse("controlling"),
            Some(AuthorityRole::Controlling)
        );
        assert_eq!(AuthorityRole::parse("nonsense"), None);
    }

    #[test]
    fn the_reference_record_shape_parses_and_declares_its_semantics() {
        // runbooks/shapes/record-documents.yaml is the shape Matrix's mode A
        // produces. If this fails, the reference asset and the code disagree.
        let yaml = include_str!("../../../runbooks/shapes/record-documents.yaml");
        let s = parse_shape(yaml).expect("the reference shape must parse");
        let ev = s.doc.spec.evidence.as_ref().expect("declares evidence");
        assert_eq!(ev.kind, EvidenceShapeKind::SystemRecord);
        assert_eq!(ev.derivation, Derivation::Original);
        assert_eq!(ev.max_authority, AuthorityRole::Controlling);
        assert_eq!(ev.default_role, Some(AuthorityRole::Primary));
        // A rendered row has no prose to quote: an answer that "quotes" one is
        // quoting the renderer.
        assert!(
            !ev.supports.iter().any(|s| s == "quotation"),
            "a system record must not claim to support verbatim quotation"
        );
        assert!(ev.supports.iter().any(|s| s == "lookup"));
        // Both times, kept apart — a backdated entry is not an out-of-order one.
        let temporal = ev.temporal.as_ref().expect("declares temporal fields");
        assert_eq!(temporal.event_time_field.as_deref(), Some("effective_date"));
        assert_eq!(temporal.observed_time_field.as_deref(), Some("recorded_at"));
    }
}
