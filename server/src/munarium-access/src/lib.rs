// SPDX-License-Identifier: Apache-2.0
//! munarium-access — deliberately-light capability tokens.
//!
//! munarium-server is NOT the identity provider. The enterprise API-management
//! layer in front authenticates users; it exchanges its long-lived `mgmt`
//! static token for a short-lived, least-privilege JWT minted here and
//! forwards that downstream. This crate holds the token mechanics and the
//! one authorization primitive (level + compartments — Bell-LaPadula
//! "simple security" with categories). See docs/security-posture.md.
//!
//! HS256 with one server-held secret; verification is local (no JWKS, no
//! OIDC, no introspection). Like munarium-core, this crate must never depend
//! on sqlx/axum/tonic/reqwest — CI greps the dependency tree.

use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

/// Scope required for the session/turn data plane.
pub const SCOPE_QUERY: &str = "query";
/// Scope required for the file-ingestion plane.
pub const SCOPE_INGEST: &str = "ingest";
/// Scope that lets a service principal FILE findings (`POST
/// /v1/versions/{id}/findings`) — warn/info only, never a gate. Held
/// by Munarium Matrix's token; a static rw token has it implicitly, an ro
/// token never does. Distinct from `ingest` so a reconciliation service
/// cannot upload documents and an uploader cannot write governance findings.
pub const SCOPE_FINDINGS: &str = "findings";
/// Scope that lets a service principal SEAL and RESOLVE evidence artifacts
/// (`POST /v1/evidence`, `GET /v1/evidence/{id}`) whose authorization
/// class the token's `lvl`/`cmp` dominate. Held by Munarium Matrix's token; a
/// static rw token has it implicitly, an ro token never does.
///
/// Note what this scope does NOT do: it never widens what the token may READ.
/// Domination is still checked per artifact, so an `evidence`-scoped token
/// under-cleared for an artifact is refused exactly as any other principal is.
/// The scope says "this service participates in the evidence plane"; the class
/// says "and only within this clearance".
pub const SCOPE_EVIDENCE: &str = "evidence";
/// Hard ceiling on token lifetime; issuance clamps to this.
pub const MAX_TTL_SECS: u64 = 86_400;
/// Default token lifetime when the deployment does not configure one.
pub const DEFAULT_TTL_SECS: u64 = 3_600;
/// Clock-skew allowance on `exp` at verification.
const LEEWAY_SECS: u64 = 30;

#[derive(Debug, thiserror::Error)]
pub enum AccessError {
    #[error("token expired")]
    Expired,
    #[error("invalid token: {0}")]
    Invalid(String),
}

/// The JWT claim set. Field names are the wire contract (kept terse on
/// purpose — these ride every data-plane request).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccessClaims {
    /// The end-user id asserted by the API manager (must match X-Munarium-Uid).
    pub sub: String,
    /// Tenant the token is scoped to.
    pub ten: String,
    /// Hierarchical access level; a collection at level L needs lvl >= L.
    pub lvl: i32,
    /// Need-to-know compartment tags; a collection's tags must be a subset.
    #[serde(default)]
    pub cmp: Vec<String>,
    /// Capabilities: "query" (sessions/turns) and/or "ingest" (file upload).
    pub scopes: Vec<String>,
    /// Optional runbook NAME allowlist; absent = any runbook the level permits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rb: Option<Vec<String>>,
    /// Token id ("tok-…") — the issuance-audit / revocation key.
    pub jti: String,
    pub iat: i64,
    pub exp: i64,
}

/// The verified principal a data-plane request acts as.
#[derive(Debug, Clone)]
pub struct AccessCtx {
    pub uid: String,
    pub tenant_id: String,
    pub level: i32,
    pub compartments: Vec<String>,
    /// True when the principal clears EVERY compartment, not just the ones it
    /// lists — the control-plane static rw/ro tokens and MUNARIUM_AUTH_MODE=disabled
    /// map here. A capability JWT is always `false`: it clears only the
    /// compartments it explicitly carries. Without this, an "unrestricted"
    /// ctx (empty compartment list) would be locked OUT of every
    /// compartmented collection, which is the opposite of unrestricted.
    pub all_compartments: bool,
    pub scopes: Vec<String>,
    pub runbooks: Option<Vec<String>>,
    pub jti: String,
}

impl From<AccessClaims> for AccessCtx {
    fn from(c: AccessClaims) -> Self {
        Self {
            uid: c.sub,
            tenant_id: c.ten,
            level: c.lvl,
            compartments: c.cmp,
            all_compartments: false,
            scopes: c.scopes,
            runbooks: c.rb,
            jti: c.jti,
        }
    }
}

