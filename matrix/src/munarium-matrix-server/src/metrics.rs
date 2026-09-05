// SPDX-License-Identifier: Apache-2.0
//! Prometheus text exposition, hand-rolled — no new crate, matching the
//! server's posture.
//!
//! **Cardinality rules, which are not negotiable:** no tenant label, no uid
//! label, no source-instance label, no parameter values. A metric with a
//! tenant label turns a monitoring system into an unaudited copy of who is
//! asking what. Per-tenant analytics live in the journal, behind the mgmt role.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct Metrics {
    counters: Mutex<BTreeMap<String, u64>>,
    /// Histogram buckets in milliseconds, per metric+labels.
    histograms: Mutex<BTreeMap<String, Vec<u64>>>,
    pub inflight: AtomicU64,
}

/// The bucket boundaries. Chosen around what actually matters here: a source
/// statement in the low hundreds of ms, a warehouse cold start in the tens of
/// seconds.
const BUCKETS_MS: &[u64] = &[
    5, 10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 30_000,
];

impl Metrics {
    pub fn inc(&self, name: &str, labels: &[(&str, &str)]) {
        self.add(name, labels, 1);
    }

    pub fn add(&self, name: &str, labels: &[(&str, &str)], n: u64) {
        let key = key_of(name, labels);
        *self
            .counters
            .lock()
            .expect("metrics")
            .entry(key)
            .or_insert(0) += n;
    }

    /// Record a duration. Consumed by the execute and sync paths as each role
    /// is wired into the binary; the exposition format it produces is tested
    /// here so a histogram cannot land malformed.
    #[allow(dead_code)]
    pub fn observe_ms(&self, name: &str, labels: &[(&str, &str)], ms: u64) {
        let key = key_of(name, labels);
        self.histograms
            .lock()
            .expect("metrics")
            .entry(key)
            .or_default()
            .push(ms);
    }

    /// Render the exposition. Kept deterministic (BTreeMap) so a diff of two
    /// scrapes is readable.
    pub fn render(&self, role: &str, version: &str) -> String {
        let mut out = String::new();
        out.push_str("# HELP munarium_matrix_build_info Build metadata; value is always 1.\n");
        out.push_str("# TYPE munarium_matrix_build_info gauge\n");
        out.push_str(&format!(
            "munarium_matrix_build_info{{version=\"{version}\",role=\"{role}\"}} 1\n"
        ));

        out.push_str("# HELP munarium_matrix_inflight_requests Requests currently being served.\n");
        out.push_str("# TYPE munarium_matrix_inflight_requests gauge\n");
        out.push_str(&format!(
            "munarium_matrix_inflight_requests {}\n",
            self.inflight.load(Ordering::Relaxed)
        ));

        let counters = self.counters.lock().expect("metrics");
        let mut current = "";
        for (key, value) in counters.iter() {
            let name = key.split('{').next().unwrap_or(key);
            if name != current {
                out.push_str(&format!("# TYPE {name} counter\n"));
                current = name;
            }
            out.push_str(&format!("{key} {value}\n"));
        }
        drop(counters);

        let hist = self.histograms.lock().expect("metrics");
        for (key, samples) in hist.iter() {
            let name = key.split('{').next().unwrap_or(key);
            let labels = key
                .strip_prefix(name)
                .unwrap_or("")
                .trim_matches(['{', '}'].as_ref());
            out.push_str(&format!("# TYPE {name} histogram\n"));
            for b in BUCKETS_MS {
                // Prometheus buckets are CUMULATIVE: `le="100"` counts every
                // sample at or under 100, not the ones between 50 and 100.
                let count = samples.iter().filter(|s| *s <= b).count();
                out.push_str(&format!(
                    "{name}_bucket{{{}le=\"{b}\"}} {count}\n",
                    if labels.is_empty() {
                        String::new()
                    } else {
                        format!("{labels},")
                    }
                ));
            }
            out.push_str(&format!(
                "{name}_bucket{{{}le=\"+Inf\"}} {}\n",
                if labels.is_empty() {
                    String::new()
                } else {
                    format!("{labels},")
                },
                samples.len()
            ));
            out.push_str(&format!(
                "{name}_sum{{{}}} {}\n",
                labels,
                samples.iter().sum::<u64>()
            ));
            out.push_str(&format!("{name}_count{{{}}} {}\n", labels, samples.len()));
        }
        out
    }
}

fn key_of(name: &str, labels: &[(&str, &str)]) -> String {
    if labels.is_empty() {
        return name.to_string();
    }
    let rendered: Vec<String> = labels
        .iter()
        .map(|(k, v)| format!("{k}=\"{}\"", v.replace('"', "")))
        .collect();
    format!("{name}{{{}}}", rendered.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_render_with_their_labels() {
        let m = Metrics::default();
        m.inc("munarium_matrix_executions_total", &[("outcome", "ok")]);
        m.inc("munarium_matrix_executions_total", &[("outcome", "ok")]);
        m.inc(
            "munarium_matrix_executions_total",
            &[("outcome", "refused")],
        );
        let text = m.render("all", "0.1.0");
        assert!(
            text.contains("munarium_matrix_executions_total{outcome=\"ok\"} 2"),
            "{text}"
        );
        assert!(text.contains("munarium_matrix_executions_total{outcome=\"refused\"} 1"));
    }

    #[test]
    fn the_exposition_carries_build_info_and_inflight() {
        let m = Metrics::default();
        m.inflight.store(3, Ordering::Relaxed);
        let text = m.render("query", "0.1.0");
        assert!(text.contains("munarium_matrix_build_info{version=\"0.1.0\",role=\"query\"} 1"));
        assert!(text.contains("munarium_matrix_inflight_requests 3"));
    }

    #[test]
    fn histograms_are_cumulative_and_end_at_inf() {
        let m = Metrics::default();
        for ms in [3, 30, 300] {
            m.observe_ms("munarium_matrix_execute_duration_ms", &[], ms);
        }
        let text = m.render("all", "0.1.0");
        assert!(text.contains("_bucket{le=\"5\"} 1"), "{text}");
        assert!(text.contains("_bucket{le=\"50\"} 2"), "{text}");
        assert!(text.contains("_bucket{le=\"+Inf\"} 3"), "{text}");
        assert!(text.contains("_count{} 3"));
    }
}
