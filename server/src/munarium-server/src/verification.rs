// SPDX-License-Identifier: Apache-2.0
//! Deterministic turn-loop verification — the server port of the measured
//! grounding checks (dev-guide §13 entry 10). Both
//! checks are pure string work over data the turn already holds:
//!
//! - **quotes**: double-quoted spans in the answer must resolve VERBATIM
//!   (whitespace-normalized) somewhere in the served hit text. A model that
//!   "quotes" text nobody served is fabricating.
//! - **citations**: bracketed labels in the answer must name content that
//!   was actually served this turn. The context block serves hits under
//!   `[collection/chunk_id]` labels, so those labels (plus source paths)
//!   ARE the citation vocabulary — no new convention is invented.
//!
//! On violations the turn loop grants ONE corrective completion (the measured
//! `conformance_retry` shape) with the violations and the original context
//! attached. Honest delta from the measured mechanism: its fetch-on-cite SERVES a
//! cited-but-unfetched document on the retry; the server cannot fetch by
//! bare name without another retrieval round, so the corrective prompt
//! re-attaches the full served context and instructs the model to cite
//! only what is there. Extending the retry with a targeted fetch is the
//! documented next step if measurements demand it.

/// Quoted spans shorter than this are ignored — short quotes ("yes", a
/// name) collide with ordinary prose and are not grounding claims. The
/// original checks used the same idea: only substantial spans must resolve.
const MIN_QUOTE_CHARS: usize = 15;

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract double-quoted spans (straight or curly quotes) from an answer.
fn quoted_spans(text: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut current: Option<String> = None;
    for c in text.chars() {
        match c {
            '"' | '\u{201C}' | '\u{201D}' => match current.take() {
                Some(span) => spans.push(span),
                None => current = Some(String::new()),
            },
            _ => {
                if let Some(span) = current.as_mut() {
                    span.push(c);
                }
            }
        }
    }
    spans
        .into_iter()
        .filter(|s| s.trim().len() >= MIN_QUOTE_CHARS)
        .collect()
}

/// `verify_quotes`: every substantial quoted span must appear
/// verbatim (whitespace-normalized) in some served text. Returns the
/// unresolved spans.
pub fn check_quotes(answer: &str, served_texts: &[&str]) -> Vec<String> {
    let haystacks: Vec<String> = served_texts.iter().map(|t| normalize_ws(t)).collect();
    quoted_spans(answer)
        .into_iter()
        .filter(|span| {
            let needle = normalize_ws(span);
            !haystacks.iter().any(|h| h.contains(&needle))
        })
        .collect()
}

/// `verify_citations`, in the server's own vocabulary: every
/// bracketed label containing '/' must be a served `collection/chunk_id`
/// label or a served source path. Returns the unserved citations. Bracketed
/// tokens WITHOUT a '/' (e.g. "[sic]", "[1]") are not citations here and
/// pass — the check never guesses.
pub fn check_citations(answer: &str, served_labels: &[&str]) -> Vec<String> {
    let mut violations = Vec::new();
    let mut rest = answer;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        let token = after[..close].trim();
        if token.contains('/')
            && !token.is_empty()
            && !token.contains('\n')
            && !served_labels.contains(&token)
        {
            violations.push(token.to_string());
        }
        rest = &after[close + 1..];
    }
    // `dedup` removes ADJACENT duplicates only; the corrective prompt should
    // name each violation once whatever order the model cited them in.
    violations.sort();
    violations.dedup();
    violations
}