impl AccessCtx {
    /// The simple-security property: the token sees a collection iff its
    /// level dominates AND it holds every compartment tag the collection
    /// carries. A collection with no compartments needs only the level; an
    /// `all_compartments` principal (static/disabled) clears the compartment
    /// gate unconditionally.
    pub fn permits(&self, level: i32, compartments: &[String]) -> bool {
        self.level >= level
            && (self.all_compartments
                || compartments
                    .iter()
                    .all(|c| self.compartments.iter().any(|have| have == c)))
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    /// `rb` allowlists runbook NAMES (not name@version refs) so one token
    /// spans a runbook's versions.
    pub fn permits_runbook(&self, name: &str) -> bool {
        match &self.runbooks {
            None => true,
            Some(list) => list.iter().any(|r| r == name),
        }
    }

    /// An AccessCtx that permits everything — the control-plane static
    /// rw token and MUNARIUM_AUTH_MODE=disabled map here. `all_compartments`
    /// clears the compartment gate, so this truly sees every collection.
    pub fn unrestricted(uid: &str, tenant_id: &str) -> Self {
        Self {
            uid: uid.to_string(),
            tenant_id: tenant_id.to_string(),
            level: i32::MAX,
            compartments: Vec::new(),
            all_compartments: true,
            scopes: vec![
                SCOPE_QUERY.to_string(),
                SCOPE_INGEST.to_string(),
                SCOPE_FINDINGS.to_string(),
                SCOPE_EVIDENCE.to_string(),
            ],
            runbooks: None,
            jti: String::new(),
        }
    }
}

/// Sign a claim set. The caller owns jti/iat/exp; `issue` is the convenience
/// that stamps them.
pub fn mint(secret: &[u8], claims: &AccessClaims) -> Result<String, AccessError> {
    encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|e| AccessError::Invalid(e.to_string()))
}

/// Verify signature + expiry (30 s leeway) and return the claims.
pub fn verify(secret: &[u8], token: &str) -> Result<AccessClaims, AccessError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = LEEWAY_SECS;
    validation.set_required_spec_claims(&["exp"]);
    decode::<AccessClaims>(token, &DecodingKey::from_secret(secret), &validation)
        .map(|data| data.claims)
        .map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => AccessError::Expired,
            _ => AccessError::Invalid(e.to_string()),
        })
}

