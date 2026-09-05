// SPDX-License-Identifier: Apache-2.0
//! Rust client for Matrix's own REST API.
//!
//! Used by `mxctl` and by `conformance --http`, which is the point: the
//! scenarios are written once and run in-process or over the wire, so a
//! scenario is never written twice and the two modes cannot drift.

#![forbid(unsafe_code)]

use munarium_matrix_types::dto::*;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("transport: {0}")]
    Transport(String),
    /// The server answered with problem+json. The slug is the stable identity;
    /// never key on the English title.
    #[error("{}: {}", .0.slug(), .0.detail)]
    Problem(Box<Problem>),
    #[error("unexpected response: {0}")]
    Malformed(String),
}

pub type Result<T> = std::result::Result<T, ClientError>;

/// Is this failure "there is no metric view by that name", so a data view is
/// worth trying?
///
/// Keyed on the STATUS and the refusal code, not on substrings of the
/// rendered message. The old test — `to_string().contains("404")` — matched
/// nothing that actually happens: `/v1/metricviews/{name}/verify` loads
/// through the runtime, which turns a registry miss into
/// `Refusal::not_covered`, and that is **422**. So the data-view fallback was
/// unreachable, and `verify_view` on a native data view reported "no
/// MetricView named 'x' is registered" instead of verifying it. A
/// `metric_view_changed` 422 is deliberately NOT matched: that is a real
/// answer about a view that exists, and retrying it as something else would
/// replace a precise refusal with a wrong one.
fn no_such_view(e: &ClientError) -> bool {
    let ClientError::Problem(p) = e else {
        return false;
    };
    if p.status == 404 {
        return true;
    }
    p.status == 422
        && p.refusal
            .as_ref()
            .and_then(|r| r.get("code"))
            .and_then(|c| c.as_str())
            == Some("not_covered")
}

pub struct MatrixClient {
    base_url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl std::fmt::Debug for MatrixClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MatrixClient")
            .field("base_url", &self.base_url)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl MatrixClient {
    pub fn new(base_url: &str, token: Option<&str>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.map(String::from),
            http: reqwest::Client::new(),
        }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let r = self
            .http
            .request(method, format!("{}{}", self.base_url, path));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    async fn send<T: serde::de::DeserializeOwned>(req: reqwest::RequestBuilder) -> Result<T> {
        let resp = req
            .send()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(match serde_json::from_str::<Problem>(&text) {
                Ok(p) => ClientError::Problem(Box::new(p)),
                Err(_) => ClientError::Malformed(format!("{status}: {text:.300}")),
            });
        }
        serde_json::from_str(&text).map_err(|e| ClientError::Malformed(format!("{e}: {text:.300}")))
    }

    pub async fn version(&self) -> Result<VersionResponse> {
        Self::send(self.req(reqwest::Method::GET, "/version")).await
    }

