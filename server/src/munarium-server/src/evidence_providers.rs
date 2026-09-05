// SPDX-License-Identifier: Apache-2.0
//! The three evidence providers.
//!
//! [`munarium_core::hierarchy::EvidenceProvider`] is the seam; this is where it
//! meets real I/O. Two implementations:
//!
//! - [`FactProvider`] — a pinned `slice_facts` read of the ledger.
//! - [`MatrixProvider`] — HTTP to Munarium Matrix for typed tables and counts.
//!
//! # There is deliberately no DocumentProvider
//!
//! One was built and then deleted, because it could not be used
//! honestly. `TurnResponse` carries `envelopes`, `collections_searched` and
//! `skipped`, and every existing client reads them — but `EvidenceBlock` has
//! nowhere to put them, so a document layer squeezed through this trait would
//! silently drop three response fields.
//!
//! So `evidence_hierarchy` runs the document layer through a closure over the
//! real `retrieve_documents` instead, keeping its full output. Shipping a
//! `DocumentProvider` anyway — unused, but present so the module could claim
//! three symmetrical implementations — would have made the architecture read
//! more uniform than it is. The asymmetry is real: the response contract is
//! document-shaped for backward-compatibility reasons, and the code says so.
//!
//! # Why a provider refuses instead of erroring
//!
//! Every `fetch` returns `Ok(EvidenceBlock::Refusal)` for anything the answer
//! should be able to talk about: a denied source, a stale index, a timeout, an
//! open circuit. An `Err` here would collapse "the register declined to say"
//! into "something broke", and those are different answers to give a user.
//!
//! # The circuit breaker
//!
//! Matrix is a separate deployment and can be down or slow. A per-instance
//! breaker trips after consecutive failures and refuses immediately for a
//! cool-off, so a Matrix outage costs one timeout rather than one per turn.
//!
//! Its metrics deliberately carry **no tenant label**. Two reasons, and the
//! second is the real one: unbounded label cardinality, and — because the
//! breaker is per *instance* and not per tenant — a tenant label would imply a
//! per-tenant fact that does not exist. A shared breaker reported per tenant
//! would let one tenant's scrape reveal that another tenant's traffic had
//! tripped it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use munarium_core::hierarchy::{
    CountBlock, EvidenceBlock, EvidenceLayer, EvidenceProvider, EvidenceRefusal, QueryIntent,
    TableBlock,
};
use munarium_core::ledger::FactQuery;
use munarium_core::{KernelError, Result};

/// Refusal codes. Kebab-case, matching the problem-type registry.
pub const REFUSAL_UNAVAILABLE: &str = "source-unavailable";
pub const REFUSAL_TIMEOUT: &str = "source-timeout";
pub const REFUSAL_CIRCUIT_OPEN: &str = "source-circuit-open";
pub const REFUSAL_UNBOUND: &str = "source-not-bound";
/// The plane rejected the REQUEST — a malformed intent, an unbound parameter,
/// a contract this deployment does not have. Distinct from
/// `source-unavailable` on purpose: reporting a request defect as an outage
/// sends an operator to check Matrix's health when the fault is in their own
/// runbook.
pub const REFUSAL_REJECTED: &str = "source-request-rejected";
/// A semantic view was asked but no selection was resolved for it — the
/// runbook pins no `intent` task, or the task produced nothing usable.
pub const REFUSAL_INTENT_UNRESOLVED: &str = "intent-unresolved";

fn refuse(code: &str, message: impl Into<String>, source: Option<&str>) -> EvidenceBlock {
    EvidenceBlock::Refusal(EvidenceRefusal {
        code: code.into(),
        message: message.into(),
        source: source.map(|s| s.to_string()),
    })
}

// ---------------------------------------------------------------------------
// Fact provider
// ---------------------------------------------------------------------------

/// A pinned slice of the ledger's own accepted facts.
///
/// Pinned by `version_id`: a turn reads the memory version the session is bound
/// to, not whatever the head happens to be mid-conversation. Two turns in one
/// session must not silently disagree because an ingest landed between them.
pub struct FactProvider {
    pub store: Arc<dyn munarium_core::storage::StorageBackend>,
}

#[async_trait]
impl EvidenceProvider for FactProvider {
    fn id(&self) -> &str {
        "facts"
    }

    fn can_serve(&self, source: &str) -> bool {
        source.starts_with("facts:")
    }

