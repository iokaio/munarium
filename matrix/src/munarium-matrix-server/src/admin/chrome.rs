// SPDX-License-Identifier: Apache-2.0
//! Page chrome: the stylesheet, escaping, tables, badges and inline SVG.
//!
//! This is a **copy** of the pattern the server's `charts.rs` established, not
//! an import of it. Ground rule 1 says `matrix/` never depends on a `server/`
//! crate, and a shared UI crate would be exactly that dependency wearing a
//! different hat. The cost is two stylesheets that can drift; the price of the
//! alternative is a build-graph edge that CI exists to refuse. Where the two
//! consoles sit side by side in a browser they should *look* alike, and the
//! palette below is the same one, so drift shows up as a visual difference an
//! operator would notice rather than as a silent behavioural one.
//!
//! Everything here is server-rendered. There is no JavaScript on any page —
//! not "progressive enhancement that degrades", none — so the CSP can be
//! `default-src 'self'` with no script source at all, and every page works in
//! a browser with scripting off because there is nothing to turn off.

/// The whole stylesheet, inlined. No external asset means no CDN, no cache
/// story, and no second request that could fail while the page renders.
pub const STYLE: &str = r#"<style>
:root {
  color-scheme: light;
  --page: #f9f9f7; --surface: #fcfcfb;
  --ink: #0b0b0b; --ink-2: #52514e; --muted: #898781;
  --grid: #e1e0d9; --baseline: #c3c2b7; --ring: rgba(11,11,11,0.10);
  --ok: #2f6f4f; --warn: #8a6d1f; --bad: #8c3b2f; --idle: #6a6a6a;
  --accent: #33566e;
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--page); color: var(--ink);
  font: 14px/1.5 ui-sans-serif, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif; }
nav { display: flex; flex-wrap: wrap; gap: .75rem; align-items: baseline;
  padding: .6rem 1rem; background: var(--surface); border-bottom: 1px solid var(--grid); }
nav .brand { font-weight: 600; margin-right: .5rem; }
nav a { color: var(--ink-2); text-decoration: none; padding: .1rem .3rem; border-radius: 3px; }
nav a:hover { background: var(--page); color: var(--ink); }
nav a.active { color: var(--ink); box-shadow: inset 0 -2px 0 var(--accent); }
nav .badge { background: var(--warn); color: #fff; font-size: 11px;
  padding: .05rem .35rem; border-radius: 3px; margin-left: .4rem; }
main { padding: 1rem 1.25rem 3rem; max-width: 1100px; }
h1 { font-size: 20px; margin: .2rem 0 1rem; }
h2 { font-size: 15px; margin: 1.6rem 0 .5rem; color: var(--ink-2);
  text-transform: lowercase; letter-spacing: .02em; }
a { color: var(--accent); }
table { border-collapse: collapse; width: 100%; margin: .4rem 0 1rem; background: var(--surface); }
table.kv td:first-child { color: var(--ink-2); width: 15rem; }
th, td { text-align: left; padding: .35rem .5rem; border-bottom: 1px solid var(--grid);
  vertical-align: top; font-variant-numeric: tabular-nums; }
th { color: var(--ink-2); font-weight: 600; font-size: 12px; text-transform: lowercase; }
tr:last-child td { border-bottom: none; }
pre { background: var(--surface); border: 1px solid var(--grid); border-radius: 4px;
  padding: .6rem .7rem; overflow-x: auto; font-size: 12.5px; margin: .4rem 0 1rem; }
code { font-size: 12.5px; }
.notice { background: var(--surface); border: 1px solid var(--grid); border-left: 3px solid var(--warn);
  padding: .5rem .7rem; border-radius: 3px; margin: .5rem 0 1rem; color: var(--ink-2); }
.empty { color: var(--muted); font-style: italic; padding: .3rem 0 1rem; }
.swatch { display: inline-block; width: .6rem; height: .6rem; border-radius: 2px;
  margin-right: .3rem; vertical-align: baseline; }
.legend { color: var(--ink-2); font-size: 12px; margin: -.2rem 0 .8rem; }
form.act { display: inline-flex; gap: .35rem; align-items: center; margin: 0; }
form.login { max-width: 22rem; margin: 4rem auto; background: var(--surface);
  border: 1px solid var(--grid); border-radius: 5px; padding: 1.2rem 1.4rem 1.5rem; }