    pub async fn healthz(&self) -> Result<bool> {
        let v: serde_json::Value = Self::send(self.req(reqwest::Method::GET, "/healthz")).await?;
        Ok(v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false))
    }

    pub async fn apply(&self, yaml: &str) -> Result<ApplyResponse> {
        Self::send(
            self.req(reqwest::Method::POST, "/v1/assets")
                .header("content-type", "text/yaml")
                .body(yaml.to_string()),
        )
        .await
    }

    pub async fn validate(&self, yaml: &str) -> Result<ValidateResponse> {
        Self::send(
            self.req(reqwest::Method::POST, "/v1/assets/validate")
                .header("content-type", "text/yaml")
                .body(yaml.to_string()),
        )
        .await
    }

    pub async fn list(&self, kind_path: &str, all_versions: bool) -> Result<AssetListResponse> {
        let path = format!(
            "/v1/{kind_path}{}",
            if all_versions {
                "?all_versions=true"
            } else {
                ""
            }
        );
        Self::send(self.req(reqwest::Method::GET, &path)).await
    }

    /// The applied YAML back, verbatim.
    pub async fn get_yaml(&self, kind_path: &str, name: &str) -> Result<String> {
        let resp = self
            .req(reqwest::Method::GET, &format!("/v1/{kind_path}/{name}"))
            .send()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(match serde_json::from_str::<Problem>(&text) {
                Ok(p) => ClientError::Problem(Box::new(p)),
                Err(_) => ClientError::Malformed(format!("{status}: {text:.200}")),
            });
        }
        Ok(text)
    }

    /// Run a contract's verified questions.
    ///
    /// The HTTP call succeeds even when questions fail — a failed question is a
    /// result, not a transport error — so the caller inspects `failed`. Only a
    /// contract that could not be run at all comes back as `Err`.
    pub async fn verify(&self, contract: &str) -> Result<VerifyResponse> {
        Self::send(self.req(
            reqwest::Method::POST,
            &format!("/v1/contracts/{contract}/verify"),
        ))
        .await
    }

    /// Run a metric view's verified questions under the definition the source
    /// reports now, and record that definition's fingerprint.
    pub async fn verify_view(&self, view: &str) -> Result<VerifyResponse> {
        // A metric view first; a native data view when there is none by that
        // name. Any other failure is reported as it came.
        match Self::send(self.req(
            reqwest::Method::POST,
            &format!("/v1/metricviews/{view}/verify"),
        ))
        .await
        {
            Ok(r) => Ok(r),
            Err(e) if no_such_view(&e) => {
                Self::send(self.req(
                    reqwest::Method::POST,
                    &format!("/v1/dataviews/{view}/verify"),
                ))
                .await
            }
            Err(e) => Err(e),
        }
    }

    /// Enqueue a sync run, one job per authorization class.
    pub async fn sync(&self, source: &str) -> Result<JobAccepted> {
        Self::send(self.req(
            reqwest::Method::POST,
            &format!("/v1/datasources/{source}/sync"),
        ))
        .await
    }

    pub async fn promotion_status(&self, mapping: &str) -> Result<PromotionStatus> {
        Self::send(self.req(
            reqwest::Method::GET,
            &format!("/v1/mappings/{mapping}/promotion"),
        ))
        .await
    }

    /// The promotion gates over time — the monitoring surface for the
    /// thresholds (Q8: 0.95 confirmed *with monitoring*).
    pub async fn gate_history(&self, mapping: &str, limit: Option<i64>) -> Result<GateHistory> {
        let q = limit.map(|n| format!("?limit={n}")).unwrap_or_default();
        Self::send(self.req(
            reqwest::Method::GET,
            &format!("/v1/mappings/{mapping}/gate-history{q}"),
        ))
        .await
    }

    pub async fn promote(
        &self,
        mapping: &str,
        decision_id: &str,
        reason: Option<&str>,
    ) -> Result<PromotionStatus> {
        Self::send(
            self.req(
                reqwest::Method::POST,
                &format!("/v1/mappings/{mapping}/promote"),
            )
            .json(&PromoteRequest {
                decision_id: decision_id.to_string(),
                actor: None,
                reason: reason.map(str::to_string),
            }),
        )
        .await
    }

    pub async fn demote(&self, mapping: &str, decision_id: &str) -> Result<PromotionStatus> {
        Self::send(
            self.req(
                reqwest::Method::POST,
                &format!("/v1/mappings/{mapping}/demote"),
            )
            .json(&DecisionRequest {
                decision_id: decision_id.to_string(),
            }),
        )
        .await
    }

    pub async fn rollback(&self, mapping: &str, decision_id: &str) -> Result<RollbackResponse> {
        Self::send(
            self.req(
                reqwest::Method::POST,
                &format!("/v1/mappings/{mapping}/rollback"),
            )
            .json(&DecisionRequest {
                decision_id: decision_id.to_string(),
            }),
        )
        .await
    }

    /// Enqueue a reconcile pass.
    pub async fn reconcile(&self, mapping: &str) -> Result<JobAccepted> {
        Self::send(self.req(
            reqwest::Method::POST,
            &format!("/v1/mappings/{mapping}/run"),
        ))
        .await
    }

    pub async fn journal(&self, limit: usize) -> Result<JournalListResponse> {
        Self::send(self.req(reqwest::Method::GET, &format!("/v1/journal?limit={limit}"))).await
    }

    pub async fn healthdata(&self) -> Result<HealthDataResponse> {
        Self::send(self.req(reqwest::Method::GET, "/healthdata")).await
    }

    // -- the query plane -----------------------------------------------------
    //
    // Absent until 2026-08-30, which made this client a registry-and-operations
    // client wearing the name of an API client. `mxctl` and the conformance
    // crate reached `execute` with a hand-rolled `reqwest` call each, so the
    // one surface a customer actually calls was the one surface not covered —
    // and §21's "the Matrix Rust client covers Matrix's API" was not true.

    /// `POST /v1/{kind}/{name}/execute` — run a contract, a metric view or a
    /// native data view.
    ///
    /// One method for all three because the SERVICE has one handler for all
    /// three: the intent's `kind` selects the path, and the route segment only
    /// says which asset registry to look in. A client with three near-identical
    /// methods would imply a distinction the service does not draw.
    ///
    /// A refusal arrives as `Err(ClientError::Problem)`, exactly as it does
    /// over gRPC as a `Refusal` message: the transport differs, the answer does
    /// not.
    pub async fn execute(
        &self,
        kind: AssetRoute,
        name: &str,
        intent: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        Self::send(
            self.req(
                reqwest::Method::POST,
                &format!("/v1/{}/{name}/execute", kind.segment()),
            )
            .json(intent),
        )
        .await
    }

    // -- sources -------------------------------------------------------------

    /// `POST /v1/datasources/{name}/probe` — reachable, right now?
    ///
    /// A refusal is an ANSWER here rather than an error: `reachable: false`
    /// with a typed reason is what was asked for, so this returns `Ok` for an
    /// unreachable source and `Err` only when the call itself could not be
    /// made.
    pub async fn probe(&self, source: &str) -> Result<ProbeResponse> {
        Self::send(self.req(
            reqwest::Method::POST,
            &format!("/v1/datasources/{source}/probe"),
        ))
        .await
    }

    /// `POST /v1/datasources/{name}/introspect` — prove the role posture and
    /// read the schema as the effective principal sees it.
    pub async fn introspect(&self, source: &str) -> Result<IntrospectResponse> {
        Self::send(self.req(
            reqwest::Method::POST,
            &format!("/v1/datasources/{source}/introspect"),
        ))
        .await
    }

    /// `POST /v1/datasources/{name}/planner/ask` — ask a conversational
    /// planner a question.
    ///
    /// It executes nothing. `admitted_sql` is what the allowlist let through,
    /// for the caller to run through a contract; `plan_pinned` is false
    /// everywhere today, and the response says in words what that means.
    pub async fn planner_ask(
        &self,
        source: &str,
        question: &str,
        mode: Option<&str>,
    ) -> Result<PlannerAskResponse> {
        Self::send(
            self.req(
                reqwest::Method::POST,
                &format!("/v1/datasources/{source}/planner/ask"),
            )
            .json(&PlannerAskRequest {
                question: question.to_string(),
                mode: mode.map(str::to_string),
            }),
        )
        .await
    }
}

