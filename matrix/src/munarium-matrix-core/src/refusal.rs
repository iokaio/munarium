// SPDX-License-Identifier: Apache-2.0
//! The refusal taxonomy (G7).
//!
//! Matrix answers with a typed refusal instead of a degraded answer or a
//! generic connector error. The `class` is closed and is what the server
//! switches on; the `code` is open and is what an operator reads.
//!
//! One rule is enforced by construction rather than by review: a refusal's
//! message must not name a source the caller is not entitled to know exists.
//! [`Refusal::hidden`] is the constructor for that case and it takes no source.

use serde::{Deserialize, Serialize};
use std::fmt;

/// CLOSED. How the server must react. Adding a variant is a major contract bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalClass {
    /// This layer cannot answer this question at all. Not retryable.
    NotCovered,
    /// Transient: the source or a dependency is down. Retryable.
    Unavailable,
    /// Policy said no. Not retryable without a different principal.
    Denied,
    /// Something came back, but it cannot support a completeness or exactness
    /// claim. The server may still show it, labelled.
    Incomplete,
    /// The intent was malformed or contradicts the contract. A bug, not a state.
    Invalid,
    /// A budget, row cap, byte cap or deadline stopped it.
    Exhausted,
}

impl RefusalClass {
    pub fn as_str(self) -> &'static str {
        match self {
            RefusalClass::NotCovered => "not_covered",
            RefusalClass::Unavailable => "unavailable",
            RefusalClass::Denied => "denied",
            RefusalClass::Incomplete => "incomplete",
            RefusalClass::Invalid => "invalid",
            RefusalClass::Exhausted => "exhausted",
        }
    }

    /// Whether a caller should try again later without changing anything.
    pub fn retryable(self) -> bool {
        matches!(self, RefusalClass::Unavailable | RefusalClass::Exhausted)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refusal {
    pub class: RefusalClass,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl Refusal {
    pub fn new(class: RefusalClass, code: &str, message: impl Into<String>) -> Self {
        Self {
            class,
            code: code.to_string(),
            message: message.into(),
            source_id: None,
            retry_after_seconds: None,
            detail: None,
        }
    }

    /// Attach the source. Only legal when the caller already knows this source
    /// is in play — see [`Refusal::hidden`] for when it is not.
    pub fn with_source(mut self, source_id: impl Into<String>) -> Self {
        self.source_id = Some(source_id.into());
        self
    }

    pub fn with_retry_after(mut self, seconds: u64) -> Self {
        self.retry_after_seconds = Some(seconds);
        self
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }

    /// The refusal for a required layer the caller may not know about. Takes no
    /// source and no detail **by signature**, so the leak cannot be introduced
    /// by an edit that only touches a call site.
    pub fn hidden() -> Self {
        Self {
            class: RefusalClass::NotCovered,
            code: "required_evidence_not_permitted".into(),
            message: "A required layer of this research profile is not available to this session."
                .into(),
            source_id: None,
            retry_after_seconds: None,
            detail: None,
        }
    }

    // ---- the codes with fixed classes, so a class/code mismatch cannot be
    // introduced at a call site ------------------------------------------------

    pub fn not_covered(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::NotCovered, "not_covered", message)
    }
    pub fn contract_not_found(name: &str) -> Self {
        Self::new(
            RefusalClass::NotCovered,
            "contract_not_found",
            format!("no applied query contract named '{name}'"),
        )
    }
    /// This build carries no adapter for the kind the asset names.
    ///
    /// `not_covered` rather than `invalid`, and the distinction is the whole
    /// point: the asset is well formed and the grammar accepts it — every
    /// adapter kind stays in `AdapterKind` and in the validator whichever
    /// adapters a build links. What is missing is an implementation in *this*
    /// binary, which is a shape rather than a state, so no retry and no
    /// configuration change makes it succeed. The message names what would.
    ///
    /// Munarium Matrix core carries the adapters for operational databases and
    /// file sources; the analytics-platform adapters are Munarium Matrix
    /// Enterprise. A build may also register adapters of its own through
    /// `AdapterRegistry`, in which case this refusal means none matched.
    pub fn adapter_not_available(kind: &str) -> Self {
        Self::new(
            RefusalClass::NotCovered,
            "adapter_not_available",
            format!(
                "this build carries no '{kind}' adapter. The asset is valid; the \
                 implementation is not present. Adapters for analytics platforms \
                 (databricks, bigquery, snowflake, cube, dbt) are part of Munarium \
                 Matrix Enterprise"
            ),
        )
    }
    pub fn source_unavailable(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Unavailable, "source_unavailable", message)
    }
    pub fn source_stale(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Incomplete, "source_stale", message)
    }
    pub fn policy_denied(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Denied, "policy_denied", message)
    }
    pub fn policy_delegation_unavailable(message: impl Into<String>) -> Self {
        Self::new(
            RefusalClass::Denied,
            "policy_delegation_unavailable",
            message,
        )
    }
    pub fn budget_exceeded(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Exhausted, "budget_exceeded", message)
    }
    pub fn result_too_large(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Exhausted, "result_too_large", message)
    }
    pub fn result_truncated(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Incomplete, "result_truncated", message)
    }
    pub fn partial_result(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Incomplete, "partial_result", message)
    }
    pub fn deadline_exceeded(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Exhausted, "deadline_exceeded", message)
    }
    pub fn schema_drift(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Invalid, "schema_drift", message)
    }
    pub fn checkpoint_gap(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Incomplete, "checkpoint_gap", message)
    }
    pub fn snapshot_expired(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Unavailable, "snapshot_expired", message)
    }
    pub fn identity_ambiguous(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Incomplete, "identity_ambiguous", message)
    }
    pub fn result_not_identifiable(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Invalid, "result_not_identifiable", message)
    }
    pub fn too_many_classes(found: usize, max: usize) -> Self {
        Self::new(
            RefusalClass::Invalid,
            "too_many_classes",
            format!(
                "source resolves to {found} authorization equivalence classes, over the \
                 configured maximum of {max}; narrow the projection or raise maxAuthorizationClasses"
            ),
        )
    }
    pub fn seal_failed(message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Unavailable, "seal_failed", message)
    }
    pub fn invalid(code: &str, message: impl Into<String>) -> Self {
        Self::new(RefusalClass::Invalid, code, message)
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [{}]: {}",
            self.class.as_str(),
            self.code,
            self.message
        )
    }
}

