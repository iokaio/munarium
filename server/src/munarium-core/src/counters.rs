// SPDX-License-Identifier: Apache-2.0
//! Counters: absolute whole-document frequency budgets. Pure logic — the
//! per-scope upserts live behind `StorageBackend`.

use crate::types::{CounterTotal, GateFinding, Severity};
use serde_json::json;

pub const RULE_COUNTER: &str = "gate.counter-budget";

/// Case-insensitive non-overlapping occurrence count.
pub fn count_pattern(text: &str, pattern: &str) -> usize {
    if pattern.is_empty() {
        return 0;
    }
    text.to_lowercase().matches(&pattern.to_lowercase()).count()
}

/// Warn when a total exceeds its budget.
pub fn check_budgets(totals: &[CounterTotal], scope_path: Option<&str>) -> Vec<GateFinding> {
    totals
        .iter()
        .filter(|t| t.budget.map(|b| t.total > b).unwrap_or(false))
        .map(|t| GateFinding {
            rule_id: RULE_COUNTER.into(),
            severity: Severity::Warn,
            message: format!(
                "counter '{}' at {} exceeds whole-document budget {}",
                t.key,
                t.total,
                t.budget.unwrap_or(0)
            ),
            scope_path: scope_path.map(String::from),
            detail: Some(json!({ "counter_key": t.key, "total": t.total, "budget": t.budget })),
        })
        .collect()
}

/// Composer directives: used/budget lines plus avoid-instructions when a
/// budget is exhausted or within one use of it.
pub fn directives(totals: &[CounterTotal]) -> String {
    let mut lines = Vec::new();
    for t in totals {
        let Some(budget) = t.budget else { continue };
        lines.push(format!("{}: used {}/{}", t.key, t.total, budget));
        if t.total >= budget {
            lines.push(format!("AVOID: '{}' — budget exhausted", t.key));
        } else if t.total + 1 == budget {
            lines.push(format!("CAUTION: '{}' — one use remaining", t.key));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_case_insensitively() {
        assert_eq!(
            count_pattern("The Bell rang. the bell again.", "the bell"),
            2
        );
    }

    #[test]
    fn budget_check_and_directives() {
        let totals = vec![
            CounterTotal {
                key: "flashback".into(),
                total: 3,
                budget: Some(2),
            },
            CounterTotal {
                key: "storm".into(),
                total: 1,
                budget: Some(2),
            },
            CounterTotal {
                key: "unbudgeted".into(),
                total: 9,
                budget: None,
            },
        ];
        let findings = check_budgets(&totals, Some("ch4"));
        assert_eq!(findings.len(), 1);
        let d = directives(&totals);
        assert!(d.contains("AVOID: 'flashback'"));
        assert!(d.contains("CAUTION: 'storm'"));
        assert!(!d.contains("unbudgeted"));
    }
}
