// SPDX-License-Identifier: Apache-2.0
//! The `TARGET_SERVER_VERSION` lockstep check.
//!
//! Matrix seals evidence into munarium-server and reads its ledger, so the two
//! move together. This module decides what a version difference MEANS:
//!
//! - **Major mismatch → refuse.** The wire changed. Running anyway would
//!   produce failures far from the cause, at the worst possible moment.
//! - **Minor mismatch → warn and continue, but say WHICH WAY.** Minor is
//!   additive by the contract's own compatibility rule, and additive is
//!   **asymmetric**: a newer server is harmless to an older Matrix (the new
//!   fields are simply unused), while an older server may be missing an
//!   endpoint this build calls. Reporting both as one benign `minor_drift`
//!   hid a real case — 2026-08-28, Matrix at 0.5.0 against the only published
//!   server image, 0.3.0, which has no `/v1/evidence` at all. The verdict read
//!   "minor drift, additive, fine"; every seal would have 404'd at runtime.
//!   The two directions are now distinguishable.
//! - **Server unreachable → warn and continue.** Refusing to start because a
//!   peer is down would turn a transient outage into an outage that needs a
//!   human to end. The startup check is a diagnostic, not a dependency.
//!
//! The check runs once at startup and its result is reported on `/version`, so
//! an operator can see the answer without reading logs.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compatibility {
    /// Same major and minor.
    Exact,
    /// Same major, server minor is NEWER than this build targets. Additive by
    /// the compatibility rule, so the extra surface is simply unused: the
    /// ordinary rolling-deploy state.
    MinorDrift,
    /// Same major, server minor is OLDER than this build targets. NOT
    /// symmetric with the above: this build may call routes the server does
    /// not have, and the failure would land at the first seal rather than at
    /// startup. Non-fatal — the startup check is a diagnostic, not a
    /// dependency, and the runtime already fails closed with a typed refusal —
    /// but it must never read as benign.
    MinorBehind,
    /// Different major. Refuse.
    MajorMismatch,
    /// Could not ask. Not a verdict about compatibility.
    Unknown,
}

impl Compatibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Compatibility::Exact => "exact",
            Compatibility::MinorDrift => "minor_drift",
            Compatibility::MinorBehind => "minor_behind",
            Compatibility::MajorMismatch => "major_mismatch",
            Compatibility::Unknown => "unknown",
        }
    }

    /// Whether this verdict should stop the process starting.
    pub fn is_fatal(self) -> bool {
        matches!(self, Compatibility::MajorMismatch)
    }
}

impl fmt::Display for Compatibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn parts(v: &str) -> Option<(u32, u32)> {
    let v = v.trim().trim_start_matches('v');
    let mut it = v.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next().unwrap_or("0").parse().unwrap_or(0);
    Some((major, minor))
}

/// Compare the version this build targets with what the server reports.
pub fn compare(target: &str, actual: Option<&str>) -> Compatibility {
    let Some(actual) = actual else {
        return Compatibility::Unknown;
    };
    match (parts(target), parts(actual)) {
        (Some((tmaj, tmin)), Some((amaj, amin))) => {
            if tmaj != amaj {
                Compatibility::MajorMismatch
            } else if amin > tmin {
                // Server ahead: the additive surface is unused. Benign.
                Compatibility::MinorDrift
            } else if amin < tmin {
                // Server behind: this build may need routes it does not have.
                Compatibility::MinorBehind
            } else {
                Compatibility::Exact
            }
        }
        // An unparseable version on either side is not a compatibility verdict.
        _ => Compatibility::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_match_is_exact() {
        assert_eq!(compare("0.3.0", Some("0.3.0")), Compatibility::Exact);
        // The patch component is not part of the wire contract.
        assert_eq!(compare("0.3.0", Some("0.3.7")), Compatibility::Exact);
    }

    #[test]
    fn a_newer_server_warns_but_does_not_stop_a_rolling_deploy() {
        let c = compare("0.3.0", Some("0.4.0"));
        assert_eq!(c, Compatibility::MinorDrift);
        assert!(
            !c.is_fatal(),
            "minor is additive; a rolling deploy must survive it"
        );
    }

    #[test]
    fn an_older_server_is_reported_differently_from_a_newer_one() {
        // The case that hid: Matrix at 0.5.0 against the only published image,
        // 0.3.0 — which has no /v1/evidence at all. Both directions used to
        // report `minor_drift`, so the verdict read "additive, fine" while
        // every seal would 404 at runtime. Additive is ASYMMETRIC.
        let behind = compare("0.5.0", Some("0.3.0"));
        assert_eq!(behind, Compatibility::MinorBehind);
        assert_eq!(behind.as_str(), "minor_behind");
        assert_ne!(
            behind,
            compare("0.3.0", Some("0.5.0")),
            "a server that is BEHIND must not report the same verdict as one AHEAD"
        );
        // Still a diagnostic, not a dependency: the runtime fails closed with a
        // typed refusal, and refusing to boot would turn a deploy-order mistake
        // into an outage needing a human.
        assert!(!behind.is_fatal());
    }

    #[test]
    fn a_major_difference_is_fatal() {
        let c = compare("0.3.0", Some("1.0.0"));
        assert_eq!(c, Compatibility::MajorMismatch);
        assert!(c.is_fatal());
    }

    #[test]
    fn an_unreachable_server_is_unknown_not_a_mismatch() {
        let c = compare("0.3.0", None);
        assert_eq!(c, Compatibility::Unknown);
        assert!(
            !c.is_fatal(),
            "a peer being down must not turn into an outage that needs a human"
        );
    }

    #[test]
    fn an_unparseable_version_is_unknown_rather_than_a_guess() {
        assert_eq!(compare("0.3.0", Some("nightly")), Compatibility::Unknown);
        assert_eq!(compare("main", Some("0.3.0")), Compatibility::Unknown);
    }

    #[test]
    fn a_leading_v_is_tolerated() {
        assert_eq!(compare("v0.3.0", Some("0.3.0")), Compatibility::Exact);
    }
}