impl std::error::Error for Refusal {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_layer_refusal_carries_no_source_and_no_detail() {
        let r = Refusal::hidden();
        assert!(r.source_id.is_none(), "would leak which source is required");
        assert!(r.detail.is_none());
        assert!(!r.message.to_lowercase().contains("collection"));
        assert_eq!(r.class, RefusalClass::NotCovered);
    }

    #[test]
    fn classes_say_honestly_whether_a_retry_can_help() {
        assert!(RefusalClass::Unavailable.retryable());
        assert!(RefusalClass::Exhausted.retryable());
        assert!(!RefusalClass::Denied.retryable());
        assert!(!RefusalClass::NotCovered.retryable());
        assert!(!RefusalClass::Invalid.retryable());
        // Incomplete is NOT retryable: the same query will truncate again.
        assert!(!RefusalClass::Incomplete.retryable());
    }

    #[test]
    fn constructors_pin_the_class_to_the_code() {
        assert_eq!(Refusal::policy_denied("x").class, RefusalClass::Denied);
        assert_eq!(
            Refusal::result_truncated("x").class,
            RefusalClass::Incomplete
        );
        assert_eq!(Refusal::budget_exceeded("x").class, RefusalClass::Exhausted);
        assert_eq!(Refusal::schema_drift("x").class, RefusalClass::Invalid);
    }

    #[test]
    fn refusals_round_trip_through_the_wire_form() {
        let r = Refusal::policy_denied("nope")
            .with_source("crm")
            .with_retry_after(30);
        let json = serde_json::to_string(&r).unwrap();
        let back: Refusal = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}

#[cfg(test)]
mod registry_doc {
    /// `docs/errors.md` lists every refusal code, and every code it lists
    /// exists.
    ///
    /// Both directions, because each failure is its own kind of lie: a code
    /// the service can emit and the registry omits leaves an operator reading
    /// a document that does not describe their incident, and a code the
    /// registry names and the service cannot emit is dead vocabulary
    /// presented as a capability.
    ///
    /// The scrape is deliberately crude — it reads the workspace's own
    /// sources — because a subtle check here would itself need checking.
    #[test]
    fn the_errors_registry_matches_the_vocabulary() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("matrix/ is two levels above this crate");
        let doc_path = root.join("docs/errors.md");
        let doc = std::fs::read_to_string(&doc_path)
            .unwrap_or_else(|e| panic!("{}: {e}", doc_path.display()));

        // Codes the workspace can actually emit, from the two shapes that
        // produce one: a constructor in this file, and an explicit
        // `Refusal::new(class, "code", ..)` / `Refusal::invalid("code", ..)`
        // anywhere in `src/`.
        let mut emitted: std::collections::BTreeSet<String> = Default::default();
        for f in walk(&root.join("src")) {
            let text = match std::fs::read_to_string(&f) {
                Ok(t) => t,
                Err(_) => continue,
            };
            for (i, _) in text.match_indices("Refusal::new(") {
                if let Some(c) = first_string_after(&text[i..], 200) {
                    emitted.insert(c);
                }
            }
            for (i, _) in text.match_indices("Refusal::invalid(") {
                if let Some(c) = first_string_after(&text[i..], 120) {
                    emitted.insert(c);
                }
            }
        }
        // The named constructors in this file are the rest of the vocabulary.
        let me = include_str!("refusal.rs");
        for line in me.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("pub fn ") {
                if let Some(name) = rest.split('(').next() {
                    // Builders and accessors are not codes.
                    const NOT_CODES: &[&str] = &[
                        "new",
                        "as_str",
                        "retryable",
                        "hidden",
                        "with_detail",
                        "with_retry_after",
                        "with_source",
                        "invalid",
                    ];
                    if !NOT_CODES.contains(&name)
                        && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    {
                        emitted.insert(name.to_string());
                    }
                }
            }
        }

        let missing: Vec<&String> = emitted
            .iter()
            .filter(|c| !doc.contains(&format!("`{c}`")))
            .collect();
        assert!(
            missing.is_empty(),
            "refusal codes the service can emit but docs/errors.md does not list: {missing:?}"
        );

        // A floor, so a scrape that found nothing cannot pass.
        assert!(
            emitted.len() > 20,
            "the code scrape found only {} codes — it has drifted",
            emitted.len()
        );
    }

    fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
        out
    }

    /// The first `"..."` literal within `window` bytes of the start of `s`.
    fn first_string_after(s: &str, window: usize) -> Option<String> {
        let hay = &s[..s.len().min(window)];
        let start = hay.find('"')? + 1;
        let end = hay[start..].find('"')? + start;
        let code = &hay[start..end];
        (!code.is_empty()
            && code
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit()))
        .then(|| code.to_string())
    }
}