/// The corrective instruction for the one retry: violations first (the
/// model must know exactly what failed), then the rules, then the original
/// prompt with its full served context re-attached.
pub fn corrective_prompt(
    original_prompt: &str,
    previous_answer: &str,
    quote_violations: &[String],
    citation_violations: &[String],
) -> String {
    let mut out = String::from(
        "Your previous answer failed deterministic verification and must be revised.\n",
    );
    if !quote_violations.is_empty() {
        out.push_str("\nThese quoted passages do NOT appear in the provided context — quote only text that appears verbatim, or remove the quotation marks and paraphrase:\n");
        for q in quote_violations {
            out.push_str(&format!("  - \"{q}\"\n"));
        }
    }
    if !citation_violations.is_empty() {
        out.push_str("\nThese citations name content that was NOT provided — cite only the bracketed labels present in the context, or state that the information is not in the provided documents:\n");
        for c in citation_violations {
            out.push_str(&format!("  - [{c}]\n"));
        }
    }
    out.push_str("\nYour previous answer:\n");
    out.push_str(previous_answer);
    out.push_str("\n\n--- Original task, with the provided context ---\n");
    out.push_str(original_prompt);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_resolve_whitespace_normalized_and_short_spans_pass() {
        let served = ["The  harbor bell\nrang twice at dawn, said the keeper."];
        // Resolves despite whitespace/newline differences in the served text.
        assert!(check_quotes(
            r#"The log notes "The harbor bell rang twice at dawn" clearly."#,
            &served
        )
        .is_empty());
        // A fabricated substantial quote is a violation.
        let v = check_quotes(
            r#"It says "the lighthouse was painted crimson that spring" too."#,
            &served,
        );
        assert_eq!(v.len(), 1, "{v:?}");
        // Short quotes never gate.
        assert!(check_quotes(r#"He said "yes" firmly."#, &served).is_empty());
    }

    #[test]
    fn citations_must_name_served_labels_and_plain_brackets_pass() {
        let served = ["docs/chunk-01", "kb/faq-9", "cl/one.txt"];
        assert!(check_citations("See [docs/chunk-01] and [kb/faq-9].", &served).is_empty());
        let v = check_citations("Per [docs/chunk-99] the cap is 60%.", &served);
        assert_eq!(v, vec!["docs/chunk-99".to_string()]);
        // Non-citation brackets are not the check's business.
        assert!(check_citations("The report [sic] cites [1] and [2].", &served).is_empty());
    }

    #[test]
    fn corrective_prompt_carries_violations_answer_and_context() {
        let p = corrective_prompt(
            "Context: [a/b] text\n\nQ: what?",
            "Previous answer.",
            &["ghost quote".into()],
            &["a/zz".into()],
        );
        assert!(p.contains("ghost quote"));
        assert!(p.contains("[a/zz]"));
        assert!(p.contains("Previous answer."));
        assert!(p.contains("Q: what?"));
    }
}

// ---------------------------------------------------------------------------
// Typed assertions over sealed evidence
// ---------------------------------------------------------------------------

/// One numeric or categorical claim an answer makes, bound to the evidence it
/// came from.
///
/// The problem this solves: a model given a sealed table will happily write
/// "revenue grew 12%" without saying which rows it subtracted, and no
/// deterministic check can tell a correct derivation from an invented one. An
/// assertion makes the derivation *stateable*, and therefore checkable.
///
/// `unit` is separate from `value` on purpose. "900000.50" and "900000.50 EUR"
/// are different facts, and folding the unit into the value string would make
/// the exact-decimal comparison below meaningless.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TypedAssertion {
    /// Canonical text, never a JSON number: a decimal(38,2) does not survive
    /// an IEEE-754 double, and exactness is why the structured plane exists.
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// `count` | `sum` | `value` | `ratio` | `category` — free text; the
    /// checks below do not branch on it, it is for the reader.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// How the value was reached, when it was not read off a single row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation_ref: Option<String>,
    /// `evidence/<id>#<row_id>` refs this value rests on.
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

/// What was actually served from one sealed artifact.
pub struct ServedEvidence {
    pub evidence_id: String,
    /// Row id, and the row's cells as canonical text.
    pub rows: Vec<(String, Vec<String>)>,
}

impl ServedEvidence {
    fn row(&self, row_id: &str) -> Option<&Vec<String>> {
        self.rows
            .iter()
            .find(|(id, _)| id == row_id)
            .map(|(_, cells)| cells)
    }
}

/// Parse `evidence/<id>#<row_id>` into its parts.
pub fn parse_evidence_ref(token: &str) -> Option<(&str, &str)> {
    let rest = token.strip_prefix("evidence/")?;
    let (id, row) = rest.split_once('#')?;
    (!id.is_empty() && !row.is_empty()).then_some((id, row))
}

/// Every `[evidence/<id>#<row_id>]` in the answer that names a row the turn
/// did not serve.
///
/// Kept separate from [`check_citations`] rather than folded into it: a
/// document citation names a chunk that was served, an evidence citation names
/// a ROW inside a sealed artifact, and the corrective messages a model needs
/// to fix them are different. One check reporting both would have to describe
/// both, badly.
pub fn check_evidence_citations(answer: &str, served: &[ServedEvidence]) -> Vec<String> {
    let mut violations = Vec::new();
    let mut rest = answer;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else { break };
        let token = after[..close].trim();
        if let Some((id, row_id)) = parse_evidence_ref(token) {
            let served_row = served
                .iter()
                .any(|e| e.evidence_id == id && e.row(row_id).is_some());
            if !served_row {
                violations.push(token.to_string());
            }
        }
        rest = &after[close + 1..];
    }
    // `dedup` removes ADJACENT duplicates only; the corrective prompt should
    // name each violation once whatever order the model cited them in.
    violations.sort();
    violations.dedup();
    violations
}

/// The fence an assertions block is carried in.
const ASSERTIONS_FENCE: &str = "```assertions";

/// Extract the assertions block, when the answer carries one.
///
/// Its own fence, so it never collides with prose or with a JSON answer the
/// runbook already expects.
///
/// `Ok(empty)` when the answer carries no block; `Err` when it carries one
/// that does not parse. The two used to collapse into "no assertions", which
/// made the answer most likely to be wrong — the model TRIED to state its
/// derivations and produced junk — indistinguishable from one that made no
/// claims, so the corrective retry never fired on it.
pub fn extract_assertions(answer: &str) -> Result<Vec<TypedAssertion>, String> {
    let Some(start) = answer.find(ASSERTIONS_FENCE) else {
        return Ok(Vec::new());
    };
    let body = &answer[start + ASSERTIONS_FENCE.len()..];
    let Some(end) = body.find("```") else {
        return Err("assertions block is not closed".into());
    };
    serde_json::from_str(body[..end].trim()).map_err(|e| format!("block does not parse: {e}"))
}

/// Check every assertion against what was actually served.
///
/// Two rules, both deliberately narrow:
///
/// 1. Each `evidence_refs` entry must name a served row.
/// 2. A SINGLE-ref assertion's `value` must appear verbatim in that row.
///
/// Rule 2 stops at one reference on purpose. With two or more refs the value
/// is a derivation — a sum, a difference, a ratio — and it is *supposed* not
/// to appear in any single row. Demanding that it did would fail every correct
/// aggregate, which is worse than not checking at all: a check that fires on
/// correct work teaches people to switch it off. Verifying the arithmetic
/// itself needs derivation semantics, and those are not built.
pub fn check_assertions(assertions: &[TypedAssertion], served: &[ServedEvidence]) -> Vec<String> {
    let mut violations = Vec::new();
    for a in assertions {
        if a.evidence_refs.is_empty() {
            violations.push(format!("assertion '{}' cites no evidence", a.value));
            continue;
        }
        let mut resolved = Vec::new();
        for r in &a.evidence_refs {
            match parse_evidence_ref(r) {
                Some((id, row_id)) => {
                    let cells = served
                        .iter()
                        .find(|e| e.evidence_id == id)
                        .and_then(|e| e.row(row_id));
                    match cells {
                        Some(cells) => resolved.push(cells),
                        None => violations.push(format!(
                            "assertion '{}' cites {r}, which was not served",
                            a.value
                        )),
                    }
                }
                None => violations.push(format!(
                    "assertion '{}' cites '{r}', which is not an evidence/<id>#<row> reference",
                    a.value
                )),
            }
        }
        if a.evidence_refs.len() == 1 && resolved.len() == 1 {
            let wanted = a.value.trim();
            if !resolved[0].iter().any(|c| c.trim() == wanted) {
                violations.push(format!(
                    "assertion '{}' does not appear in the single row it cites ({})",
                    a.value, a.evidence_refs[0]
                ));
            }
        }
    }
    violations
}

#[cfg(test)]
mod s34_tests {
    use super::*;

    fn served() -> Vec<ServedEvidence> {
        vec![ServedEvidence {
            evidence_id: "ev-1".into(),
            rows: vec![
                ("r0001".into(), vec!["north".into(), "900000.50".into()]),
                ("r0002".into(), vec!["south".into(), "3050001.50".into()]),
            ],
        }]
    }

    #[test]
    fn an_evidence_ref_parses_into_artifact_and_row() {
        assert_eq!(
            parse_evidence_ref("evidence/ev-1#r0001"),
            Some(("ev-1", "r0001"))
        );
        assert_eq!(parse_evidence_ref("evidence/ev-1"), None, "no row");
        assert_eq!(
            parse_evidence_ref("contracts/chunk-9"),
            None,
            "not evidence"
        );
    }

    #[test]
    fn a_citation_to_an_unserved_row_is_a_violation() {
        let v = check_evidence_citations("north was 900000.50 [evidence/ev-1#r0001]", &served());
        assert!(v.is_empty(), "{v:?}");

        let v = check_evidence_citations("also [evidence/ev-1#r0099]", &served());
        assert_eq!(v, vec!["evidence/ev-1#r0099"], "a row nobody served");

        let v = check_evidence_citations("also [evidence/ev-9#r0001]", &served());
        assert_eq!(v, vec!["evidence/ev-9#r0001"], "an artifact nobody served");
    }

    #[test]
    fn a_document_citation_is_not_an_evidence_citation() {
        // The two checks stay separate; this one must not claim a document
        // citation as its own and then report it under the wrong message.
        let v = check_evidence_citations("see [contracts/chunk-9] and [sic]", &served());
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn an_assertion_is_extracted_from_its_fenced_block_and_verifies() {
        let answer = concat!(
            "Revenue in the north was 900000.50.\n\n",
            "```assertions\n",
            r#"[{"value":"900000.50","unit":"EUR","type":"value","#,
            r#""evidence_refs":["evidence/ev-1#r0001"]}]"#,
            "\n```\n"
        );
        let a = extract_assertions(answer).expect("a well-formed block parses");
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].value, "900000.50");
        assert_eq!(a[0].unit.as_deref(), Some("EUR"));
        assert_eq!(a[0].kind.as_deref(), Some("value"));
        assert!(check_assertions(&a, &served()).is_empty());
    }

    #[test]
    fn a_single_ref_assertion_whose_value_is_not_in_the_row_fails() {
        // 900000.5 and 900000.50 are the SAME number and different sealed
        // values. Comparing as text is what keeps them distinguishable.
        let a = vec![TypedAssertion {
            value: "900000.5".into(),
            unit: None,
            kind: None,
            derivation_ref: None,
            evidence_refs: vec!["evidence/ev-1#r0001".into()],
        }];
        let v = check_assertions(&a, &served());
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].contains("does not appear"), "{v:?}");
    }

    #[test]
    fn a_multi_ref_derivation_is_not_required_to_appear_in_any_row() {
        // A sum is SUPPOSED not to be in any single row. A check that fired
        // here would fail every correct aggregate.
        let a = vec![TypedAssertion {
            value: "3950002.00".into(),
            unit: Some("EUR".into()),
            kind: Some("sum".into()),
            derivation_ref: Some("sum(amount)".into()),
            evidence_refs: vec!["evidence/ev-1#r0001".into(), "evidence/ev-1#r0002".into()],
        }];
        assert!(check_assertions(&a, &served()).is_empty());
    }

    #[test]
    fn an_assertion_citing_nothing_is_a_violation() {
        let a = vec![TypedAssertion {
            value: "12".into(),
            unit: None,
            kind: None,
            derivation_ref: None,
            evidence_refs: vec![],
        }];
        let v = check_assertions(&a, &served());
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("cites no evidence"), "{v:?}");
    }

    #[test]
    fn an_answer_with_no_assertions_block_yields_none_and_passes() {
        assert!(extract_assertions("just prose").unwrap().is_empty());
        assert!(check_assertions(&[], &served()).is_empty());
    }

    /// A block the model emitted but got wrong is a violation, not "no
    /// assertions": it is the answer most likely to be wrong.
    #[test]
    fn a_malformed_assertions_block_is_an_error_not_an_absence() {
        let broken = "text\n```assertions\n[{\"value\": 1,}]\n```";
        let err = extract_assertions(broken).unwrap_err();
        assert!(err.contains("does not parse"), "{err}");
        let unclosed = "text\n```assertions\n[]";
        assert!(extract_assertions(unclosed).is_err());
    }
}
