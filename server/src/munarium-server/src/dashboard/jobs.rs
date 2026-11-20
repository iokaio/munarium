// SPDX-License-Identifier: Apache-2.0
//! The build-job pages (ClearView U12): the tenant's queued index build jobs
//! and, for one job, its state, the progress its current claim recorded, its
//! recorded transitions and its per-version outcomes.
//!
//! Read-only, and every number comes from the same `BuildJobs` reads the
//! `/v1/index-build-jobs` routes serve (R6 and R9); no new API. The rules
//! these pages keep:
//!
//! - a read that fails is shown as unavailable with its reason, never as
//!   zero or an empty list (U5);
//! - each fact is its own row, and every count says what it counts (U6);
//! - an unknown total is "total unknown", never 0 (R9);
//! - pruned event history is announced, never silently absent (B2);
//! - per-version history of a job only a v1.4 builder claimed is
//!   "unavailable", so an empty list there is never read as "nothing" (R6);
//! - the complete job and its API request sit one disclosure away, with the
//!   `mmctl` command where one exists (U7).
//!
//! While a job is pending or running its page reloads itself with a
//! `<meta http-equiv="refresh">` and stops once the job ends (U12).

use super::{
    admin_auth, complete_result, error_panel, html_table, kv, link, path_segment, pill, render,
    render_refreshing, shell_word, Tone,
};
use crate::charts::esc;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use munarium_core::KernelError;
use munarium_store_pg::jobs::{
    BuildJob, BuildJobs, HistoryStatus, JobEvent, JobId, JobProgress, JobVersionHistory,
};
use std::sync::Arc;

const ACTIVE: &str = "build jobs";
const LIST: &str = "/admin/build-jobs";
/// The list's bound: the same newest-50 window as `GET /v1/index-build-jobs`.
const LIST_LIMIT: i64 = 50;
/// Events read per page, and at most this many pages per render.
const EVENT_PAGE: i64 = 200;
const EVENT_PAGES: usize = 5;
/// Per-version outcomes read per render.
const VERSION_PAGE: i64 = 200;
/// How often a pending or running job's page reloads.
const REFRESH_SECS: u32 = 5;

fn job_path(job_id: &str) -> String {
    format!("{LIST}/{}", path_segment(job_id))
}

fn api_path(job_id: &str, read: &str) -> String {
    format!("GET /v1/index-build-jobs/{}{read}", path_segment(job_id))
}

fn jobs_of(state: &AppState) -> Result<BuildJobs, KernelError> {
    state
        .pg_pool()
        .map(|p| BuildJobs::new(p.clone()))
        .ok_or_else(|| KernelError::InvalidInput("build jobs require the postgres store".into()))
}

fn muted(text: &str) -> String {
    format!(r#"<span class="muted">{}</span>"#, esc(text))
}

fn code(text: &str) -> String {
    format!("<code>{}</code>", esc(text))
}

fn opt_code(v: &Option<String>) -> String {
    v.as_deref().map(code).unwrap_or_else(|| "—".into())
}

/// A section's unavailable state (U5): the read failed, so nothing below it
/// is known. Never rendered as zero or as an empty list.
fn unavailable(what: &str, reason: &str) -> String {
    format!(
        r#"<div class="notice">{} {} could not be read: {}</div>"#,
        pill("unavailable", Tone::Warn),
        esc(what),
        esc(reason)
    )
}

/// The API request (and the absence of a CLI command) for one read.
fn api_line(api: &str) -> String {
    format!(
        r#"<p class="muted">API: <code class="cmd">{}</code> · no mmctl command; use the API request</p>"#,
        esc(api)
    )
}

fn state_pill(state: &str) -> String {
    let tone = match state {
        "succeeded" => Tone::Good,
        "failed" => Tone::Bad,
        "cancelled" => Tone::Warn,
        _ => Tone::Neutral,
    };
    pill(state, tone)
}

fn is_open(state: &str) -> bool {
    matches!(state, "pending" | "running")
}

/// What a job builds: a collection (linked) or one index version.
fn target(job: &BuildJob) -> String {
    match (&job.scope_kind, &job.scope_id, &job.index_version_id) {
        (Some(kind), Some(id), _) if kind == "collection" => format!(
            "collection {}",
            link(&format!("/admin/collections/{}", path_segment(id)), id)
        ),
        (Some(kind), Some(id), _) => format!("{} {}", esc(kind), code(id)),
        (_, _, Some(v)) => format!("index version {}", code(v)),
        _ => "—".into(),
    }
}

/// The claim generation: how many times the job has been claimed.
fn generation(attempts: i32) -> String {
    if attempts <= 0 {
        muted("never claimed")
    } else {
        format!("generation {attempts}")
    }
}

/// What a job's progress counts: sources for a direct build, versions for a
/// backfill. A rebuild records no counts.
fn unit_noun(kind: &str) -> Option<&'static str> {
    match kind {
        "direct" => Some("sources"),
        "backfill" => Some("versions"),
        _ => None,
    }
}

