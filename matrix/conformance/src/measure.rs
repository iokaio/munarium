// SPDX-License-Identifier: Apache-2.0
//! The measurement harness.
//!
//! Two halves, both measured rather than composed:
//!
//! 1. **N executes** of one contract over REST, reading the `Server-Timing`
//!    header each answer carries (`total`, `source`, `seal`, `matrix`), and
//!    the client's own wall clock around the call. Percentiles of each.
//! 2. **N turns** through a munarium-server, each through a research profile
//!    with ONE required Matrix layer and no completion (`complete: false` —
//!    the test server holds no provider key, and a model's latency is
//!    not what this measures). The turn's wall clock is the client's; the
//!    layer's `elapsed_ms` is the server's view of the whole Matrix call;
//!    and the execute that call triggered is read back from Matrix's journal
//!    with its `source_ms` and `seal_ms`. The plan's formula is then applied
//!    per turn, exactly as written in §18.3:
//!
//!    ```text
//!    transport + serialization = execute wall (the server's view)
//!                              − source statement time
//!                              − canonicalize + seal time
//!    transport share           = p95(transport + serialization) / p95(turn)
//!    ```
//!
//!    The §5.1 trigger was "≥ 5% of p95 turn latency with one Matrix layer".
//!    The gRPC plane was built on the owner's decision before this number
//!    existed; the harness exists so the decision has a measurement beside
//!    it rather than under it.
//!
//! Every number is written to `MUNARIUM_MATRIX_MEASURE_OUT` as JSON, which
//! a live run folds into its results file — the only
//! place a number quoted in the documents may come from. Unset, the harness
//! prints that it SKIPPED. Set, anything missing is a panic, not a skip: a
//! harness that measures nothing and prints `ok` is the failure this harness was
//! written against.
//!
//! Pairing a turn with its execute uses the newest `execute` journal row
//! after each turn. That is sound only because the harness is the sole
//! caller while it runs — which the live runner guarantees by running it
//! before the parallel conformance tier — and it is asserted, not assumed:
//! the row must be newer than the previous turn's, or the pairing is wrong
//! and the harness says so.

#[cfg(test)]
mod harness {
    use std::time::Instant;

