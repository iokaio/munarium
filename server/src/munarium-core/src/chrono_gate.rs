// SPDX-License-Identifier: Apache-2.0
//! Canonical date/interval parsing and algebra for the chronology gate —
//! a faithful port of the research implementation's chronology engine.
//!
//! `parse_when` turns a claim VALUE into a `When` — an interval `[start, end]`
//! with explicit precision (day | month | year) and an `uncertain` flag for
//! hedged dates ("circa 1943", "spring 2021"). Unparseable text returns
//! `None` — the parser never guesses.
//!
//! The load-bearing distinction is `definitely_before`: true only when the
//! comparison is CERTAIN given precision and uncertainty. "circa 1943" is
//! never definitely before or after anything; "2020" vs "2020-05" is
//! undecided. Rules only ever fire on `definitely_*` outcomes, so an
//! intentionally uncertain date is never a violation by itself.
//!
//! Repeat suppression: order/contains/overlap/duration violations file only
//! when at least one contributing event arrives in the CURRENT candidate;
//! deadline-absence checks are clock-driven and re-file while overdue.

use chrono::{Duration, NaiveDate};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

pub const PRECISION_DAY: &str = "day";
pub const PRECISION_MONTH: &str = "month";
pub const PRECISION_YEAR: &str = "year";

/// The temporal-claims convention: claims whose KEY matches one of these
/// patterns are chronology-relevant by default (an asset may override).
pub const DEFAULT_TEMPORAL_KEY_PATTERNS: [&str; 6] =
    ["*_date", "*_on", "date_*", "*_deadline", "*_due", "*_when"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct When {
    pub start: NaiveDate,
    pub end: NaiveDate,
    /// "day" | "month" | "year"
    pub precision: &'static str,
    pub uncertain: bool,
}

impl When {
    pub fn overlaps(&self, other: &When) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    /// True only when this is certainly earlier: neither side uncertain and
    /// the intervals disjoint in order.
    pub fn definitely_before(&self, other: &When) -> bool {
        !self.uncertain && !other.uncertain && self.end < other.start
    }

    pub fn definitely_after(&self, other: &When) -> bool {
        other.definitely_before(self)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "start": self.start.to_string(),
            "end": self.end.to_string(),
            "precision": self.precision,
            "uncertain": self.uncertain,
        })
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn month_num(name: &str) -> Option<u32> {
    Some(match name.to_lowercase().as_str() {
        "january" | "jan" => 1,
        "february" | "feb" => 2,
        "march" | "mar" => 3,
        "april" | "apr" => 4,
        "may" => 5,
        "june" | "jun" => 6,
        "july" | "jul" => 7,
        "august" | "aug" => 8,
        "september" | "sep" | "sept" => 9,
        "october" | "oct" => 10,
        "november" | "nov" => 11,
        "december" | "dec" => 12,
        _ => return None,
    })
}

fn month_end(year: i32, month: u32) -> NaiveDate {
    if month == 12 {
        NaiveDate::from_ymd_opt(year, 12, 31).unwrap()
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1).unwrap() - Duration::days(1)
    }
}

const MONTH_RE: &str = "january|february|march|april|august|september|october|november|december|june|july|sept|jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec";
const RANGE_SEP: &str = r"\s*(?:-|–|—|to)\s*";

struct Parsers {
    circa: Regex,
    iso_day: Regex,
    iso_month: Regex,
    year: Regex,
    year_range: Regex,
    month_year: Regex,
    month_range: Regex,
    month_day_year: Regex,
    day_month_year: Regex,
    season: Regex,
}