/// The labelled count (U6, R9): "3 of 10 sources", "3 sources done, total
/// unknown", or a statement that nothing was recorded. Never a bare 0.
pub(super) fn units_label(kind: &str, done: Option<i64>, total: Option<i64>) -> String {
    let noun = unit_noun(kind).unwrap_or("units");
    match (done, total) {
        (Some(d), Some(t)) => esc(&format!("{d} of {t} {noun}")),
        (Some(d), None) => format!(
            "{} {}",
            esc(&format!("{d} {noun} done")),
            muted("total unknown")
        ),
        (None, Some(t)) => format!(
            "{} {}",
            muted("done not recorded"),
            esc(&format!("of {t} {noun}"))
        ),
        (None, None) if unit_noun(kind).is_none() => {
            muted("not counted: a rebuild records no units")
        }
        (None, None) => muted("no count recorded"),
    }
}

// ---------------------------------------------------------------------------
// the list
// ---------------------------------------------------------------------------

/// `GET /admin/build-jobs` — the tenant's newest jobs, newest first.
pub(super) async fn list(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let admin = match admin_auth(&state, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let rows = match jobs_of(&state) {
        Ok(q) => q.list(&admin.tenant.tenant_id, LIST_LIMIT).await,
        Err(e) => Err(e),
    };
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => return error_panel(&admin, ACTIVE, "Build jobs", &e),
    };
    let body = list_body(&rows);
    render(&admin, ACTIVE, "Build jobs", &body)
}

pub(super) fn list_body(rows: &[BuildJob]) -> String {
    let table = html_table(
        &[
            "job",
            "kind",
            "target",
            "state",
            "claims",
            "claimed by",
            "requested by",
            "created",
        ],
        &rows
            .iter()
            .map(|j| {
                vec![
                    format!("<code>{}</code>", link(&job_path(&j.job_id), &j.job_id)),
                    esc(&j.kind),
                    target(j),
                    state_pill(&j.state),
                    generation(j.attempts),
                    opt_code(&j.claimed_by),
                    esc(&j.requested_by),
                    esc(&j.created_at),
                ]
            })
            .collect::<Vec<_>>(),
    );
    let bound = if rows.len() as i64 >= LIST_LIMIT {
        format!(
            r#"<p class="muted">The newest {LIST_LIMIT} jobs are shown, as <code>GET /v1/index-build-jobs</code> returns them.</p>"#
        )
    } else {
        String::new()
    };
    format!(
        r#"<div class="card">{table}</div>{bound}
<div class="legend">claims = how many times a builder has claimed the job; each claim is a new generation, and only the current one may write. Open a job for its progress, history and per-version outcomes.</div>
{}"#,
        complete_result(
            "GET /v1/index-build-jobs",
            Some("mmctl datastore jobs list"),
            "",
            None,
        )
    )
}

// ---------------------------------------------------------------------------
// one job
// ---------------------------------------------------------------------------

/// The events read for one render: every page up to [`EVENT_PAGES`].
#[derive(Debug, Default)]
pub(super) struct EventsRead {
    pub events: Vec<JobEvent>,
    /// Events before the first one shown were pruned, or predate the ring.
    pub pruned_before: bool,
    /// A later page reported a gap: events were pruned while being read.
    pub pruned_within: bool,
    /// More events follow the last one shown.
    pub truncated: bool,
}