    fn env(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.trim().is_empty())
    }

    fn rest_url() -> String {
        env("MUNARIUM_MATRIX_TEST_URL").unwrap_or_else(|| "http://localhost:8180".into())
    }

    fn token() -> String {
        env("MUNARIUM_MATRIX_TEST_TOKEN").unwrap_or_else(|| "mxdev".into())
    }

    fn mgmt_token() -> String {
        env("MUNARIUM_MATRIX_TEST_MGMT_TOKEN").unwrap_or_else(|| "mxmgmt".into())
    }

    fn tenant() -> String {
        env("MUNARIUM_MATRIX_TEST_TENANT").unwrap_or_else(|| "tenant-default".into())
    }

    /// Nearest-rank percentile over a sample — the same rule for every number
    /// here, so p50 and p95 of two series are comparable.
    fn percentile(samples: &[u64], p: f64) -> u64 {
        assert!(!samples.is_empty(), "a percentile of nothing");
        let mut s = samples.to_vec();
        s.sort_unstable();
        let rank = ((p * s.len() as f64).ceil() as usize).clamp(1, s.len());
        s[rank - 1]
    }

    fn summary(samples: &[u64]) -> serde_json::Value {
        serde_json::json!({
            "n": samples.len(),
            "p50": percentile(samples, 0.50),
            "p95": percentile(samples, 0.95),
            "min": samples.iter().min().copied().unwrap_or(0),
            "max": samples.iter().max().copied().unwrap_or(0),
        })
    }

    /// `Server-Timing: total;dur=48, source;dur=11, seal;dur=29, matrix;dur=8`
    /// → the four numbers. A missing or malformed header is a failure, not a
    /// zero: a zero would read as "Matrix took no time".
    fn parse_server_timing(header: &str) -> ServerTiming {
        let mut st = ServerTiming::default();
        let mut seen = 0;
        for part in header.split(',') {
            let part = part.trim();
            let Some((name, rest)) = part.split_once(';') else {
                continue;
            };
            let dur = rest
                .split(';')
                .find_map(|kv| kv.trim().strip_prefix("dur="))
                .and_then(|v| v.trim().parse::<f64>().ok())
                .unwrap_or_else(|| panic!("Server-Timing entry without dur: {part:?}"))
                .round() as u64;
            match name.trim() {
                "total" => st.total_ms = dur,
                "source" => st.source_ms = dur,
                "seal" => st.seal_ms = dur,
                "matrix" => st.matrix_ms = dur,
                _ => continue,
            }
            seen += 1;
        }
        assert_eq!(
            seen, 4,
            "Server-Timing carries all four entries: {header:?}"
        );
        st
    }

    #[derive(Debug, Default, Clone, Copy)]
    struct ServerTiming {
        total_ms: u64,
        source_ms: u64,
        seal_ms: u64,
        matrix_ms: u64,
    }

    fn execute_intent(contract: &str, as_of: &str) -> serde_json::Value {
        serde_json::json!({
            "contract_version": munarium_matrix_core::CONTRACT_VERSION,
            "kind": "structured_query",
            "contract": contract,
            "parameters": { "as_of": { "type": "date", "value": as_of } },
            "authorization": { "tenant": tenant(), "access_level": 0, "compartments": [] },
            "limits": { "max_rows": 500, "max_bytes": 1048576 }
        })
    }

    /// The newest `execute` row in Matrix's journal — the execute a turn just
    /// triggered, when the harness is the only caller.
    async fn newest_execute(http: &reqwest::Client) -> serde_json::Value {
        let r = http
            .get(format!("{}/v1/journal?kind=execute&limit=1", rest_url()))
            .bearer_auth(mgmt_token())
            .send()
            .await
            .expect("the journal answers");
        assert!(
            r.status().is_success(),
            "journal read: {} — is MUNARIUM_MATRIX_TEST_MGMT_TOKEN the mgmt token?",
            r.status()
        );
        let body: serde_json::Value = r.json().await.expect("journal is JSON");
        body["entries"]
            .as_array()
            .and_then(|e| e.first().cloned())
            .expect("the turn's execute landed a journal row")
    }

    #[tokio::test]
    #[ignore = "needs MUNARIUM_MATRIX_MEASURE_OUT (and a Matrix to measure)"]
    async fn measure_transport_share_with_one_matrix_layer() {
        let Some(out_path) = env("MUNARIUM_MATRIX_MEASURE_OUT") else {
            eprintln!(
                "SKIPPED: MUNARIUM_MATRIX_MEASURE_OUT is unset — the §18.3 harness did not run"
            );
            return;
        };
        let n: usize = env("MUNARIUM_MATRIX_MEASURE_N")
            .and_then(|v| v.parse().ok())
            .unwrap_or(20);
        assert!(n >= 20, "§18.3 asks for at least twenty iterations; N={n}");
        let contract = env("MUNARIUM_MATRIX_MEASURE_CONTRACT")
            .unwrap_or_else(|| "open-pipeline-by-region".into());
        let as_of = env("MUNARIUM_MATRIX_MEASURE_AS_OF").unwrap_or_else(|| "2026-06-30".into());
        let http = reqwest::Client::new();

        // ---- 1. N executes, straight at Matrix ------------------------------
        let mut wall = Vec::with_capacity(n);
        let mut total = Vec::with_capacity(n);
        let mut source = Vec::with_capacity(n);
        let mut seal = Vec::with_capacity(n);
        let mut matrix = Vec::with_capacity(n);
        for i in 0..n {
            let started = Instant::now();
            let r = http
                .post(format!("{}/v1/contracts/{contract}/execute", rest_url()))
                .bearer_auth(token())
                .json(&execute_intent(&contract, &as_of))
                .send()
                .await
                .expect("execute reaches the REST plane");
            let elapsed = started.elapsed().as_millis() as u64;
            let status = r.status();
            let timing = r
                .headers()
                .get("server-timing")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            assert!(status.is_success(), "execute #{i}: {status}: {body}");
            // A refusal is a 200 with a Refusal block. It is an ANSWER, but it
            // is not an execution, and timing twenty refusals would measure
            // nothing about the source or the seal.
            assert_eq!(
                body["kind"].as_str(),
                Some("complete_table"),
                "execute #{i} did not produce a table: {body}"
            );
            let st = parse_server_timing(
                &timing.unwrap_or_else(|| panic!("execute #{i} carried no Server-Timing header")),
            );
            wall.push(elapsed);
            total.push(st.total_ms);
            source.push(st.source_ms);
            seal.push(st.seal_ms);
            matrix.push(st.matrix_ms);
        }
        let mut report = serde_json::json!({
            "harness": "measure.transport_share_with_one_matrix_layer",
            "n": n,
            "contract": contract,
            "matrix_url": rest_url(),
            "execute": {
                "client_wall_ms": summary(&wall),
                "total_ms": summary(&total),
                "source_ms": summary(&source),
                "seal_ms": summary(&seal),
                "matrix_own_ms": summary(&matrix),
            },
        });
        eprintln!(
            "execute ×{n}: wall p95 {} ms, total p95 {} ms, source p95 {} ms, seal p95 {} ms, matrix p95 {} ms",
            percentile(&wall, 0.95),
            percentile(&total, 0.95),
            percentile(&source, 0.95),
            percentile(&seal, 0.95),
            percentile(&matrix, 0.95),
        );

        // ---- 2. N turns through a server, one required Matrix layer -------
        if let Some(server_url) = env("MUNARIUM_MATRIX_MEASURE_SERVER_URL") {
            let server_token = env("MUNARIUM_MATRIX_MEASURE_SERVER_TOKEN")
                .expect("MUNARIUM_MATRIX_MEASURE_SERVER_TOKEN beside the server URL");
            let runbook = env("MUNARIUM_MATRIX_MEASURE_SERVER_RUNBOOK")
                .unwrap_or_else(|| "measure-matrix".into());
            let profile =
                env("MUNARIUM_MATRIX_MEASURE_SERVER_PROFILE").unwrap_or_else(|| "measure".into());
            let uid = env("MUNARIUM_MATRIX_MEASURE_SERVER_UID").unwrap_or_else(|| "measure".into());
            let question = env("MUNARIUM_MATRIX_MEASURE_QUESTION")
                .unwrap_or_else(|| "What is the open pipeline by region?".into());
            let server_url = server_url.trim_end_matches('/').to_string();

            let session = http
                .post(format!("{server_url}/v1/runbooks/{runbook}/sessions"))
                .bearer_auth(&server_token)
                .header("X-Munarium-Uid", &uid)
                .header("Idempotency-Key", format!("measure-{}", uuid_ish()))
                .send()
                .await
                .expect("the server answers");
            let status = session.status();
            let session: serde_json::Value = session.json().await.unwrap_or_default();
            assert!(status.is_success(), "open session: {status}: {session}");
            let session_id = session["session_id"]
                .as_str()
                .expect("a session id")
                .to_string();

            let mut turn_wall = Vec::with_capacity(n);
            let mut layer_elapsed = Vec::with_capacity(n);
            let mut transport = Vec::with_capacity(n);
            let mut exec_total = Vec::with_capacity(n);
            let mut last_row_id: Option<String> = None;
            let mut layer_name = String::new();
            for i in 0..n {
                let started = Instant::now();
                let r = http
                    .post(format!("{server_url}/v1/sessions/{session_id}/turns"))
                    .bearer_auth(&server_token)
                    .header("X-Munarium-Uid", &uid)
                    .header(
                        "Idempotency-Key",
                        format!("measure-turn-{i}-{}", uuid_ish()),
                    )
                    .json(&serde_json::json!({
                        "query": question,
                        "research_profile": profile,
                        "complete": false,
                    }))
                    .send()
                    .await
                    .expect("the turn reaches the server");
                let elapsed = started.elapsed().as_millis() as u64;
                let status = r.status();
                let body: serde_json::Value = r.json().await.unwrap_or_default();
                assert!(status.is_success(), "turn #{i}: {status}: {body}");
                let layers = body["hierarchy"]["layers"]
                    .as_array()
                    .unwrap_or_else(|| panic!("turn #{i} ran no hierarchy: {body}"));
                // The ONE Matrix layer: the one that produced a table. A layer
                // that refused measured nothing, and says so by name.
                let layer = layers
                    .iter()
                    .find(|l| l["block"].as_str() == Some("complete_table"))
                    .unwrap_or_else(|| {
                        panic!(
                            "turn #{i}: no layer produced a complete table — layers: {}",
                            serde_json::to_string(layers).unwrap_or_default()
                        )
                    });
                layer_name = layer["layer"].as_str().unwrap_or("").to_string();
                let elapsed_layer = layer["elapsed_ms"].as_u64().expect("layer elapsed_ms");

                // The execute this turn triggered, from Matrix's own journal.
                let row = newest_execute(&http).await;
                let row_id = row["id"].as_str().unwrap_or("").to_string();
                assert_ne!(
                    Some(&row_id),
                    last_row_id.as_ref(),
                    "turn #{i}: the journal's newest execute is the previous turn's — the turn \
                     did not reach Matrix, or someone else is executing while this measures"
                );
                last_row_id = Some(row_id);
                let src = row["source_ms"]
                    .as_u64()
                    .expect("the execute row carries source_ms");
                let sl = row["seal_ms"]
                    .as_u64()
                    .expect("the execute row carries seal_ms");
                let tot = row["duration_ms"].as_u64().expect("duration_ms");

                turn_wall.push(elapsed);
                layer_elapsed.push(elapsed_layer);
                exec_total.push(tot);
                // §18.3, per turn: the server's view of the call, less the two
                // pieces that are not transport.
                transport.push(elapsed_layer.saturating_sub(src + sl));
            }
            let p95_turn = percentile(&turn_wall, 0.95);
            let p95_transport = percentile(&transport, 0.95);
            let share = if p95_turn == 0 {
                0.0
            } else {
                p95_transport as f64 / p95_turn as f64
            };
            eprintln!(
                "turn ×{n} via '{profile}' layer '{layer_name}': turn p95 {p95_turn} ms, layer p95 {} ms, \
                 transport+serialization p95 {p95_transport} ms → share {:.3}",
                percentile(&layer_elapsed, 0.95),
                share
            );
            report["turn"] = serde_json::json!({
                "server_url": server_url,
                "runbook": runbook,
                "profile": profile,
                "layer": layer_name,
                "complete": false,
                "turn_wall_ms": summary(&turn_wall),
                "layer_elapsed_ms": summary(&layer_elapsed),
                "execute_total_ms_per_matrix_journal": summary(&exec_total),
                "transport_serialization_ms": summary(&transport),
                "transport_share_of_p95_turn": (share * 1000.0).round() / 1000.0,
                "section_5_1_trigger_5pct": share >= 0.05,
                "formula": "p95(layer.elapsed_ms − journal.source_ms − journal.seal_ms) / p95(turn wall)",
            });
        } else {
            eprintln!(
                "turn half SKIPPED: MUNARIUM_MATRIX_MEASURE_SERVER_URL unset — executes measured, the transport share was not"
            );
            report["turn"] = serde_json::Value::Null;
        }

        std::fs::write(
            &out_path,
            serde_json::to_string_pretty(&report).expect("report serializes"),
        )
        .unwrap_or_else(|e| panic!("could not write {out_path}: {e}"));
        eprintln!("wrote {out_path}");
    }

    /// Enough uniqueness for an idempotency key in a single-process harness.
    fn uuid_ish() -> String {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{t:x}-{:x}", std::process::id())
    }

    #[test]
    fn server_timing_parses_all_four_entries() {
        let st = parse_server_timing("total;dur=48, source;dur=11.4, seal;dur=29, matrix;dur=8");
        assert_eq!(
            (st.total_ms, st.source_ms, st.seal_ms, st.matrix_ms),
            (48, 11, 29, 8)
        );
    }

    #[test]
    fn nearest_rank_percentiles() {
        let s: Vec<u64> = (1..=20).collect();
        assert_eq!(percentile(&s, 0.50), 10);
        assert_eq!(percentile(&s, 0.95), 19);
        assert_eq!(percentile(&[7], 0.95), 7);
    }
}
