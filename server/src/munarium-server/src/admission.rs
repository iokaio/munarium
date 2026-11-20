// SPDX-License-Identifier: Apache-2.0
//! Admission by work class (ClearView R7/B3).
//!
//! Every admitted request on a plane — `/v1/` and `/v1.2/` on REST, typed
//! `/mmp.v1.*` services on gRPC — is charged one slot against that plane's
//! ceiling, `MUNARIUM_MAX_CONCURRENCY`. This module splits that ceiling into
//! classes WITHOUT adding to it:
//!
//! - **ordinary** slots: `ceiling - control_reserve`. Every class tries here
//!   first, and every request that cannot be admitted is shed immediately with
//!   503 `overloaded` (REST, `Retry-After: 1`) or the gRPC overloaded reject.
//!   There is no queueing.
//! - **control reserve** (`MUNARIUM_CONTROL_RESERVE`, default 0 = off): taken
//!   only by an allowlisted control operation, only when no ordinary slot is
//!   free, and only AFTER the route's own authentication and role check pass.
//! - **heavy bound** (`MUNARIUM_HEAVY_CONCURRENCY`, default 0 = off): an extra
//!   token a heavy request must hold IN ADDITION to an ordinary slot, so heavy
//!   work is bounded inside the ordinary capacity. Heavy work never uses the
//!   reserve.
//!
//! The combined count of held slots is therefore at most
//! `(ceiling - reserve) + reserve = ceiling` by construction.
//!
//! With neither setting configured there is one pool per plane of size
//! `MUNARIUM_MAX_CONCURRENCY`, exactly as in v1.4.
//!
//! Classification is an explicit allowlist keyed on HTTP method plus the axum
//! route TEMPLATE (REST) or the full gRPC method path. A route that is not on a
//! list — including any route added later — is ordinary and can never take a
//! reserved slot. The native bridge (`ServerApiService`) replays each call
//! through `rest::router`, so a bridged call is classified by its REST route
//! and charged once, by the REST capture middleware.
//!
//! The console is outside the capture middleware's admission. Its one action
//! that runs heavy work inline, the run-step approval confirmation, admits
//! itself as heavy on the REST plane through [`admit_console`].
//!
//! The settings are read once at startup. There is no live reconfiguration,
//! so no limit can be lowered under in-flight work; a restart applies a new
//! value to new work only.

use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// The authority a control operation's own handler requires. The reserve is
/// granted only when the caller passes exactly that check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAuth {
    /// `auth_ctx` + `TenantCtx::require_rw`.
    Rw,
    /// `auth_ctx` + `TenantCtx::require_mgmt`.
    Mgmt,
    /// `auth_ctx` alone: any authenticated static-token role.
    Authenticated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkClass {
    Ordinary,
    Control(ControlAuth),
    Heavy,
}

impl WorkClass {
    pub fn label(self) -> &'static str {
        match self {
            WorkClass::Ordinary => "ordinary",
            WorkClass::Control(_) => "control",
            WorkClass::Heavy => "heavy",
        }
    }
}

/// REST control operations (B3-T2): eligible for the reserve after the
/// route's own authentication and role check. Method + route template.
pub const REST_CONTROL: &[(&str, &str, ControlAuth)] = &[
    ("PUT", "/v1/retrieval-rollout", ControlAuth::Rw),
    (
        "POST",
        "/v1/index-build-jobs/{job_id}/cancel",
        ControlAuth::Rw,
    ),
    (
        "POST",
        "/v1/index-artifacts/{index_version_id}/promote",
        ControlAuth::Rw,
    ),
    ("POST", "/v1/access-tokens/{jti}/revoke", ControlAuth::Mgmt),
    // ClearView IR-4: rollback calls no endpoint and must work under load.
    (
        "POST",
        "/v1/collections/{id}/inference/rollback",
        ControlAuth::Mgmt,
    ),
    (
        "GET",
        "/v1/index-build-jobs/{job_id}",
        ControlAuth::Authenticated,
    ),
    (
        "GET",
        "/v1/retrieval-rollout/{scope_kind}/{scope_id}",
        ControlAuth::Authenticated,
    ),
    (
        "GET",
        "/v1/index-artifacts/{index_version_id}",
        ControlAuth::Authenticated,
    ),
];