fn parsers() -> &'static Parsers {
    static P: OnceLock<Parsers> = OnceLock::new();
    P.get_or_init(|| Parsers {
        circa: Regex::new(r"(?i)^(?:circa|ca\.?|c\.|~|approx(?:\.|imately)?|around|about)\s*")
            .unwrap(),
        iso_day: Regex::new(r"^(\d{4})-(\d{2})-(\d{2})$").unwrap(),
        iso_month: Regex::new(r"^(\d{4})-(\d{2})$").unwrap(),
        year: Regex::new(r"^(\d{4})$").unwrap(),
        year_range: Regex::new(&format!(r"^(\d{{4}}){RANGE_SEP}(\d{{4}})$")).unwrap(),
        month_year: Regex::new(&format!(r"(?i)^({MONTH_RE})\.?\s+(\d{{4}})$")).unwrap(),
        month_range: Regex::new(&format!(
            r"(?i)^({MONTH_RE})\.?{RANGE_SEP}({MONTH_RE})\.?\s+(\d{{4}})$"
        ))
        .unwrap(),
        month_day_year: Regex::new(&format!(
            r"(?i)^({MONTH_RE})\.?\s+(\d{{1,2}})(?:st|nd|rd|th)?(?:,\s*|\s+)(\d{{4}})$"
        ))
        .unwrap(),
        day_month_year: Regex::new(&format!(
            r"(?i)^(\d{{1,2}})(?:st|nd|rd|th)?\s+({MONTH_RE})\.?,?\s+(\d{{4}})$"
        ))
        .unwrap(),
        season: Regex::new(r"(?i)^(spring|summer|autumn|fall|winter)\s+(\d{4})$").unwrap(),
    })
}

/// Parse one calendar assertion; `None` for anything the grammar does not
/// cover. Supported: ISO dates, `YYYY-MM`, `YYYY`, "Month YYYY",
/// "Month D, YYYY", "D Month YYYY", year and month ranges, seasons
/// (uncertain), and circa/approx/~ markers (uncertain).
pub fn parse_when(text: &str) -> Option<When> {
    let p = parsers();
    let mut cleaned = text.trim().to_string();
    if cleaned.is_empty() {
        return None;
    }
    let mut uncertain = false;
    let hedged = p.circa.replace(&cleaned, "").to_string();
    if hedged != cleaned {
        uncertain = true;
        cleaned = hedged.trim().to_string();
        if cleaned.is_empty() {
            return None;
        }
    }

    if let Some(m) = p.iso_day.captures(&cleaned) {
        let day =
            NaiveDate::from_ymd_opt(m[1].parse().ok()?, m[2].parse().ok()?, m[3].parse().ok()?)?;
        return Some(When {
            start: day,
            end: day,
            precision: PRECISION_DAY,
            uncertain,
        });
    }

    if let Some(m) = p.iso_month.captures(&cleaned) {
        let (year, month): (i32, u32) = (m[1].parse().ok()?, m[2].parse().ok()?);
        if !(1..=12).contains(&month) {
            return None;
        }
        return Some(When {
            start: NaiveDate::from_ymd_opt(year, month, 1)?,
            end: month_end(year, month),
            precision: PRECISION_MONTH,
            uncertain,
        });
    }

    if let Some(m) = p.year_range.captures(&cleaned) {
        let (y1, y2): (i32, i32) = (m[1].parse().ok()?, m[2].parse().ok()?);
        if y1 > y2 {
            return None;
        }
        return Some(When {
            start: NaiveDate::from_ymd_opt(y1, 1, 1)?,
            end: NaiveDate::from_ymd_opt(y2, 12, 31)?,
            precision: PRECISION_YEAR,
            uncertain,
        });
    }

    if let Some(m) = p.year.captures(&cleaned) {
        let year: i32 = m[1].parse().ok()?;
        return Some(When {
            start: NaiveDate::from_ymd_opt(year, 1, 1)?,
            end: NaiveDate::from_ymd_opt(year, 12, 31)?,
            precision: PRECISION_YEAR,
            uncertain,
        });
    }

    if let Some(m) = p.month_range.captures(&cleaned) {
        let (m1, m2) = (month_num(&m[1])?, month_num(&m[2])?);
        let year: i32 = m[3].parse().ok()?;
        if m1 > m2 {
            return None;
        }
        return Some(When {
            start: NaiveDate::from_ymd_opt(year, m1, 1)?,
            end: month_end(year, m2),
            precision: PRECISION_MONTH,
            uncertain,
        });
    }

    if let Some(m) = p.month_day_year.captures(&cleaned) {
        let day =
            NaiveDate::from_ymd_opt(m[3].parse().ok()?, month_num(&m[1])?, m[2].parse().ok()?)?;
        return Some(When {
            start: day,
            end: day,
            precision: PRECISION_DAY,
            uncertain,
        });
    }

    if let Some(m) = p.day_month_year.captures(&cleaned) {
        let day =
            NaiveDate::from_ymd_opt(m[3].parse().ok()?, month_num(&m[2])?, m[1].parse().ok()?)?;
        return Some(When {
            start: day,
            end: day,
            precision: PRECISION_DAY,
            uncertain,
        });
    }

    if let Some(m) = p.month_year.captures(&cleaned) {
        let (month, year): (u32, i32) = (month_num(&m[1])?, m[2].parse().ok()?);
        return Some(When {
            start: NaiveDate::from_ymd_opt(year, month, 1)?,
            end: month_end(year, month),
            precision: PRECISION_MONTH,
            uncertain,
        });
    }

    if let Some(m) = p.season.captures(&cleaned) {
        let (start_month, end_month) = match m[1].to_lowercase().as_str() {
            "spring" => (3u32, 5u32),
            "summer" => (6, 8),
            "autumn" | "fall" => (9, 11),
            "winter" => (12, 2),
            _ => return None,
        };
        let year: i32 = m[2].parse().ok()?;
        let end_year = if end_month < start_month {
            year + 1
        } else {
            year
        };
        // A season is inherently hedged: uncertain regardless of a circa marker.
        return Some(When {
            start: NaiveDate::from_ymd_opt(year, start_month, 1)?,
            end: month_end(end_year, end_month),
            precision: PRECISION_MONTH,
            uncertain: true,
        });
    }

    None
}

