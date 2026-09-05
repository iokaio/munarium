// SPDX-License-Identifier: Apache-2.0
//! Hand-rolled Prometheus text-format metrics (exposition format v0.0.4),
//! served by the ops plane at GET /metrics. Zero new crates by design: the
//! metric set is small and fixed, and the exporter crates pull a dependency
//! graph (metrics-util, quanta, sketches) that buys nothing at this scale —
//! the same trade §9 makes everywhere else. OTel export remains a documented
//! non-goal until a real backend demands it (architecture.md §12).
//!
//! Cardinality rules (load-bearing — a scrape target must stay bounded):
//! - `route` is the axum MatchedPath TEMPLATE (`/v1/versions/{version_id}/facts`),
//!   never the raw path; for gRPC it is the full method path (a bounded set).
//! - NO tenant/uid labels. Per-tenant and per-user analytics belong to the
//!   interactions table and the reports API, which aggregate across replicas
//!   naturally. Process metrics stay per-process and label-bounded.
//! - NO instance label: the scraper assigns `instance` per target, which is
//!   the cluster-correct posture (each replica is its own scrape target).
//!
//! Counters and histograms are recorded through `Metrics` (held in AppState);
//! gauges are polled at render time from live state so there is no sampler
//! task. `render(state)` assembles the whole exposition.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::RwLock;

/// Histogram buckets in seconds. Fixed for every duration metric: request
/// latencies and provider calls share a range from sub-10ms to the 30s
/// provider timeout.
pub const BUCKETS: [f64; 12] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// (name, type, help) for every metric this process may emit. Render emits
/// HELP/TYPE only for metrics with at least one series (plus build_info and
/// the polled gauges, which always exist).
const METRICS_META: &[(&str, &str, &str)] = &[
    (
        "munarium_build_info",
        "gauge",
        "Build metadata; value is always 1.",
    ),
    (
        "munarium_http_requests_total",
        "counter",
        "Requests served, by plane, route template, method, and status class.",
    ),
    (
        "munarium_http_request_duration_seconds",
        "histogram",
        "Request latency by plane and route template.",
    ),
    (
        "munarium_db_pool_connections",
        "gauge",
        "Open connections in the sqlx pool (postgres store only).",
    ),
    (
        "munarium_db_pool_idle",
        "gauge",
        "Idle connections in the sqlx pool (postgres store only).",
    ),
    (
        "munarium_interactions_queue_depth",
        "gauge",
        "Interaction records waiting in the bounded writer channel.",
    ),
    (
        "munarium_interactions_dropped_total",
        "counter",
        "Interaction records dropped because the writer channel was saturated.",
    ),
    (
        "munarium_interactions_insert_failures_total",
        "counter",
        "Interaction rows that failed to insert (write error, not saturation).",
    ),
    (
        "munarium_provider_calls_total",
        "counter",
        "Provider invocations by provider, model, kind (complete|embed), and outcome.",
    ),
    (
        "munarium_provider_call_duration_seconds",
        "histogram",
        "Provider invocation latency by provider and kind.",
    ),
    (
        "munarium_provider_tokens_total",
        "counter",
        "Completion tokens by provider, model, and direction (input|output).",
    ),
    (
        "munarium_runbook_step_transitions_total",
        "counter",
        "Runbook step state transitions, by resulting state.",
    ),
    (
        "munarium_load_shed_total",
        "counter",
        "Requests refused 503 overloaded by the concurrency-limit load shed.",
    ),
    // Derived-index builds. Labelled by mode and outcome
    // only: a tenant, collection, version or artifact label here would
    // multiply cardinality by the corpus count and publish the tenant list to
    // anyone who can scrape.
    (
        "munarium_index_build_total",
        "counter",
        "Derived-index builds, by mode (mirror|direct) and outcome          (published|converged|already_built|deferred|failed).",
    ),
    (
        "munarium_index_build_chunks_total",
        "counter",
        "Chunks sealed into derived-index artifacts, by mode and outcome.",
    ),
    (
        "munarium_index_build_bytes_total",
        "counter",
        "Component bytes sealed into derived-index artifacts, by mode and outcome.",
    ),
    (
        "munarium_index_build_duration_seconds",
        "histogram",
        "Derived-index build time by mode and phase (export|seal|publish|total).",
    ),
];

struct Histogram {
    buckets: [AtomicU64; BUCKETS.len()],
    sum_micros: AtomicU64,
    count: AtomicU64,
}