/// Mint a token now: clamps ttl to [1, MAX_TTL_SECS], stamps iat/exp, and
/// returns (token, claims-as-stamped) so the issuer can audit-record exp.
#[allow(clippy::too_many_arguments)]
pub fn issue(
    secret: &[u8],
    uid: &str,
    tenant_id: &str,
    level: i32,
    compartments: Vec<String>,
    scopes: Vec<String>,
    runbooks: Option<Vec<String>>,
    ttl_secs: u64,
    jti: String,
) -> Result<(String, AccessClaims), AccessError> {
    if uid.trim().is_empty() {
        return Err(AccessError::Invalid("uid is required".into()));
    }
    if scopes.is_empty() {
        return Err(AccessError::Invalid(
            "at least one scope (query|ingest|findings|evidence) is required".into(),
        ));
    }
    for s in &scopes {
        if s != SCOPE_QUERY && s != SCOPE_INGEST && s != SCOPE_FINDINGS && s != SCOPE_EVIDENCE {
            return Err(AccessError::Invalid(format!(
                "unknown scope '{s}' (query|ingest|findings|evidence)"
            )));
        }
    }
    let ttl = ttl_secs.clamp(1, MAX_TTL_SECS);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| AccessError::Invalid(e.to_string()))?
        .as_secs() as i64;
    let claims = AccessClaims {
        sub: uid.to_string(),
        ten: tenant_id.to_string(),
        lvl: level,
        cmp: compartments,
        scopes,
        rb: runbooks,
        jti,
        iat: now,
        exp: now + ttl as i64,
    };
    let token = mint(secret, &claims)?;
    Ok((token, claims))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"unit-test-secret-at-least-32-bytes!!";

    fn claims(exp_offset: i64) -> AccessClaims {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        AccessClaims {
            sub: "user-1".into(),
            ten: "tenant-a".into(),
            lvl: 2,
            cmp: vec!["eng".into()],
            scopes: vec![SCOPE_QUERY.into()],
            rb: None,
            jti: "tok-1".into(),
            iat: now,
            exp: now + exp_offset,
        }
    }

    #[test]
    fn roundtrip() {
        let c = claims(600);
        let tok = mint(SECRET, &c).unwrap();
        let got = verify(SECRET, &tok).unwrap();
        assert_eq!(got, c);
    }

    #[test]
    fn expired_is_rejected_as_expired() {
        let c = claims(-120); // beyond the 30 s leeway
        let tok = mint(SECRET, &c).unwrap();
        match verify(SECRET, &tok) {
            Err(AccessError::Expired) => {}
            other => panic!("expected Expired, got {other:?}"),
        }
    }

    #[test]
    fn wrong_secret_is_invalid() {
        let tok = mint(SECRET, &claims(600)).unwrap();
        assert!(matches!(
            verify(b"a-different-secret-of-32-bytes!!!!!!", &tok),
            Err(AccessError::Invalid(_))
        ));
    }

    /// Minimal base64url (no padding) for forging test tokens.
    fn b64url(b: &[u8]) -> String {
        const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut s = String::new();
        for chunk in b.chunks(3) {
            let x = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((x[0] as u32) << 16) | ((x[1] as u32) << 8) | x[2] as u32;
            s.push(TABLE[(n >> 18) as usize & 63] as char);
            s.push(TABLE[(n >> 12) as usize & 63] as char);
            if chunk.len() > 1 {
                s.push(TABLE[(n >> 6) as usize & 63] as char);
            }
            if chunk.len() > 2 {
                s.push(TABLE[n as usize & 63] as char);
            }
        }
        s
    }

    #[test]
    fn tampered_payload_is_invalid() {
        // Re-sign nothing: splice a forged lvl=99 payload into a validly
        // signed token; the signature must stop matching.
        let tok = mint(SECRET, &claims(600)).unwrap();
        let mut parts: Vec<String> = tok.split('.').map(String::from).collect();
        let mut forged = claims(600);
        forged.lvl = 99;
        parts[1] = b64url(&serde_json::to_vec(&forged).unwrap());
        assert!(matches!(
            verify(SECRET, &parts.join(".")),
            Err(AccessError::Invalid(_))
        ));
    }

    #[test]
    fn alg_none_is_rejected() {
        // Header {"alg":"none"} with an empty signature must never verify.
        let payload = serde_json::to_vec(&claims(600)).unwrap();
        let tok = format!(
            "{}.{}.",
            b64url(br#"{"alg":"none","typ":"JWT"}"#),
            b64url(&payload)
        );
        assert!(verify(SECRET, &tok).is_err());
    }

    #[test]
    fn permits_truth_table() {
        let ctx = AccessCtx {
            uid: "u".into(),
            tenant_id: "t".into(),
            level: 2,
            compartments: vec!["eng".into(), "fin".into()],
            all_compartments: false,
            scopes: vec![SCOPE_QUERY.into()],
            runbooks: None,
            jti: "tok-1".into(),
        };
        // level dominance
        assert!(ctx.permits(0, &[]));
        assert!(ctx.permits(2, &[]));
        assert!(!ctx.permits(3, &[]));
        // compartment subset
        assert!(ctx.permits(1, &["eng".into()]));
        assert!(ctx.permits(1, &["eng".into(), "fin".into()]));
        assert!(!ctx.permits(1, &["legal".into()]));
        assert!(!ctx.permits(1, &["eng".into(), "legal".into()]));
        // both must hold
        assert!(!ctx.permits(3, &["eng".into()]));
    }

    #[test]
    fn unrestricted_clears_every_compartment() {
        // The control-plane static rw token / disabled-mode principal must
        // see compartmented collections (empty compartment list would
        // otherwise lock it out — the smoke-test regression).
        let ctx = AccessCtx::unrestricted("op", "t");
        assert!(ctx.all_compartments);
        assert!(ctx.permits(0, &["eng".into()]));
        assert!(ctx.permits(99, &["eng".into(), "fin".into(), "legal".into()]));
        // A JWT-derived ctx with no compartments does NOT clear them.
        let jwt: AccessCtx = AccessClaims {
            sub: "u".into(),
            ten: "t".into(),
            lvl: 99,
            cmp: vec![],
            scopes: vec![SCOPE_QUERY.into()],
            rb: None,
            jti: "tok".into(),
            iat: 0,
            exp: 0,
        }
        .into();
        assert!(!jwt.all_compartments);
        assert!(!jwt.permits(0, &["eng".into()]));
    }

    #[test]
    fn runbook_allowlist() {
        let mut ctx = AccessCtx::unrestricted("u", "t");
        assert!(ctx.permits_runbook("anything"));
        ctx.runbooks = Some(vec!["field-support".into()]);
        assert!(ctx.permits_runbook("field-support"));
        assert!(!ctx.permits_runbook("other"));
    }

    #[test]
    fn issue_clamps_and_validates() {
        let (tok, c) = issue(
            SECRET,
            "u1",
            "t1",
            1,
            vec![],
            vec![SCOPE_QUERY.into(), SCOPE_INGEST.into()],
            None,
            10 * MAX_TTL_SECS, // clamped
            "tok-x".into(),
        )
        .unwrap();
        assert_eq!(c.exp - c.iat, MAX_TTL_SECS as i64);
        assert!(verify(SECRET, &tok).is_ok());
        assert!(issue(
            SECRET,
            "",
            "t",
            0,
            vec![],
            vec![SCOPE_QUERY.into()],
            None,
            60,
            "j".into()
        )
        .is_err());
        assert!(issue(SECRET, "u", "t", 0, vec![], vec![], None, 60, "j".into()).is_err());
        assert!(issue(
            SECRET,
            "u",
            "t",
            0,
            vec![],
            vec!["admin".into()],
            None,
            60,
            "j".into()
        )
        .is_err());
    }
}