    async fn fetch(&self, layer: &EvidenceLayer, _intent: &QueryIntent) -> Result<EvidenceBlock> {
        // The layer names its memory version. Pinned by the RUNBOOK, so two
        // turns in one session cannot silently disagree because an ingest
        // landed between them.
        //
        // [gap] A session cannot yet pin its own version — sessions carry no
        // version binding — so the pin is per runbook, not per conversation.
        let Some(version_id) = layer.sources.iter().find_map(|s| s.strip_prefix("facts:")) else {
            return Ok(refuse(
                REFUSAL_UNBOUND,
                "this layer names no memory version",
                None,
            ));
        };
        let q = FactQuery {
            // A layer may pin a scope prefix; absent that, the whole version.
            scope_prefix: layer
                .sources
                .iter()
                .find(|s| s.starts_with("scope:"))
                .map(|s| s.trim_start_matches("scope:").to_string()),
            ..Default::default()
        };
        match self.store.slice_facts(version_id, &q).await {
            Ok(claims) => Ok(EvidenceBlock::FactSlice { claims }),
            Err(KernelError::NotFound { .. }) => Ok(refuse(
                REFUSAL_UNAVAILABLE,
                "the pinned memory version is not readable",
                None,
            )),
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Circuit breaker
// ---------------------------------------------------------------------------

/// Per-instance, per-provider. Never per tenant — see the module note.
pub struct CircuitBreaker {
    consecutive_failures: AtomicU64,
    /// Milliseconds since `origin` when the circuit may be tried again; 0 =
    /// closed. Storing a deadline rather than a timestamp-of-trip keeps the
    /// whole thing to one atomic with no lock.
    open_until_ms: AtomicU64,
    origin: Instant,
    threshold: u64,
    cool_off: Duration,
}

impl CircuitBreaker {
    pub fn new(threshold: u64, cool_off: Duration) -> Self {
        Self {
            consecutive_failures: AtomicU64::new(0),
            open_until_ms: AtomicU64::new(0),
            origin: Instant::now(),
            threshold,
            cool_off,
        }
    }

    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    /// True when calls must be refused without being attempted.
    pub fn is_open(&self) -> bool {
        let until = self.open_until_ms.load(Ordering::Relaxed);
        until != 0 && self.now_ms() < until
    }

    pub fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.open_until_ms.store(0, Ordering::Relaxed);
    }

    pub fn record_failure(&self) {
        let n = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= self.threshold {
            self.open_until_ms.store(
                self.now_ms() + self.cool_off.as_millis() as u64,
                Ordering::Relaxed,
            );
        }
    }

    /// Consecutive failures, for the `/v1/reports/matrix` operator view.
    pub fn consecutive_failures(&self) -> u64 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    /// Test-only: proves `record_success` resets the counter, which
    /// `is_open()` alone cannot show.
    #[cfg(test)]
    pub fn failures(&self) -> u64 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(5, Duration::from_secs(30))
    }
}

// ---------------------------------------------------------------------------
// Matrix provider
// ---------------------------------------------------------------------------

/// Typed tables and counts from Munarium Matrix over REST.
///
/// Ground rule 1 in practice: this speaks HTTP to a contract, and links against
/// no Matrix crate.
/// One data view, resolved from the runbook at turn time.
#[derive(Debug, Clone)]
pub struct BoundDataView {
    /// `name@version` of the Matrix query contract.
    pub contract: String,
    pub kind: munarium_runbooks::DataViewKind,
    /// The contract's parameters, bound by the runbook.
    pub parameters: serde_json::Value,
    pub access_level: i32,
    pub compartments: Vec<String>,
}

/// The session's own authorization, sent verbatim in the intent.
///
/// Matrix re-checks it against the token's tenant and refuses a mismatch, so a
/// caller cannot seal evidence into someone else's tenant by editing a field.
#[derive(Debug, Clone)]
pub struct SessionAuthorization {
    pub tenant: String,
    pub uid: String,
    pub access_level: i32,
    pub compartments: Vec<String>,
    pub session_id: String,
    pub runbook_ref: String,
}

pub struct MatrixProvider {
    pub http: reqwest::Client,
    pub base_url: String,
    pub token: Option<String>,
    pub auth: SessionAuthorization,
    /// Data-view name → the contract it is bound to. A layer names the VIEW;
    /// Matrix's route takes the CONTRACT, and the runbook is the only place
    /// that mapping exists.
    pub views: std::collections::BTreeMap<String, BoundDataView>,
    pub breaker: Arc<CircuitBreaker>,
    pub metrics: Arc<crate::metrics::Metrics>,
}

impl MatrixProvider {
    /// A pooled HTTP/2 client. One per instance: rebuilding it per turn would
    /// throw away the connection pool, which is most of the point.
    pub fn client(connect_timeout: Duration) -> reqwest::Client {
        reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            // Built once at composition; a builder failure is a broken TLS
            // backend, not a runtime condition. `unwrap_or_default()` here
            // would have silently handed out a client with NO connect
            // timeout, which is the one property this function exists for.
            .expect("matrix HTTP client with a connect timeout")
    }

