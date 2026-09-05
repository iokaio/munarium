// SPDX-License-Identifier: Apache-2.0
//! The platform surface, proven through the TYPED client planes —
//! a native port of `server/conformance/src/platform.rs` (same scenario
//! names, `platform.` prefix, so CI output is comparable), plus the SSE
//! streaming-turn smoke and a route-coverage sweep the raw suite predates.
//!
//! Where the raw suite asserts HTTP statuses + problem slugs, this port
//! asserts the TYPED errors the client decodes them into — that mapping is
//! exactly what the client exists to provide. Requires the pg store, an rw
//! and a mgmt static token on the SAME tenant, and `MUNARIUM_TOKEN_SECRET`
//! configured server-side. Zero provider keys — nothing here completes.
//!
//! Re-runnable against a shared dev tenant BY DESIGN: content and doomed
//! runbook versions are nonce'd, and no scenario asserts global tenant
//! state beyond what this run created.

use crate::smoke::{nonce, Report};
use base64::Engine as _;
use futures_util::StreamExt;
use munarium_client::{dto, MunariumClient, MunariumClientOptions, MunariumError, TurnStreamEvent};
use sha2::Digest as _;

type R = Result<(), String>;

macro_rules! expect {
    ($cond:expr, $($msg:tt)*) => {
        if !$cond {
            return Err(format!($($msg)*));
        }
    };
}

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}

fn sha(s: &str) -> String {
    hex::encode(sha2::Sha256::digest(s.as_bytes()))
}

/// Guarded per-item access — a short results array must become a counted
/// FAIL line, never an index panic that aborts the whole binary.
fn nth<'a>(
    results: &'a [dto::IngestResultDto],
    i: usize,
    what: &str,
) -> Result<&'a dto::IngestResultDto, String> {
    results.get(i).ok_or_else(|| {
        format!(
            "{what}: expected result #{i}, got {} results",
            results.len()
        )
    })
}

struct Env {
    base: String,
    grpc_url: Option<String>,
    rw_token: String,
    mgmt_token: String,
    /// One pooled client per role, built once — the connection behavior a
    /// real consumer has (and what the scenarios should exercise).
    ops: MunariumClient,
    mgr: MunariumClient,
    /// comp-bob's capability token, minted by the application scenario and
    /// reused by the SSE scenario — the same ordering dependency the
    /// server's own suite has.
    bob_token: Option<String>,
}

impl Env {
    fn new(
        base: &str,
        grpc_url: Option<&str>,
        rw_token: &str,
        mgmt_token: &str,
    ) -> Result<Self, String> {
        let base = base.trim_end_matches('/').to_string();
        let mk = |token: &str, uid: &str| {
            MunariumClient::rest(
                MunariumClientOptions::new(base.clone())
                    .token(token)
                    .uid(uid),
            )
            .map_err(|e| format!("rest client for {uid}: {e}"))
        };
        Ok(Self {
            ops: mk(rw_token, "ops")?,
            mgr: mk(mgmt_token, "mgr")?,
            base,
            grpc_url: grpc_url.map(String::from),
            rw_token: rw_token.to_string(),
            mgmt_token: mgmt_token.to_string(),
            bob_token: None,
        })
    }

    /// A one-off client for a minted persona (capability JWT + its uid).
    fn rest(&self, token: &str, uid: &str) -> Result<MunariumClient, String> {
        MunariumClient::rest(
            MunariumClientOptions::new(self.base.clone())
                .token(token)
                .uid(uid),
        )
        .map_err(|e| format!("rest client for {uid}: {e}"))
    }

    /// Mint a capability token via the typed tokens plane.
    async fn mint(
        &self,
        uid: &str,
        level: i32,
        compartments: &[&str],
        scopes: &[&str],
    ) -> Result<(String, String), String> {
        let resp = self
            .mgr
            .tokens
            .mint(dto::IssueTokenRequest {
                uid: uid.into(),
                access_level: level,
                compartments: compartments.iter().map(|s| s.to_string()).collect(),
                scopes: scopes.iter().map(|s| s.to_string()).collect(),
                runbook_refs: None,
                ttl_secs: None,
            })
            .await
            .map_err(|e| format!("mint for {uid}: {e}"))?;
        Ok((resp.token, resp.jti))
    }
}

const SHAPE_YAML: &str = "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: { name: entdocs, version: 1 }\nspec:\n  fact:\n    schema: { type: object }\n";

fn runbook_yaml(version: u32) -> String {
    format!(
        r#"apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: {{ name: ent-support, version: {version} }}
spec:
  collections:
    - name: ent-public
      shape: entdocs@1
      accessLevel: 0
      sources: {{ filenamePrefix: "public/" }}
    - name: ent-secret
      shape: entdocs@1
      accessLevel: 2
      compartments: [eng]
      sources: {{ filenamePrefix: "eng/" }}
  retrieval: {{ topK: 5 }}
  models:
    default: {{ provider: default, tier: fast }}
    allowOverrides: [default]
  completion:
    promptTemplate: "Answer from context only.\n{{context}}\n\nQ: {{query}}"
  steps:
    - resolveSources: {{}}
    - buildIndex: {{}}
    - verify: {{}}
    - cutover: {{ approval: required }}
    - retireOld: {{ keep_versions: 2 }}
"#
    )
}

// ---------------------------------------------------------------------------
// scenarios
// ---------------------------------------------------------------------------

/// The uid contract: no uid draws the typed uid-required rejection; a JWT
/// presented under a different uid draws the typed 403.
async fn uid_contract(env: &Env) -> R {
    let no_uid =
        MunariumClient::rest(MunariumClientOptions::new(env.base.clone()).token(&env.rw_token))
            .map_err(|e| e.to_string())?;
    match no_uid.runbooks.list(false).await {
        Err(MunariumError::InvalidInput { detail }) => {
            expect!(
                detail.contains("uid"),
                "uid-required detail should name the uid, got '{detail}'"
            );
        }
        other => {
            return Err(format!(
                "missing uid must be typed InvalidInput, got {other:?}"
            ))
        }
    }

    let (token, _) = env.mint("uid-alice", 0, &[], &["query"]).await?;
    let mallory = env.rest(&token, "mallory")?;
    match mallory.runbooks.list(false).await {
        Err(MunariumError::Forbidden { .. }) => Ok(()),
        other => Err(format!(
            "uid mismatch must be typed Forbidden, got {other:?}"
        )),
    }
}