// ---------------------------------------------------------------------------
// Declarative rules
// ---------------------------------------------------------------------------

fn default_severity() -> String {
    "warn".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRule {
    pub before: String,
    pub after: String,
    #[serde(default = "default_severity")]
    pub severity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainsRule {
    pub outer: String,
    pub inner: String,
    #[serde(default = "default_severity")]
    pub severity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlapRule {
    pub a: String,
    pub b: String,
    #[serde(default = "default_severity")]
    pub severity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadlineRule {
    pub key: String,
    pub due_from: String,
    pub within_days: i64,
    #[serde(default = "default_severity")]
    pub severity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurationRule {
    /// YAML spells these `from:` / `to:`.
    #[serde(rename = "from")]
    pub start: String,
    #[serde(rename = "to")]
    pub end: String,
    #[serde(default)]
    pub min_days: Option<i64>,
    #[serde(default)]
    pub max_days: Option<i64>,
    #[serde(default = "default_severity")]
    pub severity: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChronologyRules {
    #[serde(default)]
    pub order: Vec<OrderRule>,
    #[serde(default)]
    pub contains: Vec<ContainsRule>,
    #[serde(default)]
    pub forbid_overlap: Vec<OverlapRule>,
    #[serde(default)]
    pub deadlines: Vec<DeadlineRule>,
    #[serde(default)]
    pub durations: Vec<DurationRule>,
    #[serde(default)]
    pub temporal_keys: Vec<String>,
}

impl ChronologyRules {
    pub fn all_targets(&self) -> Vec<&str> {
        let mut t: Vec<&str> = Vec::new();
        for r in &self.order {
            t.push(&r.before);
            t.push(&r.after);
        }
        for r in &self.contains {
            t.push(&r.outer);
            t.push(&r.inner);
        }
        for r in &self.forbid_overlap {
            t.push(&r.a);
            t.push(&r.b);
        }
        for r in &self.deadlines {
            t.push(&r.key);
            t.push(&r.due_from);
        }
        for r in &self.durations {
            t.push(&r.start);
            t.push(&r.end);
        }
        t
    }

    pub fn absolute_targets(&self) -> Vec<String> {
        self.all_targets()
            .into_iter()
            .filter(|t| is_absolute_target(t))
            .map(|t| t.to_lowercase())
            .collect()
    }

    pub fn is_temporal_key(&self, key: &str) -> bool {
        let patterns: Vec<&str> = if self.temporal_keys.is_empty() {
            DEFAULT_TEMPORAL_KEY_PATTERNS.to_vec()
        } else {
            self.temporal_keys.iter().map(String::as_str).collect()
        };
        patterns.iter().any(|p| key_pattern_matches(p, key))
    }
}

/// An absolute target names one exact `subject.key`; anything else is a key
/// pattern matched against the claim KEY alone.
pub fn is_absolute_target(target: &str) -> bool {
    target.contains('.') && !target.contains('*')
}

/// `*`-wildcard fullmatch over the claim key, case-insensitive — the port of
/// the original `.*`-joined escaped-part regex.
fn key_pattern_matches(pattern: &str, key: &str) -> bool {
    let key = key.to_lowercase();
    let pattern = pattern.to_lowercase();
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !key.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else {
            match key[pos..].find(part) {
                Some(found) => pos = pos + found + part.len(),
                None => return false,
            }
        }
    }
    // fullmatch: a non-empty trailing literal must reach the end
    if let Some(last) = parts.last() {
        if !last.is_empty() && !key.ends_with(last) {
            return false;
        }
        if !last.is_empty() && pattern.split('*').count() == 1 {
            return key == pattern;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Events and evaluation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ChronoEvent {
    pub subject: String,
    pub key: String,
    pub value: String,
    pub when: When,
    /// Set for accepted snapshot facts; None for candidate claims.
    pub claim_id: Option<String>,
    /// "ledger" | "candidate"
    pub origin: &'static str,
}

impl ChronoEvent {
    pub fn claim_key(&self) -> String {
        format!("{}.{}", self.subject, self.key)
    }

    fn to_detail(&self, role: &str) -> serde_json::Value {
        serde_json::json!({
            "role": role,
            "claim_id": self.claim_id,
            "claim": self.claim_key(),
            "value": self.value,
            "when": self.when.to_json(),
            "origin": self.origin,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ChronoViolation {
    /// order | contains | overlap | deadline | duration
    pub kind: &'static str,
    pub severity: String,
    pub message: String,
    pub rule: serde_json::Value,
    /// Complete role-tagged event chain.
    pub chain: Vec<serde_json::Value>,
    pub extras: serde_json::Map<String, serde_json::Value>,
    /// claim_keys of candidate-origin contributors (for disputing).
    pub candidate_claims: Vec<String>,
}

fn matches(target: &str, event: &ChronoEvent) -> bool {
    if is_absolute_target(target) {
        event.claim_key().to_lowercase() == target.to_lowercase()
    } else {
        key_pattern_matches(target, &event.key)
    }
}

fn pairs<'a>(
    events: &'a [ChronoEvent],
    target_a: &str,
    target_b: &str,
) -> Vec<(&'a ChronoEvent, &'a ChronoEvent)> {
    let global_pairing = is_absolute_target(target_a) || is_absolute_target(target_b);
    let mut out = Vec::new();
    for ea in events {
        if !matches(target_a, ea) {
            continue;
        }
        for eb in events {
            if eb.claim_key() == ea.claim_key() || !matches(target_b, eb) {
                continue;
            }
            if !global_pairing && ea.subject != eb.subject {
                continue;
            }
            out.push((ea, eb));
        }
    }
    out
}

fn involves_candidate(events: &[&ChronoEvent]) -> bool {
    events.iter().any(|e| e.origin == "candidate")
}

fn candidate_claims_of(events: &[&ChronoEvent]) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.origin == "candidate")
        .map(|e| e.claim_key())
        .collect()
}

/// Evaluate every declared rule over the event timeline. Pure and
/// deterministic; `now` is the deadline-absence clock (None disables).
pub fn evaluate(
    events: &[ChronoEvent],
    rules: &ChronologyRules,
    now: Option<NaiveDate>,
) -> Vec<ChronoViolation> {
    let mut events: Vec<ChronoEvent> = events.to_vec();
    events.sort_by(|a, b| {
        (a.subject.clone(), a.key.clone()).cmp(&(b.subject.clone(), b.key.clone()))
    });
    let mut violations = Vec::new();

    for rule in &rules.order {
        for (before_ev, after_ev) in pairs(&events, &rule.before, &rule.after) {
            if !involves_candidate(&[before_ev, after_ev]) {
                continue;
            }
            if after_ev.when.definitely_before(&before_ev.when) {
                violations.push(ChronoViolation {
                    kind: "order",
                    severity: rule.severity.clone(),
                    message: format!(
                        "chronology: '{}={}' must come after '{}={}' but is definitely before it",
                        after_ev.claim_key(), after_ev.value,
                        before_ev.claim_key(), before_ev.value
                    ),
                    rule: serde_json::json!({"order": {"before": rule.before, "after": rule.after}}),
                    chain: vec![before_ev.to_detail("before"), after_ev.to_detail("after")],
                    extras: Default::default(),
                    candidate_claims: candidate_claims_of(&[before_ev, after_ev]),
                });
            }
        }
    }

    for rule in &rules.contains {
        for (outer_ev, inner_ev) in pairs(&events, &rule.outer, &rule.inner) {
            if !involves_candidate(&[outer_ev, inner_ev]) {
                continue;
            }
            let certain = !outer_ev.when.uncertain && !inner_ev.when.uncertain;
            if certain && !inner_ev.when.overlaps(&outer_ev.when) {
                violations.push(ChronoViolation {
                    kind: "contains",
                    severity: rule.severity.clone(),
                    message: format!(
                        "chronology: '{}={}' must fall within '{}={}' but is definitely outside it",
                        inner_ev.claim_key(), inner_ev.value,
                        outer_ev.claim_key(), outer_ev.value
                    ),
                    rule: serde_json::json!({"contains": {"outer": rule.outer, "inner": rule.inner}}),
                    chain: vec![outer_ev.to_detail("outer"), inner_ev.to_detail("inner")],
                    extras: Default::default(),
                    candidate_claims: candidate_claims_of(&[outer_ev, inner_ev]),
                });
            }
        }
    }

    for rule in &rules.forbid_overlap {
        let mut seen: Vec<(String, String)> = Vec::new();
        for (a_ev, b_ev) in pairs(&events, &rule.a, &rule.b) {
            let mut pair_id = [a_ev.claim_key(), b_ev.claim_key()];
            pair_id.sort();
            let pair_id = (pair_id[0].clone(), pair_id[1].clone());
            if seen.contains(&pair_id) {
                continue; // symmetric duplicate when both patterns match both
            }
            seen.push(pair_id);
            if !involves_candidate(&[a_ev, b_ev]) {
                continue;
            }
            let definite = !a_ev.when.uncertain
                && !b_ev.when.uncertain
                && a_ev.when.precision == PRECISION_DAY
                && b_ev.when.precision == PRECISION_DAY;
            if definite && a_ev.when.overlaps(&b_ev.when) {
                violations.push(ChronoViolation {
                    kind: "overlap",
                    severity: rule.severity.clone(),
                    message: format!(
                        "chronology: '{}={}' and '{}={}' must not overlap but definitely do",
                        a_ev.claim_key(),
                        a_ev.value,
                        b_ev.claim_key(),
                        b_ev.value
                    ),
                    rule: serde_json::json!({"forbid_overlap": {"a": rule.a, "b": rule.b}}),
                    chain: vec![a_ev.to_detail("a"), b_ev.to_detail("b")],
                    extras: Default::default(),
                    candidate_claims: candidate_claims_of(&[a_ev, b_ev]),
                });
            }
        }
    }

    for rule in &rules.deadlines {
        let rule_doc = serde_json::json!({"deadline": {
            "key": rule.key, "due_from": rule.due_from, "within_days": rule.within_days}});
        let global_pairing = is_absolute_target(&rule.key) || is_absolute_target(&rule.due_from);
        for from_ev in &events {
            if !matches(&rule.due_from, from_ev) || from_ev.when.uncertain {
                continue;
            }
            // Checked arithmetic: within_days comes from tenant-supplied rule
            // YAML; an out-of-range value must not abort the request thread
            // (Duration::days and NaiveDate + Duration both panic on
            // overflow). A rule whose deadline cannot be represented can
            // never produce a CERTAIN violation — file nothing.
            let Some(deadline) = Duration::try_days(rule.within_days)
                .and_then(|d| from_ev.when.end.checked_add_signed(d))
            else {
                continue;
            };
            let key_events: Vec<&ChronoEvent> = events
                .iter()
                .filter(|e| {
                    e.claim_key() != from_ev.claim_key()
                        && matches(&rule.key, e)
                        && (global_pairing || e.subject == from_ev.subject)
                })
                .collect();
            if !key_events.is_empty() {
                for key_ev in key_events {
                    if !involves_candidate(&[from_ev, key_ev]) {
                        continue;
                    }
                    if !key_ev.when.uncertain && key_ev.when.start > deadline {
                        let mut extras = serde_json::Map::new();
                        extras.insert("deadline".into(), deadline.to_string().into());
                        violations.push(ChronoViolation {
                            kind: "deadline",
                            severity: rule.severity.clone(),
                            message: format!(
                                "chronology: '{}={}' is definitely later than the deadline {} ({} days after '{}={}')",
                                key_ev.claim_key(), key_ev.value, deadline, rule.within_days,
                                from_ev.claim_key(), from_ev.value
                            ),
                            rule: rule_doc.clone(),
                            chain: vec![from_ev.to_detail("due_from"), key_ev.to_detail("event")],
                            extras,
                            candidate_claims: candidate_claims_of(&[from_ev, key_ev]),
                        });
                    }
                }
            } else if let Some(clock) = now {
                if clock > deadline {
                    // Clock-driven state check: re-files while overdue and unmet.
                    let mut extras = serde_json::Map::new();
                    extras.insert("deadline".into(), deadline.to_string().into());
                    extras.insert("now".into(), clock.to_string().into());
                    violations.push(ChronoViolation {
                        kind: "deadline",
                        severity: rule.severity.clone(),
                        message: format!(
                            "chronology: no '{}' event within {} days of '{}={}' — deadline {} passed as of {}",
                            rule.key, rule.within_days, from_ev.claim_key(), from_ev.value,
                            deadline, clock
                        ),
                        rule: rule_doc.clone(),
                        chain: vec![from_ev.to_detail("due_from")],
                        extras,
                        candidate_claims: candidate_claims_of(&[from_ev]),
                    });
                }
            }
        }
    }

    for rule in &rules.durations {
        let rule_doc = serde_json::json!({"duration": {
            "from": rule.start, "to": rule.end,
            "min_days": rule.min_days, "max_days": rule.max_days}});
        for (from_ev, to_ev) in pairs(&events, &rule.start, &rule.end) {
            if !involves_candidate(&[from_ev, to_ev]) {
                continue;
            }
            if from_ev.when.uncertain || to_ev.when.uncertain {
                continue;
            }
            // The possible elapsed-days range given both intervals.
            let gap_min = (to_ev.when.start - from_ev.when.end).num_days();
            let gap_max = (to_ev.when.end - from_ev.when.start).num_days();
            let problem = if rule.max_days.map(|m| gap_min > m).unwrap_or(false) {
                Some(format!(
                    "definitely exceeds max_days={}",
                    rule.max_days.unwrap()
                ))
            } else if rule.min_days.map(|m| gap_max < m).unwrap_or(false) {
                Some(format!(
                    "definitely under min_days={}",
                    rule.min_days.unwrap()
                ))
            } else {
                None
            };
            if let Some(problem) = problem {
                let mut extras = serde_json::Map::new();
                extras.insert("gap_days_min".into(), gap_min.into());
                extras.insert("gap_days_max".into(), gap_max.into());
                violations.push(ChronoViolation {
                    kind: "duration",
                    severity: rule.severity.clone(),
                    message: format!(
                        "chronology: elapsed days from '{}={}' to '{}={}' (between {gap_min} and {gap_max}) {problem}",
                        from_ev.claim_key(), from_ev.value, to_ev.claim_key(), to_ev.value
                    ),
                    rule: rule_doc.clone(),
                    chain: vec![from_ev.to_detail("from"), to_ev.to_detail("to")],
                    extras,
                    candidate_claims: candidate_claims_of(&[from_ev, to_ev]),
                });
            }
        }
    }

    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn parses_the_grammar() {
        let w = parse_when("2020-05-17").unwrap();
        assert_eq!(
            (w.start, w.end, w.precision, w.uncertain),
            (d("2020-05-17"), d("2020-05-17"), "day", false)
        );

        let w = parse_when("2020-05").unwrap();
        assert_eq!(
            (w.start, w.end, w.precision),
            (d("2020-05-01"), d("2020-05-31"), "month")
        );

        let w = parse_when("1995").unwrap();
        assert_eq!(
            (w.start, w.end, w.precision),
            (d("1995-01-01"), d("1995-12-31"), "year")
        );

        let w = parse_when("1990-1995").unwrap();
        assert_eq!((w.start, w.end), (d("1990-01-01"), d("1995-12-31")));

        let w = parse_when("March-June 2020").unwrap();
        assert_eq!((w.start, w.end), (d("2020-03-01"), d("2020-06-30")));

        let w = parse_when("March 5, 2020").unwrap();
        assert_eq!(w.start, d("2020-03-05"));
        let w = parse_when("5th March 2020").unwrap();
        assert_eq!(w.start, d("2020-03-05"));
        let w = parse_when("Sept 2020").unwrap();
        assert_eq!((w.start, w.end), (d("2020-09-01"), d("2020-09-30")));
    }

    #[test]
    fn hedges_and_seasons_are_uncertain() {
        assert!(parse_when("circa 1943").unwrap().uncertain);
        assert!(parse_when("~March 2020").unwrap().uncertain);
        assert!(parse_when("approx. 1950").unwrap().uncertain);
        let w = parse_when("spring 2021").unwrap();
        assert!(w.uncertain);
        assert_eq!((w.start, w.end), (d("2021-03-01"), d("2021-05-31")));
        // winter spans the year boundary
        let w = parse_when("winter 2020").unwrap();
        assert_eq!((w.start, w.end), (d("2020-12-01"), d("2021-02-28")));
    }

    #[test]
    fn never_guesses() {
        assert!(parse_when("next tuesday").is_none());
        assert!(parse_when("2020-13").is_none());
        assert!(parse_when("1995-1990").is_none());
        assert!(parse_when("").is_none());
        assert!(parse_when("February 30, 2020").is_none());
    }

    #[test]
    fn certainty_semantics() {
        let a = parse_when("1944").unwrap();
        let circa = parse_when("circa 1943").unwrap();
        // uncertainty disables certainty entirely
        assert!(!circa.definitely_before(&a));
        assert!(!a.definitely_after(&circa));
        // coarse-vs-fine that could be consistent is undecided
        let year = parse_when("2020").unwrap();
        let month = parse_when("2020-05").unwrap();
        assert!(!year.definitely_before(&month));
        assert!(!month.definitely_before(&year));
        // genuinely disjoint certain intervals decide
        let w1 = parse_when("1990").unwrap();
        let w2 = parse_when("1995").unwrap();
        assert!(w1.definitely_before(&w2));
    }

    fn ev(subject: &str, key: &str, value: &str, origin: &'static str) -> ChronoEvent {
        ChronoEvent {
            subject: subject.into(),
            key: key.into(),
            value: value.into(),
            when: parse_when(value).unwrap(),
            claim_id: if origin == "ledger" {
                Some(format!("c-{subject}-{key}"))
            } else {
                None
            },
            origin,
        }
    }

    #[test]
    fn order_rule_fires_only_on_certain_violation_with_candidate() {
        let rules = ChronologyRules {
            order: vec![OrderRule {
                before: "birth_date".into(),
                after: "death_date".into(),
                severity: "warn".into(),
            }],
            ..Default::default()
        };
        // per-subject pairing: violation on hero, not across subjects
        let events = vec![
            ev("hero", "birth_date", "1950", "ledger"),
            ev("hero", "death_date", "1940", "candidate"),
            ev("villain", "death_date", "1930", "ledger"),
        ];
        let v = evaluate(&events, &rules, None);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, "order");
        assert_eq!(v[0].candidate_claims, vec!["hero.death_date"]);

        // circa hedging files nothing
        let events = vec![
            ev("hero", "birth_date", "circa 1950", "ledger"),
            ev("hero", "death_date", "1940", "candidate"),
        ];
        assert!(evaluate(&events, &rules, None).is_empty());

        // repeat suppression: both events already in the ledger => nothing
        let events = vec![
            ev("hero", "birth_date", "1950", "ledger"),
            ev("hero", "death_date", "1940", "ledger"),
        ];
        assert!(evaluate(&events, &rules, None).is_empty());
    }

    #[test]
    fn deadline_rule_late_event_and_absence() {
        let rules = ChronologyRules {
            deadlines: vec![DeadlineRule {
                key: "response_date".into(),
                due_from: "filing_date".into(),
                within_days: 30,
                severity: "warn".into(),
            }],
            ..Default::default()
        };
        // late event
        let events = vec![
            ev("case", "filing_date", "2020-01-01", "ledger"),
            ev("case", "response_date", "2020-03-15", "candidate"),
        ];
        let v = evaluate(&events, &rules, None);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, "deadline");

        // absence: fires only with a clock past the deadline
        let events = vec![ev("case", "filing_date", "2020-01-01", "ledger")];
        assert!(
            evaluate(&events, &rules, None).is_empty(),
            "no clock = no absence check"
        );
        let v = evaluate(&events, &rules, Some(d("2020-06-01")));
        assert_eq!(v.len(), 1);

        // an event inside the window clears it
        let events = vec![
            ev("case", "filing_date", "2020-01-01", "ledger"),
            ev("case", "response_date", "2020-01-20", "candidate"),
        ];
        assert!(evaluate(&events, &rules, Some(d("2020-06-01"))).is_empty());
    }

    #[test]
    fn duration_rule_uses_interval_arithmetic() {
        let rules = ChronologyRules {
            durations: vec![DurationRule {
                start: "start_date".into(),
                end: "end_date".into(),
                min_days: Some(1),
                max_days: Some(90),
                severity: "warn".into(),
            }],
            ..Default::default()
        };
        // certainly over max
        let events = vec![
            ev("p", "start_date", "2020-01-01", "ledger"),
            ev("p", "end_date", "2021-06-01", "candidate"),
        ];
        assert_eq!(evaluate(&events, &rules, None).len(), 1);
        // year-precision intervals that COULD satisfy the band file nothing
        let events = vec![
            ev("p", "start_date", "2020", "ledger"),
            ev("p", "end_date", "2020", "candidate"),
        ];
        assert!(evaluate(&events, &rules, None).is_empty());
    }

    #[test]
    fn key_patterns_match_like_the_lab() {
        assert!(key_pattern_matches("*_date", "birth_date"));
        assert!(key_pattern_matches("*_date", "BIRTH_DATE"));
        assert!(!key_pattern_matches("*_date", "date_of_birth"));
        assert!(key_pattern_matches("date_*", "date_of_birth"));
        assert!(key_pattern_matches(
            "release-date::*",
            "release-date::4-2-1"
        ));
        assert!(is_absolute_target("case.filing_date"));
        assert!(!is_absolute_target("*_date"));
    }
}