impl Histogram {
    fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            sum_micros: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    fn observe(&self, seconds: f64) {
        for (i, upper) in BUCKETS.iter().enumerate() {
            if seconds <= *upper {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.sum_micros
            .fetch_add((seconds * 1e6).max(0.0) as u64, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }
}

/// Series key: metric name + rendered label pairs (sorted at call sites by
/// construction — every call site passes labels in one fixed order).
type Key = (&'static str, String);

#[derive(Default)]
pub struct Metrics {
    counters: RwLock<HashMap<Key, Arc<AtomicU64>>>,
    histograms: RwLock<HashMap<Key, Arc<Histogram>>>,
}

/// Escape a label value per the exposition format: backslash, double quote,
/// and newline.
pub fn escape_label(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Render one label set from (name, value) pairs — values get escaped.
pub fn labels(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(k);
        out.push_str("=\"");
        out.push_str(&escape_label(v));
        out.push('"');
    }
    out
}

impl Metrics {
    pub fn inc(&self, metric: &'static str, labels: String) {
        self.inc_by(metric, labels, 1);
    }

    pub fn inc_by(&self, metric: &'static str, labels: String, n: u64) {
        // Read-lock fast path; write lock only on first sight of a series.
        {
            let map = self.counters.read().expect("metrics lock");
            if let Some(c) = map.get(&(metric, labels.clone())) {
                c.fetch_add(n, Ordering::Relaxed);
                return;
            }
        }
        let mut map = self.counters.write().expect("metrics lock");
        map.entry((metric, labels))
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .fetch_add(n, Ordering::Relaxed);
    }

    pub fn observe(&self, metric: &'static str, labels: String, seconds: f64) {
        {
            let map = self.histograms.read().expect("metrics lock");
            if let Some(h) = map.get(&(metric, labels.clone())) {
                h.observe(seconds);
                return;
            }
        }
        let mut map = self.histograms.write().expect("metrics lock");
        map.entry((metric, labels))
            .or_insert_with(|| Arc::new(Histogram::new()))
            .observe(seconds);
    }

    /// Deterministic render of the recorded counters and histograms: series
    /// sorted by (metric, labels) so scrapes and tests see a stable order.
    fn render_recorded(&self, out: &mut String, emitted_help: &mut Vec<&'static str>) {
        let counters: Vec<(Key, u64)> = {
            let map = self.counters.read().expect("metrics lock");
            let mut v: Vec<_> = map
                .iter()
                .map(|(k, c)| (k.clone(), c.load(Ordering::Relaxed)))
                .collect();
            v.sort();
            v
        };
        for ((metric, labels), value) in counters {
            help_type_once(out, metric, emitted_help);
            if labels.is_empty() {
                out.push_str(&format!("{metric} {value}\n"));
            } else {
                out.push_str(&format!("{metric}{{{labels}}} {value}\n"));
            }
        }

        let histos: Vec<(Key, Arc<Histogram>)> = {
            let map = self.histograms.read().expect("metrics lock");
            let mut v: Vec<_> = map.iter().map(|(k, h)| (k.clone(), h.clone())).collect();
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v
        };
        for ((metric, labels), h) in histos {
            help_type_once(out, metric, emitted_help);
            let sep = if labels.is_empty() { "" } else { "," };
            for (i, upper) in BUCKETS.iter().enumerate() {
                let n = h.buckets[i].load(Ordering::Relaxed);
                out.push_str(&format!(
                    "{metric}_bucket{{{labels}{sep}le=\"{upper}\"}} {n}\n"
                ));
            }
            let count = h.count.load(Ordering::Relaxed);
            out.push_str(&format!(
                "{metric}_bucket{{{labels}{sep}le=\"+Inf\"}} {count}\n"
            ));
            let sum = h.sum_micros.load(Ordering::Relaxed) as f64 / 1e6;
            if labels.is_empty() {
                out.push_str(&format!("{metric}_sum {sum}\n"));
                out.push_str(&format!("{metric}_count {count}\n"));
            } else {
                out.push_str(&format!("{metric}_sum{{{labels}}} {sum}\n"));
                out.push_str(&format!("{metric}_count{{{labels}}} {count}\n"));
            }
        }
    }
}

fn help_type_once(out: &mut String, metric: &'static str, emitted: &mut Vec<&'static str>) {
    if emitted.contains(&metric) {
        return;
    }
    emitted.push(metric);
    if let Some((name, kind, help)) = METRICS_META.iter().find(|(n, _, _)| *n == metric) {
        out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
    }
}

/// Map an HTTP status code to its class label.
pub fn status_class(status: u16) -> &'static str {
    match status / 100 {
        1 => "1xx",
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        5 => "5xx",
        _ => "other",
    }
}

/// The full exposition: build info + polled gauges + recorded series.
pub fn render(state: &crate::state::AppState) -> String {
    let mut out = String::with_capacity(4096);
    let mut emitted: Vec<&'static str> = Vec::new();

    help_type_once(&mut out, "munarium_build_info", &mut emitted);
    out.push_str(&format!(
        "munarium_build_info{{version=\"{}\"}} 1\n",
        env!("CARGO_PKG_VERSION")
    ));

    if let Some(pool) = state.pg_pool() {
        help_type_once(&mut out, "munarium_db_pool_connections", &mut emitted);
        out.push_str(&format!("munarium_db_pool_connections {}\n", pool.size()));
        help_type_once(&mut out, "munarium_db_pool_idle", &mut emitted);
        out.push_str(&format!("munarium_db_pool_idle {}\n", pool.num_idle()));
    }

    let tx = &state.interactions_tx;
    let depth = tx.max_capacity().saturating_sub(tx.capacity());
    help_type_once(&mut out, "munarium_interactions_queue_depth", &mut emitted);
    out.push_str(&format!("munarium_interactions_queue_depth {depth}\n"));

    state.metrics.render_recorded(&mut out, &mut emitted);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_escaping_covers_the_three_specials() {
        assert_eq!(escape_label(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(escape_label("line\nbreak"), "line\\nbreak");
        assert_eq!(
            labels(&[("route", "/v1/x"), ("q", "say \"hi\"")]),
            r#"route="/v1/x",q="say \"hi\"""#
        );
    }

    #[test]
    fn histogram_buckets_are_cumulative_and_inf_equals_count() {
        let h = Histogram::new();
        h.observe(0.003); // below every bound
        h.observe(0.3); // lands at 0.5 and above
        h.observe(99.0); // above every bound — only +Inf sees it
        let le_005 = h.buckets[0].load(Ordering::Relaxed);
        let le_05 = h.buckets[6].load(Ordering::Relaxed);
        let le_30 = h.buckets[BUCKETS.len() - 1].load(Ordering::Relaxed);
        assert_eq!(le_005, 1);
        assert_eq!(le_05, 2);
        assert_eq!(le_30, 2, "99s falls outside every finite bucket");
        assert_eq!(h.count.load(Ordering::Relaxed), 3, "+Inf count sees all");
    }

    #[test]
    fn render_order_is_deterministic_and_counters_accumulate() {
        let m = Metrics::default();
        m.inc(
            "munarium_http_requests_total",
            labels(&[("plane", "rest"), ("route", "/b")]),
        );
        m.inc(
            "munarium_http_requests_total",
            labels(&[("plane", "rest"), ("route", "/a")]),
        );
        m.inc(
            "munarium_http_requests_total",
            labels(&[("plane", "rest"), ("route", "/a")]),
        );
        let mut out = String::new();
        let mut emitted = Vec::new();
        m.render_recorded(&mut out, &mut emitted);
        let a = out
            .find(r#"route="/a"} 2"#)
            .expect("series /a with count 2");
        let b = out
            .find(r#"route="/b"} 1"#)
            .expect("series /b with count 1");
        assert!(a < b, "series must render label-sorted:\n{out}");
        assert!(out.starts_with("# HELP munarium_http_requests_total"));
    }

    #[test]
    fn every_recorded_metric_name_has_meta() {
        // The registry is the HELP/TYPE source; a name without meta renders
        // bare, which some scrapers reject. Keep the two lists in sync.
        for name in [
            "munarium_http_requests_total",
            "munarium_http_request_duration_seconds",
            "munarium_interactions_dropped_total",
            "munarium_interactions_insert_failures_total",
            "munarium_provider_calls_total",
            "munarium_provider_call_duration_seconds",
            "munarium_provider_tokens_total",
            "munarium_runbook_step_transitions_total",
            "munarium_load_shed_total",
        ] {
            assert!(
                METRICS_META.iter().any(|(n, _, _)| *n == name),
                "metric '{name}' missing from METRICS_META"
            );
        }
    }

    #[test]
    fn status_classes() {
        assert_eq!(status_class(200), "2xx");
        assert_eq!(status_class(404), "4xx");
        assert_eq!(status_class(503), "5xx");
        assert_eq!(status_class(101), "1xx");
    }
}