async fn read_events(q: &BuildJobs, tenant: &str, job: &JobId) -> Result<EventsRead, String> {
    let mut read = EventsRead::default();
    let mut after = 0;
    for page in 0..EVENT_PAGES {
        let Some(p) = q
            .events(tenant, job, after, EVENT_PAGE)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Err("the job is no longer visible".into());
        };
        if p.gap {
            if page == 0 {
                read.pruned_before = true;
            } else {
                read.pruned_within = true;
            }
        }
        let full = p.events.len() as i64 >= EVENT_PAGE;
        read.events.extend(p.events);
        after = p.through;
        if !full {
            return Ok(read);
        }
    }
    read.truncated = true;
    Ok(read)
}

/// `GET /admin/build-jobs/{job_id}`.
pub(super) async fn detail(
    State(state): State<Arc<AppState>>,
    Path(job_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let admin = match admin_auth(&state, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    let tenant = admin.tenant.tenant_id.clone();
    let id = match JobId::parse(&job_id) {
        Ok(id) => id,
        Err(e) => return error_panel(&admin, ACTIVE, "Build job", &e),
    };
    let q = match jobs_of(&state) {
        Ok(q) => q,
        Err(e) => return error_panel(&admin, ACTIVE, "Build job", &e),
    };
    // The job and its progress are one read: without it there is no page.
    let progress = match q.progress(&tenant, &id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            let e = KernelError::NotFound {
                kind: "build job",
                id: job_id,
            };
            return error_panel(&admin, ACTIVE, "Build job", &e);
        }
        Err(e) => return error_panel(&admin, ACTIVE, "Build job", &e),
    };
    // The other two reads are their own sections: a failure is shown there.
    let events = read_events(&q, &tenant, &id).await;
    let versions = q
        .version_history(&tenant, &id, None, VERSION_PAGE)
        .await
        .map_err(|e| e.to_string())
        .and_then(|h| h.ok_or_else(|| "the job is no longer visible".to_string()));
    let open = is_open(&progress.job.state);
    let body = detail_body(&progress, &events, &versions);
    render_refreshing(
        &admin,
        ACTIVE,
        &format!("Build job {}", progress.job.job_id),
        open.then_some(REFRESH_SECS),
        &body,
    )
}

pub(super) fn detail_body(
    p: &JobProgress,
    events: &Result<EventsRead, String>,
    versions: &Result<JobVersionHistory, String>,
) -> String {
    let job = &p.job;
    let refresh = if is_open(&job.state) {
        format!(
            r#"<p class="muted">This page reloads every {REFRESH_SECS} seconds while the job is {}; it stops once the job ends.</p>"#,
            esc(&job.state)
        )
    } else {
        String::new()
    };
    let mut facts = vec![
        ("job", code(&job.job_id)),
        ("kind", esc(&job.kind)),
        ("target", target(job)),
        ("state", state_pill(&job.state)),
        ("claim", generation(job.attempts)),
        ("claimed by", opt_code(&job.claimed_by)),
        ("requested by", esc(&job.requested_by)),
        ("created", esc(&job.created_at)),
    ];
    if let Some(c) = &job.correlation_id {
        facts.push(("correlation id", code(c)));
    }
    if let Some(e) = &job.error {
        facts.push(("error", esc(e)));
    }
    let dto = crate::datastore_jobs::job_dto(job.clone());
    format!(
        r#"{refresh}<div class="card">{}</div>
<h2>progress</h2>{}
<h2>event history</h2>{}
<h2>per-version outcomes</h2>{}
{}"#,
        kv(&facts),
        progress_section(p),
        events_section(&job.job_id, events),
        versions_section(&job.job_id, versions),
        complete_result(
            &api_path(&job.job_id, ""),
            Some(&format!(
                "mmctl datastore jobs get {}",
                shell_word(&job.job_id)
            )),
            "",
            serde_json::to_value(&dto).ok().as_ref(),
        ),
    )
}

pub(super) fn progress_section(p: &JobProgress) -> String {
    let job = &p.job;
    let phase = match &p.phase {
        Some(phase) => pill(phase, Tone::Neutral),
        None => muted("none recorded"),
    };
    let advanced = match &p.last_advance_at {
        Some(t) => esc(t),
        None => muted("no advance recorded"),
    };
    let note = if job.attempts > 0 && p.phase.is_none() && p.last_advance_at.is_none() {
        r#"<p class="muted">No progress is recorded for this claim. A claim starts with none; a v1.4 builder never records any.</p>"#
    } else {
        ""
    };
    format!(
        r#"<div class="card">{}</div>{note}{}"#,
        kv(&[
            ("phase", phase),
            (
                "progress",
                units_label(&job.kind, p.units_done, p.units_total)
            ),
            ("last advance", advanced),
            ("recorded by claim", generation(job.attempts)),
        ]),
        api_line(&api_path(&job.job_id, "/progress")),
    )
}

pub(super) fn events_section(job_id: &str, read: &Result<EventsRead, String>) -> String {
    let api = api_line(&api_path(job_id, "/events"));
    let read = match read {
        Ok(r) => r,
        Err(e) => return format!("{}{api}", unavailable("the event history", e)),
    };
    let mut notices = String::new();
    if read.pruned_before {
        let detail = match read.events.first() {
            Some(first) => format!(
                "Events before #{} are no longer kept: retention prunes each tenant's oldest events, and a job created before event history existed has none for its earlier transitions. The list starts at the oldest surviving event.",
                first.seq
            ),
            None => "No event for this job survives retention. Its state above is unaffected: the job row never depends on the event history.".into(),
        };
        notices.push_str(&format!(
            r#"<div class="notice">{} {}</div>"#,
            pill("older history pruned", Tone::Warn),
            esc(&detail)
        ));
    }
    if read.pruned_within {
        notices.push_str(&format!(
            r#"<div class="notice">{} {}</div>"#,
            pill("history pruned while reading", Tone::Warn),
            esc("Retention pruned events between the pages this view read; reload to read the history again.")
        ));
    }
    let table = if read.events.is_empty() {
        if read.pruned_before {
            String::new()
        } else {
            muted("no events recorded")
        }
    } else {
        html_table(
            &[
                "#",
                "recorded",
                "event",
                "state",
                "claim",
                "claimed by",
                "phase",
            ],
            &read
                .events
                .iter()
                .map(|e| {
                    let transition = match &e.from_state {
                        Some(from) if from != &e.to_state => {
                            format!("{} → {}", esc(from), esc(&e.to_state))
                        }
                        _ => esc(&e.to_state),
                    };
                    vec![
                        e.seq.to_string(),
                        esc(&e.recorded_at),
                        esc(&e.kind),
                        transition,
                        generation(e.generation),
                        opt_code(&e.node_id),
                        e.phase.as_deref().map(esc).unwrap_or_else(|| "—".into()),
                    ]
                })
                .collect::<Vec<_>>(),
        )
    };
    let truncated = if read.truncated {
        format!(
            r#"<p class="muted">The oldest {} events are shown; page on with the API's <code>next_cursor</code> for the rest.</p>"#,
            read.events.len()
        )
    } else {
        String::new()
    };
    format!(
        r#"{notices}<div class="card">{table}</div>{truncated}<div class="legend">Each claim, change of state and change of progress phase is one event, oldest first. Heartbeats and progress counts are not events.</div>{api}"#
    )
}

pub(super) fn versions_section(job_id: &str, read: &Result<JobVersionHistory, String>) -> String {
    let api = api_line(&api_path(job_id, "/versions"));
    let h = match read {
        Ok(h) => h,
        Err(e) => return format!("{}{api}", unavailable("the per-version outcomes", e)),
    };
    let status = match h.history {
        HistoryStatus::Recorded => String::new(),
        HistoryStatus::Partial => format!(
            r#"<div class="notice">{} {}</div>"#,
            pill("partial history", Tone::Warn),
            esc("Some claims of this job were made by a v1.4 builder, which kept no per-version records. Versions those claims touched may be missing below.")
        ),
        HistoryStatus::Unavailable => format!(
            r#"<div class="notice">{} {}</div>"#,
            pill("history unavailable", Tone::Warn),
            esc("Only a v1.4 builder claimed this job, and it kept no per-version records. An empty list here means unknown, not that the job touched nothing.")
        ),
    };
    let table = if h.versions.is_empty() {
        match h.history {
            HistoryStatus::Recorded => muted("none: this job touched no index version"),
            // Unknown, said once in the notice above; never "none".
            _ => String::new(),
        }
    } else {
        html_table(
            &[
                "index version",
                "selected",
                "logical version",
                "disposition",
                "attempts",
                "newest attempt",
                "manifest",
                "catalog",
                "staged",
            ],
            &h.versions
                .iter()
                .map(|v| {
                    let attempt = match (&v.attempt_id, v.attempt_generation, &v.attempt_state) {
                        (Some(id), g, s) => format!(
                            "{}{}{}",
                            code(id),
                            g.map(|g| format!(" · generation {g}")).unwrap_or_default(),
                            s.as_deref()
                                .map(|s| format!(" · {}", esc(s)))
                                .unwrap_or_default()
                        ),
                        _ => "—".into(),
                    };
                    vec![
                        code(&v.index_version_id),
                        v.selected_ordinal
                            .map(|o| format!("#{}", o + 1))
                            .unwrap_or_else(|| "—".into()),
                        match v.committed_generation {
                            Some(g) => format!("committed by generation {g}"),
                            None => "not committed".into(),
                        },
                        v.disposition
                            .as_deref()
                            .map(esc)
                            .unwrap_or_else(|| "—".into()),
                        v.attempt_count.to_string(),
                        attempt,
                        esc(crate::datastore_jobs::manifest_label(
                            v.intent_state.as_deref(),
                        )),
                        v.artifact_state
                            .as_deref()
                            .map(esc)
                            .unwrap_or_else(|| "—".into()),
                        if v.staged_bound { "bound" } else { "not bound" }.into(),
                    ]
                })
                .collect::<Vec<_>>(),
        )
    };
    let more = match &h.next_after {
        Some(next) => format!(
            r#"<p class="muted">The first {} versions are shown; continue with <code class="cmd">{}?cursor={}</code>.</p>"#,
            h.versions.len(),
            esc(&api_path(job_id, "/versions")),
            esc(&path_segment(next))
        ),
        None => String::new(),
    };
    format!(
        r#"{status}<div class="card">{table}</div>{more}<div class="legend">manifest: none = no publication intent; unknown = an intent not yet resolved by the worker or the reconciler; published, converged or failed = its recorded outcome. A selected version with no attempt, intent or disposition was never reached.</div>{api}"#
    )
}

#[cfg(test)]
mod tests {
    //! The rendering rules, without a database: labelled and unknown
    //! counts, the pruned-history notice, the unavailable per-version
    //! history and a failed read. The PostgreSQL round trip is in the
    //! console's test module.
    use super::*;
    use munarium_store_pg::jobs::JobVersionOutcome;

    fn job(kind: &str, state: &str, attempts: i32) -> BuildJob {
        BuildJob {
            tenant_id: "t".into(),
            job_id: "bjob-u12".into(),
            kind: kind.into(),
            scope_kind: Some("collection".into()),
            scope_id: Some("col-u12".into()),
            index_version_id: None,
            params: serde_json::json!({}),
            state: state.into(),
            attempt_ids: vec![],
            correlation_id: None,
            claimed_by: Some("builder-a".into()),
            attempts,
            result: None,
            error: None,
            error_code: None,
            requested_by: "api:rw".into(),
            created_at: "2026-11-20 10:00:00+00".into(),
        }
    }

    fn progress(kind: &str, done: Option<i64>, total: Option<i64>) -> JobProgress {
        JobProgress {
            job: job(kind, "running", 1),
            phase: Some("extracting".into()),
            units_done: done,
            units_total: total,
            last_advance_at: Some("2026-11-20 10:00:05+00".into()),
        }
    }

    fn event(seq: i64) -> JobEvent {
        JobEvent {
            seq,
            kind: "state".into(),
            from_state: Some("pending".into()),
            to_state: "running".into(),
            generation: 1,
            node_id: Some("builder-a".into()),
            phase: None,
            recorded_at: "2026-11-20 10:00:01+00".into(),
        }
    }

    fn history(status: HistoryStatus, versions: Vec<JobVersionOutcome>) -> JobVersionHistory {
        JobVersionHistory {
            job: job("backfill", "succeeded", 1),
            history: status,
            versions,
            next_after: None,
        }
    }

    #[test]
    fn counts_say_what_they_count_and_unknown_totals_stay_unknown() {
        assert_eq!(units_label("direct", Some(3), Some(10)), "3 of 10 sources");
        assert_eq!(units_label("backfill", Some(1), Some(4)), "1 of 4 versions");
        let open = units_label("direct", Some(3), None);
        assert!(open.contains("3 sources done"), "{open}");
        assert!(open.contains("total unknown"), "{open}");
        for (kind, done, total) in [
            ("direct", Some(0), None),
            ("direct", None, None),
            ("rebuild", None, None),
        ] {
            let label = units_label(kind, done, total);
            assert!(!label.contains("of 0"), "{label}");
        }
        assert!(units_label("rebuild", None, None).contains("records no units"));
        assert!(units_label("direct", None, None).contains("no count recorded"));
    }

    #[test]
    fn a_pruned_history_says_so_and_a_complete_one_does_not() {
        let whole = EventsRead {
            events: vec![event(1), event(2)],
            ..Default::default()
        };
        let body = events_section("bjob-u12", &Ok(whole));
        assert!(!body.contains("older history pruned"), "{body}");
        assert!(body.contains("pending → running"), "{body}");

        let pruned = EventsRead {
            events: vec![event(7), event(8)],
            pruned_before: true,
            ..Default::default()
        };
        let body = events_section("bjob-u12", &Ok(pruned));
        assert!(body.contains("older history pruned"), "{body}");
        assert!(
            body.contains("Events before #7 are no longer kept"),
            "{body}"
        );

        // Nothing survives: the notice, and never "no events recorded".
        let gone = EventsRead {
            pruned_before: true,
            ..Default::default()
        };
        let body = events_section("bjob-u12", &Ok(gone));
        assert!(body.contains("older history pruned"), "{body}");
        assert!(!body.contains("no events recorded"), "{body}");

        let mid = EventsRead {
            events: vec![event(1)],
            pruned_within: true,
            truncated: true,
            ..Default::default()
        };
        let body = events_section("bjob-u12", &Ok(mid));
        assert!(body.contains("history pruned while reading"), "{body}");
        assert!(body.contains("next_cursor"), "{body}");
    }

    #[test]
    fn a_failed_read_is_unavailable_never_empty() {
        let body = events_section("bjob-u12", &Err("connection refused".into()));
        assert!(body.contains("unavailable"), "{body}");
        assert!(body.contains("connection refused"), "{body}");
        assert!(!body.contains("no events recorded"), "{body}");
        let body = versions_section("bjob-u12", &Err("connection refused".into()));
        assert!(body.contains("unavailable"), "{body}");
        assert!(!body.contains("touched no index version"), "{body}");
    }

    #[test]
    fn v14_history_is_unavailable_and_recorded_empty_is_none() {
        let body = versions_section("bjob-u12", &Ok(history(HistoryStatus::Unavailable, vec![])));
        assert!(body.contains("history unavailable"), "{body}");
        assert!(body.contains("means unknown"), "{body}");
        assert!(!body.contains("touched no index version"), "{body}");

        let body = versions_section("bjob-u12", &Ok(history(HistoryStatus::Recorded, vec![])));
        assert!(body.contains("touched no index version"), "{body}");
        assert!(!body.contains("history unavailable"), "{body}");

        let body = versions_section("bjob-u12", &Ok(history(HistoryStatus::Partial, vec![])));
        assert!(body.contains("partial history"), "{body}");
        assert!(!body.contains("touched no index version"), "{body}");
    }

    #[test]
    fn the_detail_keeps_facts_separate_and_offers_the_scripted_route() {
        let body = detail_body(
            &progress("direct", Some(2), Some(5)),
            &Ok(EventsRead::default()),
            &Ok(history(HistoryStatus::Recorded, vec![])),
        );
        assert!(body.contains("2 of 5 sources"), "{body}");
        assert!(body.contains("GET /v1/index-build-jobs/bjob-u12"), "{body}");
        assert!(
            body.contains("GET /v1/index-build-jobs/bjob-u12/progress"),
            "{body}"
        );
        assert!(body.contains("mmctl datastore jobs get bjob-u12"), "{body}");
        // The ClearView reads have no CLI command; none is invented.
        assert!(!body.contains("mmctl datastore jobs progress"), "{body}");
        assert!(!body.contains("mmctl datastore jobs events"), "{body}");
        assert!(body.contains("reloads every 5 seconds"), "{body}");
        let done = JobProgress {
            job: job("direct", "succeeded", 1),
            ..progress("direct", Some(5), Some(5))
        };
        let body = detail_body(
            &done,
            &Ok(EventsRead::default()),
            &Ok(history(HistoryStatus::Recorded, vec![])),
        );
        assert!(!body.contains("reloads every"), "{body}");
    }
}
