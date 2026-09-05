// SPDX-License-Identifier: Apache-2.0
//! Promise registry logic (pure parts). Registration/fulfillment I/O lives
//! behind `StorageBackend`; the overdue check is deterministic and lives here.

use crate::types::{GateFinding, Promise, PromiseStatus, Severity};
use serde_json::json;

pub const RULE_PROMISE: &str = "gate.promise-unfulfilled";

/// Warn findings for open promises that are overdue: due at the current scope,
/// or ANY open promise when this is the final unit. Mirrors
/// `mesh/promises.find_overdue`.
pub fn find_overdue(
    promises: &[Promise],
    current_scope: Option<&str>,
    is_final_unit: bool,
) -> Vec<GateFinding> {
    promises
        .iter()
        .filter(|p| p.status == PromiseStatus::Open)
        .filter(|p| {
            is_final_unit || (p.due_scope.is_some() && p.due_scope.as_deref() == current_scope)
        })
        .map(|p| GateFinding {
            rule_id: RULE_PROMISE.into(),
            severity: Severity::Warn,
            message: if is_final_unit {
                format!(
                    "promise '{}' is still open at the final unit: {}",
                    p.key, p.description
                )
            } else {
                format!(
                    "promise '{}' was due at scope '{}' and is still open",
                    p.key,
                    p.due_scope.as_deref().unwrap_or("")
                )
            },
            scope_path: current_scope.map(String::from),
            detail: Some(json!({ "promise_key": p.key, "promise_id": p.id })),
        })
        .collect()
}

/// Pin-aware status view: a promise fulfilled AFTER the pin reads back OPEN.
pub fn status_as_of(p: &Promise, as_of_seq: Option<u64>) -> PromiseStatus {
    match (as_of_seq, p.fulfilled_seq, p.status) {
        (Some(pin), Some(fulfilled), PromiseStatus::Fulfilled) if fulfilled > pin => {
            PromiseStatus::Open
        }
        _ => p.status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn promise(
        key: &str,
        due: Option<&str>,
        status: PromiseStatus,
        fulfilled_seq: Option<u64>,
    ) -> Promise {
        Promise {
            id: format!("p-{key}"),
            version_id: "v1".into(),
            key: key.into(),
            kind: "setup".into(),
            description: format!("payoff for {key}"),
            origin_scope: Some("ch1".into()),
            due_scope: due.map(Into::into),
            status,
            seq: 1,
            fulfilled_seq,
        }
    }

    #[test]
    fn due_scope_triggers_overdue() {
        let ps = vec![promise("a", Some("ch3"), PromiseStatus::Open, None)];
        assert_eq!(find_overdue(&ps, Some("ch3"), false).len(), 1);
        assert!(find_overdue(&ps, Some("ch2"), false).is_empty());
    }

    #[test]
    fn final_unit_flags_every_open_promise() {
        let ps = vec![
            promise("a", None, PromiseStatus::Open, None),
            promise("b", Some("ch9"), PromiseStatus::Open, None),
            promise("c", None, PromiseStatus::Fulfilled, Some(4)),
        ];
        assert_eq!(find_overdue(&ps, Some("ch5"), true).len(), 2);
    }

    /// Post-pin fulfillment reads back open — the pin semantic.
    #[test]
    fn pin_reopens_later_fulfillment() {
        let p = promise("a", None, PromiseStatus::Fulfilled, Some(8));
        assert_eq!(status_as_of(&p, Some(5)), PromiseStatus::Open);
        assert_eq!(status_as_of(&p, Some(9)), PromiseStatus::Fulfilled);
        assert_eq!(status_as_of(&p, None), PromiseStatus::Fulfilled);
    }
}