form.login h1 { margin-top: 0; }
label { display: block; color: var(--ink-2); font-size: 12px; margin: .6rem 0 .2rem; }
input, select, textarea { font: inherit; padding: .3rem .4rem; border: 1px solid var(--baseline);
  border-radius: 3px; background: #fff; color: var(--ink); width: 100%; }
textarea { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12.5px; }
button { font: inherit; padding: .3rem .7rem; border: 1px solid var(--baseline);
  border-radius: 3px; background: var(--surface); color: var(--ink); cursor: pointer; }
button:hover { background: #fff; }
button.danger { border-color: var(--bad); color: var(--bad); }
.note { color: var(--muted); font-size: 12px; font-style: italic; }
.bars { display: block; margin: .3rem 0 1rem; }
.diff ins { background: #e3f0e6; text-decoration: none; display: block; }
.diff del { background: #f6e2de; text-decoration: none; display: block; }
.diff span { display: block; }
</style>"#;

/// HTML-escape. Every user-derived string on every page goes through this;
/// the helpers below take *trusted* HTML precisely so the escaping decision is
/// made at the point the value is known, not guessed at the point it is drawn.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The reserved colour for a state word. One vocabulary, so `failed` is the
/// same red on the queue page and the journal page — an operator learns the
/// colour once.
pub fn state_color(state: &str) -> &'static str {
    match state {
        "ok" | "succeeded" | "complete" | "ready" | "enabled" | "promoted" | "closed" => "#2f6f4f",
        "running" | "queued" | "pending" | "partial" | "shadow" => "#8a6d1f",
        "failed" | "refused" | "denied" | "unreachable" | "drifted" | "exhausted" => "#8c3b2f",
        _ => "#6a6a6a",
    }
}

pub fn state_badge(state: &str) -> String {
    format!(
        r#"<span class="swatch" style="background:{}"></span>{}"#,
        state_color(state),
        esc(state)
    )
}

pub fn link(href: &str, text: &str) -> String {
    format!(r#"<a href="{}">{}</a>"#, esc(href), esc(text))
}

pub fn opt(s: Option<&str>) -> String {
    s.map(esc).unwrap_or_else(|| "—".into())
}

pub fn short(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

pub fn pre(text: &str) -> String {
    format!("<pre>{}</pre>", esc(text))
}

/// A two-column definition table. Values are **trusted HTML** — the caller
/// escapes, or passes a link.
pub fn kv(rows: &[(&str, String)]) -> String {
    let mut out = String::from(r#"<table class="kv">"#);
    for (k, v) in rows {
        out.push_str(&format!("<tr><td>{}</td><td>{v}</td></tr>", esc(k)));
    }
    out.push_str("</table>");
    out
}

/// A data table whose cells are **trusted HTML**. An empty table renders as
/// the word "none" rather than as a header with nothing under it: a bare
/// header row reads as a rendering failure, and an operator should be able to
/// tell "nothing here" from "something broke".
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return r#"<div class="empty">none</div>"#.into();
    }
    let mut out = String::from("<table><tr>");
    for h in headers {
        out.push_str(&format!("<th>{}</th>", esc(h)));
    }
    out.push_str("</tr>");
    for row in rows {
        out.push_str("<tr>");
        for cell in row {
            out.push_str(&format!("<td>{cell}</td>"));
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
    out
}

/// The two colours the bar chart draws with. Hoisted out of the format
/// strings because `#33566e` inside one is parsed as a float exponent long
/// before anyone gets to the SVG.
const INK_2: &str = "#52514e";
const BAR: &str = "#33566e";

/// A horizontal bar chart as inline SVG — no library, no script, and it
/// prints. Labels are drawn as text beside the bar rather than in a legend,
/// because a legend forces a reader to hold a colour in mind while looking
/// somewhere else.
pub fn bars(rows: &[(String, f64)], unit: &str) -> String {
    if rows.is_empty() {
        return r#"<div class="empty">none</div>"#.into();
    }
    let max = rows.iter().map(|(_, v)| *v).fold(0.0_f64, f64::max);
    let row_h = 20.0;
    let label_w = 200.0;
    let bar_w = 520.0;
    let h = row_h * rows.len() as f64 + 8.0;
    let mut svg = format!(
        r#"<svg class="bars" width="{}" height="{h}" viewBox="0 0 {} {h}" role="img">"#,
        label_w + bar_w + 90.0,
        label_w + bar_w + 90.0
    );
    for (i, (label, value)) in rows.iter().enumerate() {
        let y = i as f64 * row_h + 4.0;
        // A zero-valued row still draws its label and a hairline, so "this
        // source did nothing" is visible rather than absent.
        let w = if max > 0.0 { value / max * bar_w } else { 0.0 };
        svg.push_str(&format!(
            r#"<text x="0" y="{}" font-size="12" fill="{INK_2}">{}</text>"#,
            y + 12.0,
            esc(&short(label, 34))
        ));
        svg.push_str(&format!(
            r#"<rect x="{label_w}" y="{}" width="{:.1}" height="12" fill="{BAR}" opacity="0.85"/>"#,
            y + 3.0,
            w.max(0.5)
        ));
        svg.push_str(&format!(
            r#"<text x="{:.1}" y="{}" font-size="12" fill="{INK_2}">{}{}</text>"#,
            label_w + w.max(0.5) + 6.0,
            y + 12.0,
            fmt_num(*value),
            esc(unit)
        ));
    }
    svg.push_str("</svg>");
    svg
}

fn fmt_num(v: f64) -> String {
    if (v - v.round()).abs() < f64::EPSILON {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.2}")
    }
}

/// A line-by-line diff of two texts, rendered as coloured blocks.
///
/// A **longest-common-subsequence** diff, not a naive line-by-line
/// comparison: an asset edit that inserts one line at the top would otherwise
/// mark every following line changed, and a diff that says "everything moved"
/// is the diff an operator stops reading. Kept small deliberately — assets are
/// tens of lines, and an O(n·m) table over that is nothing.
pub fn diff(before: &str, after: &str) -> String {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut out = String::from(r#"<pre class="diff">"#);
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            out.push_str(&format!("<span>  {}</span>", esc(a[i])));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push_str(&format!("<del>- {}</del>", esc(a[i])));
            i += 1;
        } else {
            out.push_str(&format!("<ins>+ {}</ins>", esc(b[j])));
            j += 1;
        }
    }
    while i < n {
        out.push_str(&format!("<del>- {}</del>", esc(a[i])));
        i += 1;
    }
    while j < m {
        out.push_str(&format!("<ins>+ {}</ins>", esc(b[j])));
        j += 1;
    }
    out.push_str("</pre>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_covers_the_attribute_and_element_contexts() {
        assert_eq!(
            esc(r#"<img src=x onerror="alert('1')">"#),
            "&lt;img src=x onerror=&quot;alert(&#39;1&#39;)&quot;&gt;"
        );
    }

    #[test]
    fn an_empty_table_says_none_rather_than_drawing_a_bare_header() {
        // A header row with nothing under it reads as a rendering failure.
        assert!(table(&["a", "b"], &[]).contains("none"));
        assert!(!table(&["a", "b"], &[]).contains("<th>"));
    }

    #[test]
    fn a_diff_of_one_inserted_line_marks_one_line() {
        let d = diff("a\nb\nc\n", "a\nx\nb\nc\n");
        assert_eq!(d.matches("<ins>").count(), 1);
        assert_eq!(d.matches("<del>").count(), 0);
    }

    #[test]
    fn a_zero_row_still_draws() {
        // "this source did nothing" must be visible, not absent.
        let svg = bars(&[("crm".into(), 0.0), ("erp".into(), 5.0)], "");
        assert!(svg.contains("crm"));
        assert_eq!(svg.matches("<rect").count(), 2);
    }

    #[test]
    fn every_state_word_the_pages_use_has_a_reserved_colour() {
        for s in ["ok", "running", "failed", "refused", "drifted"] {
            assert_ne!(state_color(s), state_color("something-unknown"), "{s}");
        }
    }
}