/// REST heavy operations (B3-T2): bounded by the heavy limit inside the
/// ordinary capacity; never eligible for the reserve. Run-step approval is
/// heavy, not control, because approval resumes the run inline.
pub const REST_HEAVY: &[(&str, &str)] = &[
    ("POST", "/v1/indexes/{shape_ref}/build"),
    ("POST", "/v1/runbooks/{name}/runs"),
    ("POST", "/v1/runs/{run_id}/steps/{ordinal}/approve"),
    ("POST", "/v1.2/collections/{id}/vocabulary/refresh"),
    ("POST", "/v1/index-artifacts/{index_version_id}/rebuild"),
    ("POST", "/v1/index-artifacts/{index_version_id}/verify"),
    ("POST", "/v1/index-artifacts/backfill"),
    ("POST", "/v1/sessions/{id}/turns"),
    ("POST", "/v1/sessions/{id}/turns/stream"),
    ("POST", "/v1.2/answers"),
    ("POST", "/v1/providers/{name}/complete"),
    ("POST", "/v1/providers/{name}/embed"),
    // ClearView IR-1: the probe makes outbound inference calls.
    ("POST", "/v1/providers/{name}/inference-probe"),
    ("PUT", "/v1/sources"),
    ("POST", "/v1/ingest"),
    ("POST", "/v1/ingest/batch"),
    ("POST", "/v1/ingest/bulk"),
    ("POST", "/v1/ingest/bulk/{bulk_id}/chunk"),
    ("PUT", "/v1/evidence/{evidence_id}/bytes"),
];

/// Console (`/admin`) routes that run heavy work inline (the R7 console
/// finding). The capture middleware never admits `/admin`, so these handlers
/// admit themselves on the REST plane as [`WorkClass::Heavy`], after their own
/// cheap checks (role, view-only, CSRF, typed confirmation, rw credential) and
/// before any store access. The console run-step approval resumes the run
/// inline exactly like `POST /v1/runs/{run_id}/steps/{ordinal}/approve`, which
/// is on [`REST_HEAVY`]. Kept apart from that list because the console routes
/// are not OpenAPI operations.
pub const CONSOLE_HEAVY: &[(&str, &str)] = &[(
    "POST",
    "/admin/runs/{run_id}/steps/{ordinal}/approve/confirm",
)];

/// Admit one console action listed in [`CONSOLE_HEAVY`] on the REST plane:
/// an ordinary slot plus, when `MUNARIUM_HEAVY_CONCURRENCY` is on, a heavy
/// token. Never the control reserve. `None` is a shed, counted in the same
/// metrics as a shed `/v1` request; the caller answers 503 with
/// `Retry-After: 1`. A route that is not listed is refused (shed) rather than
/// silently admitted.
pub fn admit_console(
    state: &crate::state::AppState,
    method: &str,
    route: &str,
) -> Option<AdmissionPermit> {
    if !CONSOLE_HEAVY
        .iter()
        .any(|(m, r)| *m == method && *r == route)
    {
        return None;
    }
    state
        .admission
        .rest
        .admit(&state.metrics, WorkClass::Heavy, |_| false)
}

/// Typed gRPC control methods (full method path).
pub const GRPC_CONTROL: &[(&str, ControlAuth)] =
    &[("/mmp.v1.AdminService/RevokeAccessToken", ControlAuth::Mgmt)];

/// Typed gRPC heavy methods (full method path).
pub const GRPC_HEAVY: &[&str] = &[
    "/mmp.v1.IngestService/PutSource",
    "/mmp.v1.IngestService/IngestFiles",
    "/mmp.v1.ProviderService/Complete",
    "/mmp.v1.ProviderService/Embed",
    "/mmp.v1.RunbookService/RunRunbook",
    "/mmp.v1.RunbookService/ApproveStep",
    "/mmp.v1.SessionService/Turn",
];

/// The class of a REST request, by method and matched route TEMPLATE.
pub fn rest_class(method: &str, route: &str) -> WorkClass {
    if let Some((_, _, auth)) = REST_CONTROL
        .iter()
        .find(|(m, r, _)| *m == method && *r == route)
    {
        return WorkClass::Control(*auth);
    }
    if REST_HEAVY.iter().any(|(m, r)| *m == method && *r == route) {
        return WorkClass::Heavy;
    }
    WorkClass::Ordinary
}

/// The class of a typed gRPC call, by full method path.
pub fn grpc_class(path: &str) -> WorkClass {
    if let Some((_, auth)) = GRPC_CONTROL.iter().find(|(p, _)| *p == path) {
        return WorkClass::Control(*auth);
    }
    if GRPC_HEAVY.contains(&path) {
        return WorkClass::Heavy;
    }
    WorkClass::Ordinary
}