    fn note(&self, outcome: &'static str) {
        // No tenant label: the breaker is per instance, so a per-tenant series
        // would report a fact that does not exist.
        self.metrics.inc(
            "munarium_matrix_provider_requests_total",
            crate::metrics::labels(&[("outcome", outcome)]),
        );
    }
}

#[async_trait]
impl EvidenceProvider for MatrixProvider {
    fn id(&self) -> &str {
        "matrix"
    }

    fn can_serve(&self, source: &str) -> bool {
        source
            .strip_prefix("matrix:")
            .map(|v| self.views.contains_key(v))
            .unwrap_or(false)
    }

    /// The turn's question is deliberately NOT sent. Matrix executes a
    /// pre-declared contract with typed parameters; the contract's own schema
    /// says no free-form expression crosses this boundary, and that is the
    /// property that makes SQL injection structurally impossible here rather
    /// than merely defended against.
    async fn fetch(&self, layer: &EvidenceLayer, intent: &QueryIntent) -> Result<EvidenceBlock> {
        let Some((view_name, view)) = layer.sources.iter().find_map(|s| {
            s.strip_prefix("matrix:")
                .and_then(|v| self.views.get(v).map(|b| (v, b)))
        }) else {
            return Ok(refuse(
                REFUSAL_UNBOUND,
                "this layer names no data view this runbook declares",
                None,
            ));
        };

        if self.breaker.is_open() {
            self.note("circuit_open");
            // Named: the runbook author pinned this view themselves, so the
            // hidden-source rule — which protects sources a caller cannot see
            // — does not apply to it. The turn-level refusal still names only
            // the layer.
            return Ok(refuse(
                REFUSAL_CIRCUIT_OPEN,
                "the structured-evidence plane is unavailable and is not being retried yet",
                Some(view_name),
            ));
        }

        let deadline = layer
            .deadline_ms
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_secs(10));

        // The contract's QueryIntent. Not a bare question: Matrix executes a
        // PRE-DECLARED contract and needs the authorization snapshot to pick
        // the class it runs under.
        //
        // `access_level` and `compartments` are the intersection of what the
        // session holds and what the view demands — the LOWER of the two, so a
        // layer can never borrow clearance the session does not have, and a
        // session can never reach past what the view was declared for.
        let selection = if view.kind.is_semantic() {
            match intent.selections.get(view_name) {
                Some(sel) => Some(sel.clone()),
                None => {
                    self.note("refused");
                    return Ok(refuse(
                        REFUSAL_INTENT_UNRESOLVED,
                        "no semantic selection was resolved for this view; the runbook's `intent` \
                         task chooses its measures and dimensions and none was produced",
                        Some(view_name),
                    ));
                }
            }
        } else {
            None
        };
        let body = semantic_or_contract_body(view, selection.as_ref(), &self.auth);

        let url = format!(
            "{}/v1/{}/{}/execute",
            self.base_url.trim_end_matches('/'),
            view.kind.route(),
            view.contract
        );
        let mut rb = self
            .http
            .post(&url)
            .timeout(deadline)
            .header("X-Munarium-Uid", &self.auth.uid)
            .json(&body);
        if let Some(t) = &self.token {
            rb = rb.bearer_auth(t);
        }