/// Which asset registry a route segment names.
///
/// A closed enum rather than a `&str`, because the three segments are the
/// service's own vocabulary and a typo in one is a 404 at run time rather than
/// a compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetRoute {
    Contracts,
    MetricViews,
    DataViews,
    DataSources,
    Mappings,
}

impl AssetRoute {
    pub fn segment(self) -> &'static str {
        match self {
            AssetRoute::Contracts => "contracts",
            AssetRoute::MetricViews => "metricviews",
            AssetRoute::DataViews => "dataviews",
            AssetRoute::DataSources => "datasources",
            AssetRoute::Mappings => "mappings",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `/v1` route the server mounts is reachable from this client.
    ///
    /// The guard that would have caught the real gap: until 2026-08-30 this
    /// client covered the registry and the operations and NOT the query plane,
    /// so `execute` — the one surface a customer actually calls — was reached
    /// by a hand-rolled `reqwest` call in two places instead. A count is not
    /// enough; this scrapes the server's own router and checks each path has a
    /// method that names it.
    ///
    /// Deliberately crude, and deliberately not a `cargo tree`-style import of
    /// the server crate: ground rule 1 forbids that edge, so the check reads
    /// the file. A path that moves breaks this test, which is the point.
    #[test]
    fn every_served_v1_route_is_reachable_from_this_client() {
        let router = include_str!("../../munarium-matrix-server/src/rest.rs");
        let me = include_str!("lib.rs");

        let mut missing: Vec<String> = Vec::new();
        let mut seen = 0usize;
        for line in router.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix(".route(\"/v1/") else {
                continue;
            };
            let Some(end) = rest.find('"') else { continue };
            let path = &rest[..end];
            seen += 1;
            // The tail after the last `{...}` placeholder is what a client
            // method's format string ends with: `promotion`, `execute`,
            // `gate-history`, or the bare collection name.
            let tail = path.rsplit('/').next().unwrap_or(path);
            let needle = if tail.starts_with('{') {
                // `/v1/contracts/{name}` — covered by `get_yaml`, which takes
                // the collection as an argument.
                path.split('/').next().unwrap_or(path).to_string()
            } else {
                tail.to_string()
            };
            if !me.contains(&needle) {
                missing.push(path.to_string());
            }
        }
        // A scrape that finds nothing passes trivially, which is the failure
        // mode of every check like this. The floor is the guard on the guard.
        assert!(
            seen > 20,
            "the route scrape found only {seen} /v1 routes — it has drifted from rest.rs"
        );
        assert!(
            missing.is_empty(),
            "served /v1 routes this client cannot reach: {missing:?}"
        );
    }