/// Whether `ctx` passes the role check `need` names. Mirrors the handlers:
/// `TenantCtx::require_rw` / `require_mgmt`; `Authenticated` needs only a
/// successful `AppState::authenticate`.
pub fn authorized(state: &crate::state::AppState, bearer: Option<&str>, need: ControlAuth) -> bool {
    let Ok(ctx) = state.authenticate(bearer) else {
        return false;
    };
    match need {
        ControlAuth::Rw => ctx.require_rw().is_ok(),
        ControlAuth::Mgmt => ctx.require_mgmt().is_ok(),
        ControlAuth::Authenticated => true,
    }
}

/// The validated admission settings. Applied to EACH plane separately, the
/// way `MUNARIUM_MAX_CONCURRENCY` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionConfig {
    /// `MUNARIUM_MAX_CONCURRENCY`: every class together, per plane.
    pub ceiling: usize,
    /// `MUNARIUM_CONTROL_RESERVE`: carved out of `ceiling`. 0 = off.
    pub control_reserve: usize,
    /// `MUNARIUM_HEAVY_CONCURRENCY`: heavy requests in flight, inside the
    /// ordinary capacity. 0 = off (heavy bounded only by the ordinary pool).
    pub heavy_limit: usize,
}

impl AdmissionConfig {
    /// The v1.4 shape: one pool of `ceiling` per plane.
    pub fn legacy(ceiling: usize) -> Self {
        Self {
            ceiling,
            control_reserve: 0,
            heavy_limit: 0,
        }
    }

    pub fn new(ceiling: usize, control_reserve: usize, heavy_limit: usize) -> Result<Self, String> {
        if ceiling == 0 {
            return Err(
                "MUNARIUM_MAX_CONCURRENCY must be >= 1 (0 would refuse every request)".into(),
            );
        }
        if control_reserve >= ceiling {
            return Err(format!(
                "MUNARIUM_CONTROL_RESERVE ({control_reserve}) must be smaller than \
                 MUNARIUM_MAX_CONCURRENCY ({ceiling}): the reserve is carved out of the \
                 per-plane ceiling and must leave at least one ordinary slot"
            ));
        }
        let ordinary = ceiling - control_reserve;
        if heavy_limit > ordinary {
            return Err(format!(
                "MUNARIUM_HEAVY_CONCURRENCY ({heavy_limit}) must not exceed the ordinary \
                 capacity MUNARIUM_MAX_CONCURRENCY - MUNARIUM_CONTROL_RESERVE ({ordinary}); \
                 heavy work is bounded inside the ordinary slots (0 turns the bound off)"
            ));
        }
        Ok(Self {
            ceiling,
            control_reserve,
            heavy_limit,
        })
    }

    /// Reads `MUNARIUM_CONTROL_RESERVE` and `MUNARIUM_HEAVY_CONCURRENCY`
    /// through `lookup` (the process environment in production). Unset or
    /// empty means 0, which is off.
    pub fn from_lookup(
        ceiling: usize,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let read = |key: &str| -> Result<usize, String> {
            match lookup(key) {
                None => Ok(0),
                Some(v) if v.trim().is_empty() => Ok(0),
                Some(v) => v.trim().parse().map_err(|e| format!("{key}: {e}")),
            }
        };
        Self::new(
            ceiling,
            read("MUNARIUM_CONTROL_RESERVE")?,
            read("MUNARIUM_HEAVY_CONCURRENCY")?,
        )
    }

    pub fn from_env(ceiling: usize) -> Result<Self, String> {
        Self::from_lookup(ceiling, |k| std::env::var(k).ok())
    }

    pub fn ordinary(&self) -> usize {
        self.ceiling - self.control_reserve
    }
}

/// A granted admission. Holding it holds the slot (and the heavy token, for
/// heavy work); dropping it releases both.
#[derive(Debug)]
pub struct AdmissionPermit {
    _slot: OwnedSemaphorePermit,
    _heavy: Option<OwnedSemaphorePermit>,
}

/// One plane's pools: `ordinary`, the control `reserve` and the optional
/// `heavy` bound.
#[derive(Debug)]
pub struct PlaneAdmission {
    pub plane: &'static str,
    pub ordinary: Arc<Semaphore>,
    pub reserve: Arc<Semaphore>,
    pub heavy: Option<Arc<Semaphore>>,
    config: AdmissionConfig,
}

/// Which slot a grant used, for the per-class metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Ordinary,
    Reserve,
}

impl Slot {
    fn label(self) -> &'static str {
        match self {
            Slot::Ordinary => "ordinary",
            Slot::Reserve => "reserve",
        }
    }
}