        // A client disconnect drops this future, which drops the request and
        // cancels the connection. No detached task keeps spending Matrix
        // budget for an answer nobody will read.
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                self.breaker.record_failure();
                self.note("timeout");
                return Ok(refuse(
                    REFUSAL_TIMEOUT,
                    "the structured-evidence plane did not answer in time",
                    Some(view_name),
                ));
            }
            Err(_) => {
                self.breaker.record_failure();
                self.note("error");
                return Ok(refuse(
                    REFUSAL_UNAVAILABLE,
                    "the structured-evidence plane could not be reached",
                    Some(view_name),
                ));
            }
        };

        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or_default();

        if !status.is_success() {
            // A 4xx is Matrix answering CORRECTLY — a refused contract, a
            // denied column, an exhausted budget. It is not a breaker failure:
            // tripping on a policy refusal would take out every other view
            // because one of them is governed the way it should be.
            if status.is_server_error() {
                self.breaker.record_failure();
                self.note("error");
            } else {
                self.breaker.record_success();
                self.note("refused");
            }
            return Ok(matrix_refusal(&body, view_name));
        }

        self.breaker.record_success();
        self.note("ok");
        Ok(parse_matrix_result(&body, view_name))
    }
}

/// A refusal Matrix returned, translated but not reinterpreted.
///
/// Matrix's `class` is a closed set and its `code` is the specific reason; the
/// code is what an operator acts on, so it is what survives. A refusal that
/// arrives without one is reported as unavailable rather than being guessed
/// at.
/// The intent Matrix receives: a structured query for a contract, or — for a
/// metric view or native data view — a semantic intent carrying the names
/// the `intent` task chose. Never SQL; never a name outside the view's lists,
/// because the resolver validated the selection against them.
pub fn semantic_or_contract_body(
    view: &BoundDataView,
    selection: Option<&munarium_core::hierarchy::SemanticSelection>,
    auth: &SessionAuthorization,
) -> serde_json::Value {
    let authorization = serde_json::json!({
        "tenant": auth.tenant,
        "uid": auth.uid,
        "access_level": auth.access_level.min(view.access_level),
        "compartments": auth
            .compartments
            .iter()
            .filter(|c| view.compartments.is_empty() || view.compartments.contains(c))
            .cloned()
            .collect::<Vec<_>>(),
        "session_id": auth.session_id,
        "runbook_ref": auth.runbook_ref,
    });
    match selection {
        Some(sel) => serde_json::json!({
            "contract_version": munarium_core::evidence::CONTRACT_VERSION.trim(),
            "kind": "semantic",
            "semantic": {
                "provider": view.contract,
                "measures": sel.measures,
                "dimensions": sel.dimensions,
                "filters": sel.filters.iter().map(|f| serde_json::json!({
                    "dimension": f.dimension,
                    "op": "eq",
                    "value": { "type": f.ty, "value": f.value },
                })).collect::<Vec<_>>(),
            },
            "authorization": authorization,
            "limits": { "max_rows": 500, "max_bytes": 1_048_576 },
            "parameters": {},
        }),
        None => serde_json::json!({
            "contract_version": munarium_core::evidence::CONTRACT_VERSION.trim(),
            "kind": "structured_query",
            "contract": view.contract,
            "authorization": authorization,
            "limits": { "max_rows": 500, "max_bytes": 1_048_576 },
            "parameters": view.parameters,
        }),
    }
}

fn matrix_refusal(body: &serde_json::Value, view: &str) -> EvidenceBlock {
    let r = body.get("refusal").unwrap_or(body);
    let code = r
        .get("code")
        .and_then(|c| c.as_str())
        // No typed refusal in the body means Matrix rejected the REQUEST
        // rather than declining the work — a malformed intent, an unbound
        // parameter, a contract this deployment does not have.
        .unwrap_or(REFUSAL_REJECTED)
        .to_string();
    EvidenceBlock::Refusal(EvidenceRefusal {
        code,
        // Matrix's own message is deliberately NOT forwarded: it is written
        // for a Matrix operator and may name a source, a class or a column
        // this caller has no clearance for.
        message: "the structured-evidence plane declined this request".into(),
        source: Some(view.to_string()),
    })
}