    #[test]
    fn every_asset_route_segment_matches_the_service() {
        // The segments the server mounts. A typo here is a 404 at run time,
        // which is why they are an enum and why this test exists.
        assert_eq!(AssetRoute::Contracts.segment(), "contracts");
        assert_eq!(AssetRoute::MetricViews.segment(), "metricviews");
        assert_eq!(AssetRoute::DataViews.segment(), "dataviews");
        assert_eq!(AssetRoute::DataSources.segment(), "datasources");
        assert_eq!(AssetRoute::Mappings.segment(), "mappings");
    }

    #[test]
    fn debug_never_prints_the_token() {
        let c = MatrixClient::new("http://x", Some("secret-token-value"));
        let s = format!("{c:?}");
        assert!(!s.contains("secret-token-value"), "{s}");
    }

    fn problem(status: u16, code: Option<&str>, detail: &str) -> ClientError {
        let mut p = Problem::new("x", status, "t", detail);
        if let Some(c) = code {
            p.refusal = Some(serde_json::json!({ "class": "not_covered", "code": c }));
        }
        ClientError::Problem(Box::new(p))
    }

    #[test]
    fn a_missing_metric_view_is_the_422_the_runtime_actually_produces() {
        // The bug this pins: `to_string().contains("404")` matched nothing
        // that happens, so the data-view fallback was unreachable and
        // `verify_view` on a native data view reported the metric view's
        // absence instead of verifying it.
        assert!(no_such_view(&problem(
            422,
            Some("not_covered"),
            "no MetricView named 'pipeline-by-region' is registered"
        )));
        assert!(no_such_view(&problem(404, None, "not found")));
    }

    #[test]
    fn a_changed_definition_is_not_retried_as_a_data_view() {
        // A real answer about a view that EXISTS. Retrying it as something
        // else replaces a precise refusal with a wrong one.
        assert!(!no_such_view(&problem(
            422,
            Some("metric_view_changed"),
            "the definition moved since it was verified"
        )));
        assert!(!no_such_view(&problem(
            403,
            Some("policy_denied"),
            "denied"
        )));
        assert!(!no_such_view(&ClientError::Transport("refused".into())));
    }
}