impl PlaneAdmission {
    pub fn new(plane: &'static str, ordinary: Arc<Semaphore>, config: AdmissionConfig) -> Self {
        Self {
            plane,
            ordinary,
            reserve: Arc::new(Semaphore::new(config.control_reserve)),
            heavy: (config.heavy_limit > 0).then(|| Arc::new(Semaphore::new(config.heavy_limit))),
            config,
        }
    }

    /// Try to admit one request of `class`, never waiting.
    ///
    /// An ordinary slot is tried first for every class. Only a control request
    /// that found none, on a plane with a reserve, calls `authorize`; the
    /// reserve is taken only when it returns true. Heavy work also needs a
    /// heavy token when the heavy bound is on, and never touches the reserve.
    pub fn try_admit(
        &self,
        class: WorkClass,
        authorize: impl FnOnce(ControlAuth) -> bool,
    ) -> Option<(AdmissionPermit, Slot)> {
        let heavy = match (class, &self.heavy) {
            (WorkClass::Heavy, Some(h)) => Some(h.clone().try_acquire_owned().ok()?),
            _ => None,
        };
        if let Ok(slot) = self.ordinary.clone().try_acquire_owned() {
            return Some((
                AdmissionPermit {
                    _slot: slot,
                    _heavy: heavy,
                },
                Slot::Ordinary,
            ));
        }
        let WorkClass::Control(need) = class else {
            return None;
        };
        if self.config.control_reserve == 0 || self.reserve.available_permits() == 0 {
            return None;
        }
        if !authorize(need) {
            return None;
        }
        let slot = self.reserve.clone().try_acquire_owned().ok()?;
        Some((
            AdmissionPermit {
                _slot: slot,
                _heavy: None,
            },
            Slot::Reserve,
        ))
    }

    /// Admit with metrics: one `munarium_admission_total` series per
    /// (plane, class, slot) on success; `munarium_load_shed_total` (unchanged
    /// from v1.4) plus `munarium_admission_shed_total{plane,class}` on a shed.
    pub fn admit(
        &self,
        metrics: &crate::metrics::Metrics,
        class: WorkClass,
        authorize: impl FnOnce(ControlAuth) -> bool,
    ) -> Option<AdmissionPermit> {
        match self.try_admit(class, authorize) {
            Some((permit, slot)) => {
                metrics.inc(
                    "munarium_admission_total",
                    crate::metrics::labels(&[
                        ("plane", self.plane),
                        ("class", class.label()),
                        ("slot", slot.label()),
                    ]),
                );
                Some(permit)
            }
            None => {
                metrics.inc("munarium_load_shed_total", String::new());
                metrics.inc(
                    "munarium_admission_shed_total",
                    crate::metrics::labels(&[("plane", self.plane), ("class", class.label())]),
                );
                None
            }
        }
    }

    /// A heavy token alone, for work already holding an ordinary slot: the
    /// MCP endpoint admits its request as ordinary before reading the body
    /// and takes the heavy token once it knows the tool's native route is
    /// heavy (`rest_class`), so the inner dispatch, which the capture
    /// middleware does not admit again, is bounded exactly like the native
    /// route. `Some(None)` when no heavy bound is configured.
    pub fn try_heavy(&self) -> Option<Option<OwnedSemaphorePermit>> {
        match &self.heavy {
            None => Some(None),
            Some(h) => h.clone().try_acquire_owned().ok().map(Some),
        }
    }

    /// (pool, capacity, in use) for the polled gauges. The reserve and heavy
    /// pools appear only when configured.
    pub fn usage(&self) -> Vec<(&'static str, usize, usize)> {
        let in_use = |s: &Semaphore, cap: usize| cap.saturating_sub(s.available_permits());
        let ordinary = self.config.ordinary();
        let mut out = vec![("ordinary", ordinary, in_use(&self.ordinary, ordinary))];
        if self.config.control_reserve > 0 {
            let cap = self.config.control_reserve;
            out.push(("reserve", cap, in_use(&self.reserve, cap)));
        }
        if let Some(h) = &self.heavy {
            let cap = self.config.heavy_limit;
            out.push(("heavy", cap, in_use(h, cap)));
        }
        out
    }
}

/// Both planes' admission.
#[derive(Debug)]
pub struct Admission {
    pub rest: PlaneAdmission,
    pub grpc: PlaneAdmission,
}

impl Admission {
    /// Builds the per-plane pools.
    pub fn new(config: AdmissionConfig) -> Self {
        let rest = Arc::new(Semaphore::new(config.ordinary()));
        let grpc = Arc::new(Semaphore::new(config.ordinary()));
        Self {
            rest: PlaneAdmission::new("rest", rest, config),
            grpc: PlaneAdmission::new("grpc", grpc, config),
        }
    }
}