/// Turn Matrix's `EvidenceBlock` into ours.
///
/// The contract's block is tagged by `kind` and carries the whole
/// `EvidenceManifest` beside its data. Two things are taken verbatim rather
/// than re-derived, and both matter:
///
/// **Row ids come from Matrix.** `manifest.identity.row_id_rule` is `keys` for
/// a keyed result, so a row's id is its key (`"EMEA"`), not its position.
/// Numbering rows ourselves would mean an answer citing
/// `[evidence/<id>#EMEA]` — the id Matrix sealed and can replay — while the
/// server checked it against an invented `r0003`, and every correct citation
/// would be rejected.
///
/// **Cells stay text.** A `decimal(38,2)` does not survive an IEEE-754 double,
/// and exactness is the entire reason the structured plane exists. `count`'s
/// `value` is a string in the contract for the same reason, and is parsed to
/// `i64` only for the block's own field.
pub fn parse_matrix_result(body: &serde_json::Value, view: &str) -> EvidenceBlock {
    let kind = body
        .get("kind")
        .and_then(|k| k.as_str())
        .unwrap_or_default();
    let evidence_id = body
        .get("evidence_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let manifest = body.get("manifest");
    let completeness = manifest.and_then(|m| m.get("completeness"));
    // The manifest is authoritative on truncation; the block's own flag is a
    // convenience copy. If they ever disagree, believe the sealed one.
    let truncated = completeness
        .and_then(|c| c.get("truncated"))
        .and_then(|v| v.as_bool())
        .or_else(|| body.get("truncated").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    match kind {
        "count" => EvidenceBlock::Count(CountBlock {
            // A string in the contract, deliberately. An unparseable one is a
            // contract violation, and 0 would be a lie, so it refuses.
            value: match body.get("value").and_then(|v| v.as_str()).map(str::parse) {
                Some(Ok(v)) => v,
                _ => {
                    return refuse(
                        REFUSAL_UNAVAILABLE,
                        "the structured-evidence plane returned a count with no readable value",
                        Some(view),
                    )
                }
            },
            rows_covered: completeness
                .and_then(|c| c.get("rows_covered"))
                .and_then(|v| v.as_i64()),
            rows_excluded: completeness
                .and_then(|c| c.get("rows_excluded"))
                .and_then(|v| v.as_i64()),
            exclusion_reason: completeness
                .and_then(|c| c.get("exclusion_reason"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            evidence_id,
        }),
        "complete_table" => {
            let columns: Vec<String> = manifest
                .and_then(|m| m.pointer("/schema/columns"))
                .and_then(|c| c.as_array())
                .map(|a| {
                    a.iter()
                        .map(|c| {
                            c.get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or_default()
                                .to_string()
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut row_ids = Vec::new();
            let rows: Vec<Vec<Option<String>>> = body
                .get("rows")
                .and_then(|r| r.as_array())
                .map(|a| {
                    a.iter()
                        .enumerate()
                        .map(|(i, row)| {
                            row_ids.push(
                                row.get("row_id")
                                    .and_then(|v| v.as_str())
                                    // Only when Matrix sent none: `position` is
                                    // a legal row_id_rule, and 1-based matches
                                    // how the context renders them.
                                    .map(str::to_string)
                                    .unwrap_or_else(|| format!("r{:04}", i + 1)),
                            );
                            row.get("cells")
                                .and_then(|c| c.as_array())
                                .map(|cells| cells.iter().map(cell_text).collect())
                                .unwrap_or_default()
                        })
                        .collect()
                })
                .unwrap_or_default();
            EvidenceBlock::CompleteTable(TableBlock {
                columns,
                rows,
                row_ids,
                truncated,
                evidence_id,
            })
        }
        "refusal" => matrix_refusal(body, view),
        other => refuse(
            REFUSAL_UNAVAILABLE,
            format!("the structured-evidence plane returned an unrecognised block kind '{other}'"),
            Some(view),
        ),
    }
}

fn cell_text(v: &serde_json::Value) -> Option<String> {
    match v {
        // NULL and the empty string are DIFFERENT facts, and the fixture
        // plants both.
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_breaker_opens_after_the_threshold_and_closes_on_success() {
        let b = CircuitBreaker::new(3, Duration::from_secs(60));
        assert!(!b.is_open());
        b.record_failure();
        b.record_failure();
        assert!(!b.is_open(), "two failures is under the threshold");
        b.record_failure();
        assert!(b.is_open(), "the third trips it");
        b.record_success();
        assert!(!b.is_open(), "a success closes it immediately");
        assert_eq!(b.failures(), 0);
    }

    #[test]
    fn the_breaker_reopens_the_gate_after_the_cool_off() {
        let b = CircuitBreaker::new(1, Duration::from_millis(0));
        b.record_failure();
        // A zero cool-off means the deadline is already past: the breaker
        // tripped, and the very next call is allowed to probe.
        assert!(!b.is_open());
    }

    // The parser is tested against the CONTRACT'S OWN committed examples, not
    // against hand-written JSON. Hand-written fixtures are how the first
    // version of this parser came to expect `{columns, rows, completeness}`
    // when Matrix actually returns a tagged EvidenceBlock wrapping a manifest:
    // the tests agreed with the code and both were wrong about the peer. An
    // example that ships in the contract cannot drift from it.
    const COMPLETE_TABLE: &str =
        include_str!("../../../contract/matrix/examples/evidence-block.complete-table.json");
    const COUNT: &str = include_str!("../../../contract/matrix/examples/evidence-block.count.json");
    const REFUSAL: &str =
        include_str!("../../../contract/matrix/examples/evidence-block.refusal.json");

    fn test_auth() -> SessionAuthorization {
        SessionAuthorization {
            tenant: "t".into(),
            uid: "u".into(),
            access_level: 2,
            compartments: vec!["sales".into()],
            session_id: "ses-1".into(),
            runbook_ref: "rb@1".into(),
        }
    }

    fn test_views() -> std::collections::BTreeMap<String, BoundDataView> {
        [(
            "revenue_by_region".to_string(),
            BoundDataView {
                contract: "open-pipeline-by-region@2".into(),
                kind: munarium_runbooks::DataViewKind::Contract,
                parameters: serde_json::json!({}),
                access_level: 2,
                compartments: vec![],
            },
        )]
        .into_iter()
        .collect()
    }

    fn example(raw: &str) -> serde_json::Value {
        serde_json::from_str(raw.trim_start_matches('\u{feff}')).expect("contract example parses")
    }

    #[test]
    fn the_contracts_count_example_parses_with_its_coverage() {
        let block = parse_matrix_result(&example(COUNT), "v");
        match &block {
            EvidenceBlock::Count(c) => {
                // A STRING in the contract, and deliberately so: the same
                // exact-decimal discipline that keeps 900000.50 distinct from
                // 900000.5.
                assert_eq!(c.value, 1284);
                assert_eq!(c.rows_covered, Some(1284));
                assert_eq!(c.rows_excluded, Some(17));
                assert!(c
                    .exclusion_reason
                    .as_deref()
                    .unwrap_or_default()
                    .contains("row policy denied"));
                assert!(c.evidence_id.is_some());
            }
            other => panic!("expected a count, got {}", other.kind_str()),
        }
        // A count with 17 rows excluded STILL supports a completeness claim,
        // because it says so: the exclusion is part of the evidence, not a
        // caveat the answer has to guess at.
        assert!(block.supports_completeness());
    }

    #[test]
    fn the_contracts_table_example_keeps_matrixs_own_row_ids() {
        // The sharpest thing in this parser. `identity.row_id_rule` is `keys`,
        // so a row's id is its KEY — "EMEA", not "r0003". Numbering them here
        // would mean the model cites the id it was shown while the checker
        // resolves an invented one, and every correct citation is rejected.
        let block = parse_matrix_result(&example(COMPLETE_TABLE), "v");
        match &block {
            EvidenceBlock::CompleteTable(t) => {
                assert_eq!(
                    t.columns,
                    vec!["region", "pipeline_amount", "opportunity_count"]
                );
                assert_eq!(t.row_ids, vec!["AMER", "APAC", "EMEA"]);
                assert!(!t.truncated);
                // Exact decimals survive as text.
                assert_eq!(t.rows[1][1].as_deref(), Some("1180250.50"));
                assert!(t.evidence_id.is_some());
            }
            other => panic!("expected a table, got {}", other.kind_str()),
        }
        assert!(block.supports_completeness());
    }

    #[test]
    fn a_truncated_manifest_beats_the_blocks_own_flag() {
        // The manifest is the SEALED statement; the block's `truncated` is a
        // convenience copy. If they disagree, believing the copy would let a
        // truncated read back a completeness claim.
        let mut body = example(COMPLETE_TABLE);
        body["manifest"]["completeness"]["truncated"] = serde_json::json!(true);
        body["truncated"] = serde_json::json!(false);
        let block = parse_matrix_result(&body, "v");
        assert!(
            !block.supports_completeness(),
            "the sealed manifest said truncated"
        );
    }

    #[test]
    fn the_contracts_refusal_example_keeps_its_code_and_drops_its_message() {
        let block = parse_matrix_result(&example(REFUSAL), "revenue");
        match block {
            EvidenceBlock::Refusal(r) => {
                assert_eq!(r.code, "required_evidence_not_permitted");
                assert_eq!(r.source.as_deref(), Some("revenue"));
                // Matrix's own message is written for a Matrix operator and
                // may name a source, class or column this caller has no
                // clearance for. The CODE is what an operator acts on.
                assert!(
                    !r.message.contains("research profile"),
                    "Matrix's message must not be forwarded verbatim: {}",
                    r.message
                );
            }
            other => panic!("expected a refusal, got {}", other.kind_str()),
        }
    }

    #[test]
    fn a_count_with_no_readable_value_refuses_rather_than_reporting_zero() {
        // 0 would be an answer. "I could not read it" is not the same answer,
        // and a count is exactly the shape a reader trusts most.
        let mut body = example(COUNT);
        body["value"] = serde_json::json!("not a number");
        match parse_matrix_result(&body, "v") {
            EvidenceBlock::Refusal(r) => assert_eq!(r.code, REFUSAL_UNAVAILABLE),
            other => panic!("expected a refusal, got {}", other.kind_str()),
        }
    }

    #[test]
    fn an_unrecognised_block_kind_refuses_instead_of_being_guessed_at() {
        let mut body = example(COUNT);
        body["kind"] = serde_json::json!("something_new");
        match parse_matrix_result(&body, "v") {
            EvidenceBlock::Refusal(r) => assert_eq!(r.code, REFUSAL_UNAVAILABLE),
            other => panic!("expected a refusal, got {}", other.kind_str()),
        }
    }

    #[test]
    fn null_and_the_empty_string_stay_distinct_cells() {
        let mut body = example(COMPLETE_TABLE);
        body["rows"] = serde_json::json!([
            {"row_id": "a", "cells": ["900000.50", null]},
            {"row_id": "b", "cells": ["900000.5", ""]}
        ]);
        match parse_matrix_result(&body, "v") {
            EvidenceBlock::CompleteTable(t) => {
                assert_ne!(
                    t.rows[0][0], t.rows[1][0],
                    "900000.50 and 900000.5 are different sealed values"
                );
                assert_eq!(t.rows[0][1], None, "NULL");
                assert_eq!(t.rows[1][1].as_deref(), Some(""), "the empty string");
            }
            other => panic!("expected a table, got {}", other.kind_str()),
        }
    }

    #[test]
    fn rows_without_a_sealed_id_fall_back_to_position() {
        // `position` is a legal row_id_rule, and 1-based matches how the
        // context renders them.
        let mut body = example(COMPLETE_TABLE);
        body["rows"] = serde_json::json!([{"cells": ["x", "1.00", "1"]}]);
        match parse_matrix_result(&body, "v") {
            EvidenceBlock::CompleteTable(t) => assert_eq!(t.row_ids, vec!["r0001"]),
            other => panic!("expected a table, got {}", other.kind_str()),
        }
    }

    #[test]
    fn matrix_only_serves_matrix_prefixed_sources() {
        let p = MatrixProvider {
            http: reqwest::Client::new(),
            base_url: "http://localhost:8180".into(),
            token: None,
            auth: test_auth(),
            views: test_views(),
            breaker: Arc::new(CircuitBreaker::default()),
            metrics: Arc::new(crate::metrics::Metrics::default()),
        };
        assert!(p.can_serve("matrix:revenue_by_region"));
        assert!(!p.can_serve("contracts"), "a collection is not a data view");
    }

    #[tokio::test]
    async fn an_unbound_layer_refuses_rather_than_calling_anything() {
        let p = MatrixProvider {
            http: reqwest::Client::new(),
            // Deliberately unroutable: reaching the network would be the bug.
            base_url: "http://127.0.0.1:1".into(),
            token: None,
            auth: test_auth(),
            views: test_views(),
            breaker: Arc::new(CircuitBreaker::default()),
            metrics: Arc::new(crate::metrics::Metrics::default()),
        };
        let layer = EvidenceLayer {
            name: "register".into(),
            sources: vec!["contracts".into()],
            requirement: munarium_core::hierarchy::LayerRequirement::Optional,
            role: munarium_core::hierarchy::AnswerRole::Primary,
            context_char_budget: None,
            preserve_complete_result: false,
            deadline_ms: None,
        };
        let intent = QueryIntent {
            question: "q".into(),
            kind: None,
            explicit: true,
            selections: Default::default(),
        };
        let block = p.fetch(&layer, &intent).await.expect("refusal, not error");
        match block {
            EvidenceBlock::Refusal(r) => assert_eq!(r.code, REFUSAL_UNBOUND),
            other => panic!("expected a refusal, got {}", other.kind_str()),
        }
    }

    #[tokio::test]
    async fn an_open_circuit_refuses_without_a_call() {
        let breaker = Arc::new(CircuitBreaker::new(1, Duration::from_secs(60)));
        breaker.record_failure();
        let p = MatrixProvider {
            http: reqwest::Client::new(),
            base_url: "http://127.0.0.1:1".into(),
            token: None,
            auth: test_auth(),
            views: test_views(),
            breaker,
            metrics: Arc::new(crate::metrics::Metrics::default()),
        };
        let layer = EvidenceLayer {
            name: "register".into(),
            sources: vec!["matrix:revenue_by_region".into()],
            requirement: munarium_core::hierarchy::LayerRequirement::Required,
            role: munarium_core::hierarchy::AnswerRole::Controlling,
            context_char_budget: None,
            preserve_complete_result: false,
            deadline_ms: None,
        };
        let intent = QueryIntent {
            question: "q".into(),
            kind: None,
            explicit: true,
            selections: Default::default(),
        };
        // 127.0.0.1:1 would fail fast anyway; the point is the refusal code
        // says the circuit was open, so this cost no connection attempt.
        let block = p.fetch(&layer, &intent).await.expect("refusal, not error");
        match block {
            EvidenceBlock::Refusal(r) => assert_eq!(r.code, REFUSAL_CIRCUIT_OPEN),
            other => panic!("expected a refusal, got {}", other.kind_str()),
        }
    }

    #[test]
    fn the_provider_metric_carries_no_tenant_label() {
        // Guards the module's own rule. The breaker is per instance; a tenant
        // label would report a per-tenant fact that does not exist, and would
        // leak one tenant's failures into another's scrape.
        let src = include_str!("evidence_providers.rs");
        let call = src
            .split("munarium_matrix_provider_requests_total")
            .nth(1)
            .expect("the metric is emitted");
        let window = &call[..call.len().min(200)];
        assert!(
            !window.contains("tenant"),
            "the Matrix provider metric must not be labelled by tenant"
        );
    }
}

#[cfg(test)]
mod semantic_body_tests {
    use super::*;
    use munarium_core::hierarchy::{SemanticFilterSelection, SemanticSelection};

    fn auth() -> SessionAuthorization {
        SessionAuthorization {
            tenant: "demo".into(),
            uid: "u".into(),
            access_level: 3,
            compartments: vec!["finance".into()],
            session_id: "s".into(),
            runbook_ref: "r@1".into(),
        }
    }

    #[test]
    fn a_semantic_view_posts_names_never_sql_and_binds_filters_by_type() {
        let view = BoundDataView {
            contract: "pipeline-by-region".into(),
            kind: munarium_runbooks::DataViewKind::DataView,
            parameters: serde_json::json!({}),
            access_level: 0,
            compartments: vec![],
        };
        let sel = SemanticSelection {
            measures: vec!["pipeline_amount".into()],
            dimensions: vec!["region".into()],
            filters: vec![SemanticFilterSelection {
                dimension: "stage".into(),
                value: "Proposal".into(),
                ty: "string".into(),
            }],
        };
        let body = semantic_or_contract_body(&view, Some(&sel), &auth());
        assert_eq!(body["kind"], "semantic");
        assert_eq!(body["semantic"]["provider"], "pipeline-by-region");
        assert_eq!(body["semantic"]["filters"][0]["op"], "eq");
        assert_eq!(body["semantic"]["filters"][0]["value"]["type"], "string");
        assert_eq!(
            body["authorization"]["access_level"], 0,
            "the view's ceiling wins"
        );
        assert!(body.get("contract").is_none());
    }

    #[test]
    fn a_contract_view_posts_the_structured_query_exactly_as_before() {
        let view = BoundDataView {
            contract: "open-pipeline-by-region@3".into(),
            kind: munarium_runbooks::DataViewKind::Contract,
            parameters: serde_json::json!({ "as_of": { "type": "date", "value": "2026-06-30" } }),
            access_level: 3,
            compartments: vec![],
        };
        let body = semantic_or_contract_body(&view, None, &auth());
        assert_eq!(body["kind"], "structured_query");
        assert_eq!(body["contract"], "open-pipeline-by-region@3");
        assert_eq!(body["parameters"]["as_of"]["value"], "2026-06-30");
    }
}