/// Role partition: rw cannot mint tokens; mgmt cannot write the ledger.
async fn role_partition(env: &Env) -> R {
    match env
        .ops
        .tokens
        .mint(dto::IssueTokenRequest {
            uid: "x".into(),
            access_level: 0,
            compartments: vec![],
            scopes: vec!["query".into()],
            runbook_refs: None,
            ttl_secs: None,
        })
        .await
    {
        Err(MunariumError::Forbidden { .. }) => {}
        other => return Err(format!("rw minting must be Forbidden, got {other:?}")),
    }

    match env
        .mgr
        .commands
        .create_version(Default::default(), None)
        .await
    {
        Err(MunariumError::Forbidden { .. }) => Ok(()),
        other => Err(format!(
            "mgmt ledger write must be Forbidden, got {other:?}"
        )),
    }
}

/// The full retrieval-application lifecycle + compartmentalized sessions.
async fn application_and_compartments(env: &mut Env) -> R {
    let ops = &env.ops;
    ops.runbooks
        .apply_shape(SHAPE_YAML, None)
        .await
        .map_err(|e| format!("apply shape: {e}"))?;

    // Validation first: clean passes, topK: 0 invalidates.
    let clean = ops
        .runbooks
        .validate(&runbook_yaml(1), Default::default())
        .await
        .map_err(|e| format!("validate clean: {e}"))?;
    expect!(clean.valid, "clean runbook must validate: {clean:?}");
    let broken = runbook_yaml(1).replace("topK: 5", "topK: 0");
    let bad = ops
        .runbooks
        .validate(&broken, Default::default())
        .await
        .map_err(|e| format!("validate broken: {e}"))?;
    expect!(!bad.valid, "topK: 0 must invalidate, got {bad:?}");

    ops.runbooks
        .apply_runbook(&runbook_yaml(1))
        .await
        .map_err(|e| format!("apply runbook: {e}"))?;

    // Ingest via the file plane under the ingest scope; matchers auto-bind.
    let (ingest_token, _) = env.mint("loader", 2, &["eng"], &["ingest"]).await?;
    let loader = env.rest(&ingest_token, "loader")?;
    let batch = loader
        .ingest
        .ingest_batch(dto::IngestBatchRequest {
            files: vec![
                dto::IngestFileRequest {
                    filename: "public/handbook.md".into(),
                    media_type: "text/markdown".into(),
                    content_base64: b64("The public handbook grants twenty vacation days."),
                    sha256: None,
                    collections: None,
                },
                dto::IngestFileRequest {
                    filename: "eng/launch.md".into(),
                    media_type: "text/markdown".into(),
                    content_base64: b64("Secret launch window: vacation blackout in Q4."),
                    sha256: None,
                    collections: None,
                },
            ],
        })
        .await
        .map_err(|e| format!("batch ingest: {e}"))?;
    expect!(batch.results.len() == 2, "expected 2 results: {batch:?}");
    expect!(
        nth(&batch.results, 0, "batch")?.bound_to == vec!["ent-public".to_string()]
            && nth(&batch.results, 1, "batch")?.bound_to == vec!["ent-secret".to_string()],
        "matcher auto-bind wrong: {batch:?}"
    );

    // A level-0 ingest token must NOT write into ent-secret.
    let (low_token, _) = env.mint("lowloader", 0, &[], &["ingest"]).await?;
    let low = env.rest(&low_token, "lowloader")?;
    match low
        .ingest
        .ingest(dto::IngestFileRequest {
            filename: "sneak.md".into(),
            media_type: "text/markdown".into(),
            content_base64: b64("nope"),
            sha256: None,
            collections: Some(vec!["ent-secret".into()]),
        })
        .await
    {
        Err(MunariumError::Forbidden { .. }) => {}
        other => {
            return Err(format!(
                "low-clearance write must be Forbidden, got {other:?}"
            ))
        }
    }

    // Run with two per-collection approval passes.
    let run = ops
        .runbooks
        .run_runbook("ent-support", None)
        .await
        .map_err(|e| format!("run: {e}"))?;
    expect!(
        run.state == "awaiting_approval",
        "run must pause, got '{}'",
        run.state
    );
    for _pass in 0..2 {
        let status = ops
            .runbooks
            .get_run(&run.run_id)
            .await
            .map_err(|e| e.to_string())?;
        let awaiting = status
            .steps
            .iter()
            .find(|s| s.state == "awaiting_approval")
            .ok_or_else(|| format!("no step awaiting approval: {status:?}"))?;
        ops.runbooks
            .approve_step(&run.run_id, awaiting.ordinal)
            .await
            .map_err(|e| format!("approve {}: {e}", awaiting.ordinal))?;
    }
    let done = ops
        .runbooks
        .get_run(&run.run_id)
        .await
        .map_err(|e| e.to_string())?;
    expect!(done.state == "done", "run must finish done: {done:?}");

    // List + info expose per-collection access requirements.
    let list = ops
        .runbooks
        .list(false)
        .await
        .map_err(|e| format!("list runbooks: {e}"))?;
    let entry = list
        .runbooks
        .iter()
        .find(|b| b.runbook_ref == "ent-support@1")
        .ok_or_else(|| "ent-support@1 missing from list".to_string())?;
    let levels: Vec<i32> = entry.collections.iter().map(|c| c.access_level).collect();
    expect!(
        levels.contains(&0) && levels.contains(&2),
        "list must show levels 0 and 2: {levels:?}"
    );
    let info = ops
        .runbooks
        .get_info("ent-support")
        .await
        .map_err(|e| format!("get_info: {e}"))?;
    expect!(
        info.collections.len() == 2 && info.has_completion,
        "info must carry both collections + completion: {info:?}"
    );

    // Two clearances, one runbook: disjoint result sets for one query.
    let (alice_token, _) = env.mint("comp-alice", 0, &[], &["query"]).await?;
    let (bob_token, _) = env.mint("comp-bob", 2, &["eng"], &["query"]).await?;
    let alice = env.rest(&alice_token, "comp-alice")?;
    let bob = env.rest(&bob_token, "comp-bob")?;

    let session_a = alice
        .sessions
        .create("ent-support")
        .await
        .map_err(|e| format!("alice session: {e}"))?;
    expect!(
        session_a.permitted_collections == vec!["ent-public".to_string()],
        "alice must see only ent-public: {session_a:?}"
    );
    let session_b = bob
        .sessions
        .create("ent-support")
        .await
        .map_err(|e| format!("bob session: {e}"))?;
    expect!(
        session_b.permitted_collections.len() == 2,
        "bob must see both collections: {session_b:?}"
    );

    let turn_a = alice
        .sessions
        .turn(
            &session_a.session_id,
            dto::TurnRequest {
                query: "vacation".into(),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("alice turn: {e}"))?;
    expect!(
        !turn_a.hits.is_empty() && turn_a.hits.iter().all(|h| h.collection == "ent-public"),
        "alice hits must be ent-public only: {turn_a:?}"
    );

    let turn_b = bob
        .sessions
        .turn(
            &session_b.session_id,
            dto::TurnRequest {
                query: "vacation".into(),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("bob turn: {e}"))?;
    expect!(
        turn_b.hits.iter().any(|h| h.collection == "ent-secret"),
        "bob's merged hits must include ent-secret: {turn_b:?}"
    );
    expect!(
        turn_b.envelopes.len() == 2,
        "bob must get one envelope per collection: {turn_b:?}"
    );

    // Multiturn continuity, transcript readback, cross-uid refusal.
    let turn2 = bob
        .sessions
        .turn(
            &session_b.session_id,
            dto::TurnRequest {
                query: "blackout".into(),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("bob turn 2: {e}"))?;
    expect!(turn2.ordinal == 2, "follow-on turn must be ordinal 2");
    let readback = bob
        .sessions
        .get(&session_b.session_id)
        .await
        .map_err(|e| format!("session readback: {e}"))?;
    expect!(
        readback.turns.len() == 2 && readback.state == "open",
        "transcript must hold both turns: {readback:?}"
    );
    match alice
        .sessions
        .turn(
            &session_b.session_id,
            dto::TurnRequest {
                query: "x".into(),
                ..Default::default()
            },
        )
        .await
    {
        Err(MunariumError::Forbidden { .. }) => {}
        other => return Err(format!("cross-uid turn must be Forbidden, got {other:?}")),
    }

    // Model-override policy refusal (checked BEFORE any provider spend).
    match bob
        .sessions
        .turn(
            &session_b.session_id,
            dto::TurnRequest {
                query: "x".into(),
                complete: Some(true),
                model_override: Some(dto::ModelOverrideDto {
                    provider: Some("not-allowed-provider".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
    {
        Err(MunariumError::Forbidden { .. }) => {}
        other => {
            return Err(format!(
                "disallowed override must be Forbidden, got {other:?}"
            ))
        }
    }

    // Scope enforcement: a query token cannot ingest.
    match bob
        .ingest
        .ingest(dto::IngestFileRequest {
            filename: "x.md".into(),
            media_type: "text/markdown".into(),
            content_base64: b64("x"),
            sha256: None,
            collections: None,
        })
        .await
    {
        Err(MunariumError::Forbidden { .. }) => {}
        other => return Err(format!("scope-missing must be Forbidden, got {other:?}")),
    }

    env.bob_token = Some(bob_token);
    Ok(())
}

/// Soft removal is double-pass and leaves data intact.
async fn removal_double_pass(env: &Env) -> R {
    let ops = &env.ops;
    // The doomed version is NONCE'D (seconds since epoch): removal is
    // permanent, so a fixed number makes this scenario single-use against a
    // shared dev tenant (proven live — the second run drew "was removed;
    // publish a new version instead").
    let doomed_version: u32 = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        % 2_000_000_000) as u32;
    let doomed = format!("ent-support@{doomed_version}");
    ops.runbooks
        .apply_runbook(&runbook_yaml(doomed_version))
        .await
        .map_err(|e| format!("apply {doomed}: {e}"))?;

    // Single-pass confirm is refused (409 removal-not-confirmed → typed).
    match ops.runbooks.remove_confirm(&doomed, "rm-guess").await {
        Err(MunariumError::InvalidInput { .. }) => {}
        other => {
            return Err(format!(
                "confirm without request must be typed, got {other:?}"
            ))
        }
    }

    let removal = ops
        .runbooks
        .remove_request(&doomed)
        .await
        .map_err(|e| format!("remove-request: {e}"))?;
    expect!(
        !removal.removal_id.is_empty(),
        "removal_id missing: {removal:?}"
    );

    // A WRONG removal_id must draw the SAME typed refusal as no request —
    // accepting any error here would let a transient 503 or a routing bug
    // masquerade as the double-pass guard working.
    match ops.runbooks.remove_confirm(&doomed, "rm-wrong").await {
        Err(MunariumError::InvalidInput { .. }) => {}
        other => {
            return Err(format!(
                "wrong removal_id must be the typed removal-not-confirmed, got {other:?}"
            ))
        }
    }

    let confirmed = ops
        .runbooks
        .remove_confirm(&doomed, &removal.removal_id)
        .await
        .map_err(|e| format!("confirm: {e}"))?;
    expect!(confirmed.status == "removed", "confirm: {confirmed:?}");

    // Sessions on the removed exact ref: typed NotFound (410 runbook-removed);
    // the bare name still resolves to a LIVE version — not asserted to be @1,
    // because earlier smoke runs against a shared tenant may have left other
    // versions, but never the one this run just removed.
    let (token, _) = env.mint("rm-user", 0, &[], &["query"]).await?;
    let user = env.rest(&token, "rm-user")?;
    match user.sessions.create(&doomed).await {
        Err(MunariumError::NotFound { .. }) => {}
        other => return Err(format!("removed ref must be typed NotFound, got {other:?}")),
    }
    let live = user
        .sessions
        .create("ent-support")
        .await
        .map_err(|e| format!("bare-name session: {e}"))?;
    expect!(
        live.runbook_ref.starts_with("ent-support@") && live.runbook_ref != doomed,
        "bare name must resolve to a live version: {live:?}"
    );

    // Hidden from the default list; visible with include_removed.
    let list = ops.runbooks.list(false).await.map_err(|e| e.to_string())?;
    expect!(
        !list.runbooks.iter().any(|b| b.runbook_ref == doomed),
        "removed ref must be hidden from the default list"
    );
    let all = ops.runbooks.list(true).await.map_err(|e| e.to_string())?;
    expect!(
        all.runbooks.iter().any(|b| b.runbook_ref == doomed),
        "include_removed must show it"
    );
    Ok(())
}

/// Reports are mgmt-gated and reflect this suite's traffic; revocation
/// lands in the issuance audit.
async fn reports_and_revoke(env: &Env) -> R {
    match env
        .ops
        .reports
        .usage(munarium_client::UsageQuery {
            group_by: Some("uid".into()),
            ..Default::default()
        })
        .await
    {
        Err(MunariumError::Forbidden { .. }) => {}
        other => return Err(format!("rw on reports must be Forbidden, got {other:?}")),
    }

    let mgr = &env.mgr;
    let usage = mgr
        .reports
        .usage(munarium_client::UsageQuery {
            group_by: Some("uid".into()),
            ..Default::default()
        })
        .await
        .map_err(|e| format!("usage: {e}"))?;
    let keys: Vec<&str> = usage.rows.iter().map(|r| r.key.as_str()).collect();
    expect!(
        keys.contains(&"comp-alice") && keys.contains(&"comp-bob"),
        "usage rows must include the session uids: {keys:?}"
    );

    let audit = mgr
        .reports
        .audit(munarium_client::AuditQuery {
            uid: Some("comp-bob".into()),
            limit: Some(10),
            ..Default::default()
        })
        .await
        .map_err(|e| format!("audit: {e}"))?;
    expect!(
        !audit.entries.is_empty(),
        "audit for comp-bob must be non-empty"
    );

    // The dashboard-view reports answer too (2026-08-18 routes).
    let ts = mgr
        .reports
        .timeseries(Some("24h"), None)
        .await
        .map_err(|e| format!("timeseries: {e}"))?;
    expect!(ts.window == "24h", "timeseries window echo: {ts:?}");
    let eps = mgr
        .reports
        .endpoints(Some("24h"), Some(5))
        .await
        .map_err(|e| format!("endpoints: {e}"))?;
    expect!(!eps.rows.is_empty(), "endpoint rows must reflect traffic");
    mgr.reports
        .runbooks(Some("24h"))
        .await
        .map_err(|e| format!("runbook report: {e}"))?;
    let sess = mgr
        .reports
        .sessions(Some("24h"))
        .await
        .map_err(|e| format!("sessions report: {e}"))?;
    expect!(
        sess.buckets.iter().any(|b| b.turns > 0),
        "sessions report must show the turns this suite took: {sess:?}"
    );
    mgr.reports
        .cost(None, None)
        .await
        .map_err(|e| format!("cost: {e}"))?;

    // The S-3.5 operator views. No runbook in this suite declares a research
    // profile, so the evidence report is the governing invariant measured on
    // a live server: every turn the suite took must land in `legacy_turns`,
    // and no layer rows may appear from turns that ran no hierarchy.
    let ev = mgr
        .reports
        .evidence(Some("24h"))
        .await
        .map_err(|e| format!("evidence report: {e}"))?;
    expect!(ev.window == "24h", "evidence window echo: {ev:?}");
    expect!(
        ev.legacy_turns > 0 && ev.hierarchy_turns == 0 && ev.layers.is_empty(),
        "unprofiled turns must report as legacy with no layer stats: {ev:?}"
    );

    let mx = mgr
        .reports
        .matrix()
        .await
        .map_err(|e| format!("matrix report: {e}"))?;
    expect!(
        mx.configured || (!mx.circuit_open && mx.consecutive_failures == 0),
        "an unwired Matrix plane must not read as a tripped breaker: {mx:?}"
    );

    // Revoke: the deny-list row lands and the audit shows it.
    let (_, jti) = env.mint("revokee", 0, &[], &["query"]).await?;
    let revoked = mgr
        .tokens
        .revoke(&jti)
        .await
        .map_err(|e| format!("revoke: {e}"))?;
    expect!(revoked.revoked, "revoke must land: {revoked:?}");
    let tokens = mgr
        .tokens
        .list(munarium_client::TokenListQuery {
            uid: Some("revokee".into()),
            active: None,
        })
        .await
        .map_err(|e| format!("token list: {e}"))?;
    expect!(
        tokens
            .tokens
            .first()
            .is_some_and(|t| t.revoked_at.is_some()),
        "issuance audit must show revoked_at: {tokens:?}"
    );
    Ok(())
}

/// Guided authoring end to end, keyless: catalog → draft → answers →
/// validate → assist (degrades to a note) → export (hash-verified
/// client-side) → apply → hosted → cleaned up.
async fn authoring_lifecycle(env: &Env) -> R {
    let ops = &env.ops;
    let patterns = ops
        .authoring
        .list_patterns()
        .await
        .map_err(|e| format!("patterns: {e}"))?;
    expect!(
        patterns.patterns.len() == 7,
        "expected the 7 §19 patterns, got {}",
        patterns.patterns.len()
    );
    let detail = ops
        .authoring
        .get_pattern("ask-the-corpus")
        .await
        .map_err(|e| format!("pattern detail: {e}"))?;
    expect!(
        detail.runbook_yaml.contains("kind: Runbook"),
        "pattern detail carries the exemplar"
    );

    let draft = ops
        .authoring
        .create_draft(dto::CreateDraftRequest {
            name: "vendor-security".into(),
            pattern_id: Some("ask-the-corpus".into()),
            seed_from_exemplar: false,
        })
        .await
        .map_err(|e| format!("create draft: {e}"))?;
    expect!(!draft.draft_id.is_empty(), "draft_id missing");
    expect!(
        draft.interview.first().map(|s| s.id.as_str()) == Some("identity"),
        "interview starts at identity"
    );

    // The workspace listing + readback name the draft.
    let drafts = ops
        .authoring
        .list_drafts()
        .await
        .map_err(|e| format!("list drafts: {e}"))?;
    expect!(
        drafts.drafts.iter().any(|d| d.draft_id == draft.draft_id),
        "list_drafts must contain the new draft"
    );
    let readback = ops
        .authoring
        .get_draft(&draft.draft_id)
        .await
        .map_err(|e| format!("get draft: {e}"))?;
    expect!(
        readback.name == "vendor-security",
        "draft readback: {readback:?}"
    );

    // A blank draft refuses to export (409 authoring-draft-invalid → typed).
    match ops.authoring.export(&draft.draft_id).await {
        Err(MunariumError::InvalidInput { .. }) => {}
        other => return Err(format!("blank export must be typed, got {other:?}")),
    }

    let answers = serde_json::json!({
        "identity.description": "Vendor security reviews for procurement.",
        "prefix.root": "vendors/",
        "prefix.areas": [
            { "path": "public/", "description": "published attestations" },
            { "path": "contracts/", "description": "signed agreements" },
        ],
        "access.uniform_public": false,
        "access.area_levels": { "public": 0, "contracts": 2 },
        "access.area_compartments": { "contracts": ["legal"] },
    });
    let updated = ops
        .authoring
        .put_answers(
            &draft.draft_id,
            dto::UpdateAnswersRequest {
                answers,
                materialize: true,
            },
        )
        .await
        .map_err(|e| format!("answers: {e}"))?;
    expect!(
        updated.validation.as_ref().is_some_and(|v| v.valid),
        "canonical answers must validate clean: {:?}",
        updated.validation
    );
    expect!(
        updated.documents.len() == 2,
        "one shape + one runbook: {}",
        updated.documents.len()
    );

    // Assist DEGRADES keyless: 200 + assist_note, documents intact.
    let assist = ops
        .authoring
        .assist(&draft.draft_id, Default::default())
        .await
        .map_err(|e| format!("keyless assist must succeed: {e}"))?;
    expect!(
        assist.assist_note.is_some(),
        "keyless assist must carry a degrade note"
    );
    expect!(
        assist.documents.len() == 2,
        "assist must not lose documents"
    );

    let validation = ops
        .authoring
        .validate(&draft.draft_id)
        .await
        .map_err(|e| format!("validate: {e}"))?;
    expect!(validation.valid, "validate: {validation:?}");

    // Export: verify the manifest CLIENT-side, exactly as mmctl does.
    let bundle = ops
        .authoring
        .export(&draft.draft_id)
        .await
        .map_err(|e| format!("export: {e}"))?;
    expect!(
        bundle.kind == "MunariumAuthoringBundle",
        "bundle kind: {}",
        bundle.kind
    );
    let mut buf = String::new();
    for (path, yaml) in &bundle.files {
        let actual = hex::encode(sha2::Sha256::digest(yaml.as_bytes()));
        expect!(
            bundle.hashes.get(path) == Some(&actual),
            "per-file hash mismatch for {path}"
        );
        buf.push_str(path);
        buf.push('\0');
        buf.push_str(&actual);
        buf.push('\n');
    }
    let manifest = hex::encode(sha2::Sha256::digest(buf.as_bytes()));
    expect!(
        bundle.manifest_hash == manifest,
        "manifest hash mismatch (client-recomputed {manifest})"
    );
    expect!(
        bundle
            .apply_order
            .first()
            .is_some_and(|p| p.starts_with("shapes/")),
        "shapes apply first: {:?}",
        bundle.apply_order
    );

    let applied = ops
        .authoring
        .apply(&draft.draft_id)
        .await
        .map_err(|e| format!("apply: {e}"))?;
    expect!(applied.applied.len() == 2, "apply covers the set");
    let hosted = ops
        .runbooks
        .get_info("vendor-security")
        .await
        .map_err(|e| format!("applied runbook must be hosted: {e}"))?;
    expect!(
        hosted.collections.len() == 2,
        "applied runbook reaches its two collections"
    );

    // Draft cleanup — the client surface's one DELETE (soft, workspace-only).
    let deleted = ops
        .authoring
        .delete_draft(&draft.draft_id)
        .await
        .map_err(|e| format!("delete draft: {e}"))?;
    expect!(deleted.status == "deleted", "delete: {deleted:?}");
    Ok(())
}

/// Bulk upload sessions: manifest diff, chunked upload with per-file sha
/// verification, replay idempotency, finalize verification, the zero-byte
/// re-run — plus the CLIENT-side chunk-cap guard.
async fn bulk_upload_lifecycle(env: &Env) -> R {
    let ops = &env.ops;
    let shape = "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: { name: bulkdocs, version: 1 }\nspec:\n  fact:\n    schema: { type: object }\n";
    ops.runbooks
        .apply_shape(shape, None)
        .await
        .map_err(|e| format!("apply bulk shape: {e}"))?;
    let runbook = r#"apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: bulk-archive, version: 1 }
spec:
  collections:
    - name: bulk-open-docs
      shape: bulkdocs@1
      accessLevel: 0
      sources: { filenamePrefix: "bulkdocs/" }
  retrieval: { topK: 5 }
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
    - retireOld: { keep_versions: 2 }
"#;
    ops.runbooks
        .apply_runbook(runbook)
        .await
        .map_err(|e| format!("apply bulk runbook: {e}"))?;

    let (ingest_token, _) = env.mint("bulkloader", 0, &[], &["ingest"]).await?;
    let loader = env.rest(&ingest_token, "bulkloader")?;
    // Nonce'd contents: this scenario re-runs against a shared dev server,
    // and the zero-byte-re-run assertion needs fresh bytes each run.
    let n = nonce();
    let a = format!("Bulk document alpha {n}: the treaty was signed.");
    let b = format!("Bulk document beta {n}: the harbor closed in March.");
    let c = format!("Bulk document gamma {n}: the assembly dissolved.");
    let entry = |name: &str, text: &str| dto::BulkManifestEntry {
        filename: name.into(),
        sha256: sha(text),
        bytes_len: text.len() as u64,
        media_type: "text/markdown".into(),
    };
    let file = |name: &str, text: &str| dto::IngestFileRequest {
        filename: name.into(),
        media_type: "text/markdown".into(),
        content_base64: b64(text),
        sha256: None,
        collections: None,
    };

    // CLIENT-side guard: an over-cap chunk never leaves the process.
    let oversized: Vec<dto::IngestFileRequest> = (0..501)
        .map(|i| file(&format!("bulkdocs/f{i}.md"), "x"))
        .collect();
    match loader.ingest.bulk_chunk("blk-any", oversized).await {
        Err(MunariumError::InvalidInput { detail }) => {
            expect!(
                detail.contains("500"),
                "cap error must name the cap: {detail}"
            );
        }
        other => {
            return Err(format!(
                "501-file chunk must be a typed local error, got {other:?}"
            ))
        }
    }

    // Manifest validation server-side: duplicates rejected whole.
    match loader
        .ingest
        .bulk_open(dto::BulkOpenRequest {
            files: vec![entry("bulkdocs/a.md", &a), entry("bulkdocs/a.md", &a)],
            label: None,
        })
        .await
    {
        Err(MunariumError::InvalidInput { .. }) => {}
        other => {
            return Err(format!(
                "duplicate manifest entry must be typed, got {other:?}"
            ))
        }
    }

    // Open: fresh manifest, all three needed.
    let open = loader
        .ingest
        .bulk_open(dto::BulkOpenRequest {
            files: vec![
                entry("bulkdocs/a.md", &a),
                entry("bulkdocs/b.md", &b),
                entry("bulkdocs/c.md", &c),
            ],
            label: Some("client-conformance".into()),
        })
        .await
        .map_err(|e| format!("bulk open: {e}"))?;
    expect!(
        open.total == 3 && open.already_present == 0 && open.needed.len() == 3,
        "fresh open must need all three: {open:?}"
    );

    // Chunk 1: a good; b deliberately corrupt (per-file sha mismatch).
    let chunk1 = loader
        .ingest
        .bulk_chunk(
            &open.bulk_id,
            vec![
                file("bulkdocs/a.md", &a),
                file("bulkdocs/b.md", "corrupted bytes"),
            ],
        )
        .await
        .map_err(|e| format!("chunk 1: {e}"))?;
    expect!(
        nth(&chunk1.results, 0, "chunk 1")?.error.is_none(),
        "a.md must store: {chunk1:?}"
    );
    expect!(
        nth(&chunk1.results, 1, "chunk 1")?
            .error
            .as_deref()
            .is_some_and(|e| e.contains("sha256 mismatch")),
        "corrupt b.md must fail per-file: {chunk1:?}"
    );
    expect!(
        chunk1.stored == 1 && chunk1.failed == 1 && chunk1.pending == 1,
        "chunk 1 counts: {chunk1:?}"
    );

    // Early finalize: incomplete, naming what is owed.
    let early = loader
        .ingest
        .bulk_complete(&open.bulk_id)
        .await
        .map_err(|e| format!("early complete: {e}"))?;
    expect!(
        early.status == "incomplete" && early.missing_count == 2,
        "early complete: {early:?}"
    );

    // Chunk 2 — wholesale replay: a again (idempotent), b fixed, c.
    let chunk2 = loader
        .ingest
        .bulk_chunk(
            &open.bulk_id,
            vec![
                file("bulkdocs/a.md", &a),
                file("bulkdocs/b.md", &b),
                file("bulkdocs/c.md", &c),
            ],
        )
        .await
        .map_err(|e| format!("chunk 2: {e}"))?;
    expect!(
        nth(&chunk2.results, 0, "chunk 2")?.existed,
        "replayed a.md must be an idempotent no-op: {chunk2:?}"
    );
    expect!(
        chunk2.pending == 0 && chunk2.failed == 0,
        "nothing owed after chunk 2: {chunk2:?}"
    );

    // Finalize + status agree; the session's stored count survives replay.
    let complete = loader
        .ingest
        .bulk_complete(&open.bulk_id)
        .await
        .map_err(|e| format!("complete: {e}"))?;
    expect!(complete.status == "completed", "complete: {complete:?}");
    let status = loader
        .ingest
        .bulk_status(&open.bulk_id, true)
        .await
        .map_err(|e| format!("status: {e}"))?;
    expect!(
        status.status == "completed"
            && status.needed.as_ref().is_some_and(|n| n.is_empty())
            && status.stored == 3,
        "status after complete: {status:?}"
    );

    // Zero-byte re-run: same manifest, nothing owed, completes chunkless.
    let rerun = loader
        .ingest
        .bulk_open(dto::BulkOpenRequest {
            files: vec![
                entry("bulkdocs/a.md", &a),
                entry("bulkdocs/b.md", &b),
                entry("bulkdocs/c.md", &c),
            ],
            label: None,
        })
        .await
        .map_err(|e| format!("re-run open: {e}"))?;
    expect!(
        rerun.already_present == 3 && rerun.needed.is_empty(),
        "re-run open must owe nothing: {rerun:?}"
    );
    let rerun_done = loader
        .ingest
        .bulk_complete(&rerun.bulk_id)
        .await
        .map_err(|e| format!("re-run complete: {e}"))?;
    expect!(
        rerun_done.status == "completed",
        "zero-byte re-run completes: {rerun_done:?}"
    );

    // Unknown session: typed NotFound.
    match loader.ingest.bulk_status("blk-doesnotexist", false).await {
        Err(MunariumError::NotFound { .. }) => {}
        other => return Err(format!("unknown session must be NotFound, got {other:?}")),
    }

    // get_source is a CONTROL-plane read: static tokens only — a capability
    // JWT draws the typed 403 (proven live), and the rw static token reads
    // the metadata back.
    let source_id = nth(&chunk2.results, 2, "chunk 2")?
        .source_id
        .clone()
        .ok_or("c.md must carry a source_id")?;
    match loader.ingest.get_source(&source_id).await {
        Err(MunariumError::Forbidden { .. }) => {}
        other => {
            return Err(format!(
                "get_source under a capability JWT must be Forbidden, got {other:?}"
            ))
        }
    }
    let info = ops
        .ingest
        .get_source(&source_id)
        .await
        .map_err(|e| format!("get_source: {e}"))?;
    expect!(
        info.filename == "bulkdocs/c.md" && info.content_hash == sha(&c),
        "source metadata must match what was uploaded: {info:?}"
    );
    Ok(())
}

/// The routes no other scenario touches: /version, the collections trio,
/// chronology rules, and the findings query — so a regression in any of
/// them fails a smoke instead of shipping green.
async fn route_coverage(env: &Env) -> R {
    let ops = &env.ops;

    // GET /version (unauthenticated meta).
    let version = ops
        .server_version()
        .await
        .map_err(|e| format!("server_version: {e}"))?;
    expect!(
        version.name == "munarium-server" && !version.version.is_empty(),
        "version handshake: {version:?}"
    );

    // Collections trio (depends on entdocs@1 from the application scenario).
    let name = format!("cov-{}", nonce());
    let created = ops
        .retrieval
        .create_collection(dto::CreateCollectionRequest {
            name: name.clone(),
            shape_ref: "entdocs@1".into(),
            access_level: 1,
            compartments: vec!["cov".into()],
            description: Some("route-coverage smoke".into()),
        })
        .await
        .map_err(|e| format!("create_collection: {e}"))?;
    expect!(
        created.name == name && created.access_level == 1,
        "created collection echo: {created:?}"
    );
    let listed = ops
        .retrieval
        .list_collections()
        .await
        .map_err(|e| format!("list_collections: {e}"))?;
    expect!(
        listed.collections.iter().any(|c| c.id == created.id),
        "collection must appear in the listing"
    );
    let fetched = ops
        .retrieval
        .get_collection(&created.id)
        .await
        .map_err(|e| format!("get_collection: {e}"))?;
    expect!(
        fetched.compartments == vec!["cov".to_string()]
            && fetched.description.as_deref() == Some("route-coverage smoke"),
        "collection round-trip: {fetched:?}"
    );

    // Chronology rules: apply (upsert) + verbatim readback.
    let rules_yaml = "apiVersion: munarium.ioka.io/v1\nkind: ChronologyRules\nmetadata: { name: cov-rules }\nspec:\n  order:\n    - { before: founding.date, after: dissolution.date }\n";
    let applied = ops
        .runbooks
        .apply_chronology_rules(rules_yaml)
        .await
        .map_err(|e| format!("apply_chronology_rules: {e}"))?;
    expect!(
        applied.name == "cov-rules" && applied.rule_count == 2,
        "chronology apply echo: {applied:?}"
    );
    let readback = ops
        .runbooks
        .get_chronology_rules("cov-rules")
        .await
        .map_err(|e| format!("get_chronology_rules: {e}"))?;
    expect!(
        readback == rules_yaml,
        "chronology rules must read back verbatim"
    );

    // Findings query: empty on a fresh lineage, severity filter accepted,
    // and a bogus severity draws the typed rejection.
    let v = ops
        .commands
        .create_version(Default::default(), None)
        .await
        .map_err(|e| format!("create_version: {e}"))?;
    let findings = ops
        .query
        .findings(
            &v.version_id,
            munarium_client::FindingsQuery {
                severity: Some("block".into()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("findings: {e}"))?;
    expect!(
        findings.findings.is_empty(),
        "fresh lineage must have no findings"
    );
    match ops
        .query
        .findings(
            &v.version_id,
            munarium_client::FindingsQuery {
                severity: Some("bogus".into()),
                ..Default::default()
            },
        )
        .await
    {
        Err(MunariumError::InvalidInput { .. }) => Ok(()),
        other => Err(format!("bogus severity must be typed, got {other:?}")),
    }
}

/// The SSE streaming turn: progress events at real stage boundaries, then
/// exactly one Done that matches the unary shape; a closed session draws
/// the typed session-not-open refusal.
async fn turn_stream_sse(env: &Env) -> R {
    let bob_token = env
        .bob_token
        .as_ref()
        .ok_or("application scenario did not run — no session token")?;
    let bob = env.rest(bob_token, "comp-bob")?;
    let session = bob
        .sessions
        .create("ent-support")
        .await
        .map_err(|e| format!("stream session: {e}"))?;

    let mut stream = bob
        .sessions
        .turn_stream(
            &session.session_id,
            dto::TurnRequest {
                query: "vacation".into(),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("turn_stream open: {e}"))?;

    let mut progress = 0usize;
    let mut done: Option<dto::TurnResponse> = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(TurnStreamEvent::Progress(_)) => {
                expect!(
                    done.is_none(),
                    "no progress may arrive after the terminal done event"
                );
                progress += 1;
            }
            Ok(TurnStreamEvent::Done(t)) => {
                expect!(done.is_none(), "exactly one done event");
                done = Some(*t);
            }
            Err(e) => return Err(format!("stream error: {e}")),
        }
    }
    let done = done.ok_or("stream ended without a done event")?;
    expect!(
        progress >= 1,
        "expected at least one progress event (retrieval/merge), got {progress}"
    );
    expect!(
        !done.hits.is_empty() && done.ordinal >= 1,
        "streamed done must carry the full TurnResponse: {done:?}"
    );

    // Closed session: the refusal is typed session-not-open. It may land
    // either pre-stream (plain problem+json) or as the stream's terminal
    // `error` event (proven live: the SSE response opens, then op_turn's
    // state check fails inside it) — both decode identically, and an
    // errored stream must yield nothing else.
    let closed = bob
        .sessions
        .close(&session.session_id)
        .await
        .map_err(|e| format!("close: {e}"))?;
    expect!(closed.state == "closed", "close must land: {closed:?}");
    match bob
        .sessions
        .turn_stream(
            &session.session_id,
            dto::TurnRequest {
                query: "x".into(),
                ..Default::default()
            },
        )
        .await
    {
        Err(MunariumError::InvalidInput { .. }) => Ok(()),
        Err(e) => Err(format!(
            "turn_stream on a closed session must be the typed session-not-open, got {e}"
        )),
        Ok(mut stream) => {
            let first = stream.next().await;
            match first {
                Some(Err(MunariumError::InvalidInput { .. })) => {
                    expect!(
                        stream.next().await.is_none(),
                        "nothing may ride after the terminal error event"
                    );
                    Ok(())
                }
                other => Err(format!(
                    "closed-session stream must yield the typed session-not-open, got {other:?}"
                )),
            }
        }
    }
}

/// gRPC halves of the platform surface: the AdminService token trio, the
/// SessionService round-trip, the collections trio, and the honest
/// Unsupported set.
async fn grpc_platform(env: &Env) -> R {
    let Some(url) = &env.grpc_url else {
        return Ok(()); // REST-only invocation — nothing to prove here
    };
    let mgr = MunariumClient::grpc(
        MunariumClientOptions::new(url.clone())
            .token(env.mgmt_token.clone())
            .uid("mgr"),
    )
    .await
    .map_err(|e| format!("grpc mgmt connect: {e}"))?;

    // Token trio over AdminService.
    let minted = mgr
        .tokens
        .mint(dto::IssueTokenRequest {
            uid: "grpc-user".into(),
            access_level: 2,
            compartments: vec!["eng".into()],
            scopes: vec!["query".into()],
            runbook_refs: None,
            ttl_secs: None,
        })
        .await
        .map_err(|e| format!("grpc mint: {e}"))?;
    let listed = mgr
        .tokens
        .list(munarium_client::TokenListQuery {
            uid: Some("grpc-user".into()),
            active: Some(true),
        })
        .await
        .map_err(|e| format!("grpc token list: {e}"))?;
    expect!(
        listed.tokens.iter().any(|t| t.jti == minted.jti),
        "grpc-minted token must appear in the audit"
    );

    // Collections trio over RetrievalService (rw static token).
    let rw = MunariumClient::grpc(
        MunariumClientOptions::new(url.clone())
            .token(env.rw_token.clone())
            .uid("ops"),
    )
    .await
    .map_err(|e| format!("grpc rw connect: {e}"))?;
    let name = format!("cov-grpc-{}", nonce());
    let created = rw
        .retrieval
        .create_collection(dto::CreateCollectionRequest {
            name: name.clone(),
            shape_ref: "entdocs@1".into(),
            access_level: 0,
            compartments: vec![],
            description: None,
        })
        .await
        .map_err(|e| format!("grpc create_collection: {e}"))?;
    let fetched = rw
        .retrieval
        .get_collection(&created.id)
        .await
        .map_err(|e| format!("grpc get_collection: {e}"))?;
    expect!(fetched.name == name, "grpc collection round-trip");
    let listed = rw
        .retrieval
        .list_collections()
        .await
        .map_err(|e| format!("grpc list_collections: {e}"))?;
    expect!(
        listed.collections.iter().any(|c| c.id == created.id),
        "grpc collection listing"
    );

    // SessionService round-trip with the minted JWT.
    let user = MunariumClient::grpc(
        MunariumClientOptions::new(url.clone())
            .token(minted.token.clone())
            .uid("grpc-user"),
    )
    .await
    .map_err(|e| format!("grpc user connect: {e}"))?;
    let session = user
        .sessions
        .create("ent-support")
        .await
        .map_err(|e| format!("grpc session: {e}"))?;
    let turn = user
        .sessions
        .turn(
            &session.session_id,
            dto::TurnRequest {
                query: "vacation".into(),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| format!("grpc turn: {e}"))?;
    expect!(
        !turn.hits.is_empty() && !turn.envelopes.is_empty(),
        "grpc turn must carry hits + envelopes: {turn:?}"
    );
    let readback = user
        .sessions
        .get(&session.session_id)
        .await
        .map_err(|e| format!("grpc session readback: {e}"))?;
    expect!(readback.turns.len() == 1, "grpc transcript readback");
    let closed = user
        .sessions
        .close(&session.session_id)
        .await
        .map_err(|e| format!("grpc close: {e}"))?;
    expect!(closed.state == "closed", "grpc close: {closed:?}");

    // The honest Unsupported set.
    match user
        .sessions
        .turn_stream(&session.session_id, Default::default())
        .await
    {
        Err(MunariumError::Unsupported { .. }) => {}
        Err(e) => return Err(format!("grpc turn_stream must be Unsupported, got {e}")),
        Ok(_) => return Err("grpc turn_stream must be Unsupported, got an open stream".into()),
    }
    match mgr.reports.usage(Default::default()).await {
        Err(MunariumError::Unsupported { .. }) => {}
        other => return Err(format!("grpc reports must be Unsupported, got {other:?}")),
    }
    // The S-3.5 additions join the same honest gap rather than quietly
    // becoming the first reports that answer on gRPC.
    match mgr.reports.evidence(Some("24h")).await {
        Err(MunariumError::Unsupported { .. }) => {}
        other => {
            return Err(format!(
                "grpc evidence report must be Unsupported, got {other:?}"
            ))
        }
    }
    match mgr.reports.matrix().await {
        Err(MunariumError::Unsupported { .. }) => {}
        other => {
            return Err(format!(
                "grpc matrix report must be Unsupported, got {other:?}"
            ))
        }
    }
    match mgr.authoring.list_patterns().await {
        Err(MunariumError::Unsupported { .. }) => {}
        other => return Err(format!("grpc authoring must be Unsupported, got {other:?}")),
    }

    // Revoke last so the earlier calls ran under a live token.
    let revoked = mgr
        .tokens
        .revoke(&minted.jti)
        .await
        .map_err(|e| format!("grpc revoke: {e}"))?;
    expect!(revoked.revoked, "grpc revoke must land");
    Ok(())
}

pub async fn run(base: &str, grpc_url: Option<&str>, rw_token: &str, mgmt_token: &str) -> usize {
    println!("munarium-client platform smokes (pg store + MUNARIUM_TOKEN_SECRET required)");
    println!("{}", "-".repeat(56));
    let mut env = match Env::new(base, grpc_url, rw_token, mgmt_token) {
        Ok(env) => env,
        Err(e) => {
            println!("  FAIL  platform.setup\n        {e}");
            return 1;
        }
    };
    let mut r = Report { failed: 0 };
    r.check("platform.uid-contract", uid_contract(&env).await);
    r.check("platform.role-partition", role_partition(&env).await);
    r.check(
        "platform.application-and-compartments",
        application_and_compartments(&mut env).await,
    );
    r.check(
        "platform.removal-double-pass",
        removal_double_pass(&env).await,
    );
    r.check(
        "platform.reports-and-revoke",
        reports_and_revoke(&env).await,
    );
    r.check(
        "platform.authoring-lifecycle",
        authoring_lifecycle(&env).await,
    );
    r.check(
        "platform.bulk-upload-lifecycle",
        bulk_upload_lifecycle(&env).await,
    );
    r.check("platform.route-coverage", route_coverage(&env).await);
    r.check("platform.turn-stream-sse", turn_stream_sse(&env).await);
    r.check("platform.grpc-surface", grpc_platform(&env).await);
    println!("{}", "-".repeat(56));
    println!("platform smokes: {} failed\n", r.failed);
    r.failed
}
