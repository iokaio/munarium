// SPDX-License-Identifier: Apache-2.0
//! Inline-SVG chart primitives for the /admin dashboard. Pure functions
//! returning SVG/HTML strings;
//! zero JavaScript, zero external assets (the report surface must work over
//! a bare port-forward with a strict CSP).
//!
//! Design method: the dataviz procedure (form → color-by-job → validated
//! palette → mark specs → hover → accessibility). Palette: the reference
//! instance, categorical slots 1–3 only (validated all-pairs in both modes;
//! the light-mode aqua slot is sub-3:1 on the surface, so the RELIEF RULE
//! applies — every chart page ships direct labels and a table view, which
//! `data_table` provides). Status colors are reserved for states (runbook
//! step states, error classes) and always ride with a text label. Hover:
//! native SVG `<title>` tooltips (the zero-JS equivalent of the tooltip
//! layer). Text wears ink tokens, never series colors.
//!
//! All user-derived strings pass through `esc()` — uids, model names, and
//! runbook refs are hostile input on an HTML surface.

/// HTML/attribute escape for every user-derived string.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
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

/// The design tokens, declared once per page. Light values on `:root`, dark
/// under `prefers-color-scheme` — both from the validated reference palette.
pub const STYLE: &str = r#"<style>
:root {
  color-scheme: light;
  --page: #f9f9f7; --surface: #fcfcfb;
  --ink: #0b0b0b; --ink-2: #52514e; --muted: #898781;
  --grid: #e1e0d9; --baseline: #c3c2b7; --ring: rgba(11,11,11,0.10);
  --s1: #2a78d6; --s2: #eb6834; --s3: #1baf7a;
  --good: #0ca30c; --warning: #fab219; --serious: #ec835a; --critical: #d03b3b;
}
@media (prefers-color-scheme: dark) {
  :root {
    color-scheme: dark;
    --page: #0d0d0d; --surface: #1a1a19;
    --ink: #ffffff; --ink-2: #c3c2b7; --muted: #898781;
    --grid: #2c2c2a; --baseline: #383835; --ring: rgba(255,255,255,0.10);
    --s1: #3987e5; --s2: #d95926; --s3: #199e70;
  }
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--page); color: var(--ink);
       font: 14px/1.45 system-ui, -apple-system, "Segoe UI", sans-serif; }
a { color: var(--s1); text-decoration: none; }
a:hover { text-decoration: underline; }
nav { display: flex; gap: 14px; flex-wrap: wrap; align-items: baseline;
      padding: 12px 20px; border-bottom: 1px solid var(--grid); background: var(--surface); }
nav .brand { font-weight: 600; color: var(--ink); margin-right: 8px; }
nav a.active { color: var(--ink); font-weight: 600; }
main { max-width: 1080px; margin: 0 auto; padding: 20px; }
h1 { font-size: 19px; margin: 6px 0 14px; }
h2 { font-size: 15px; margin: 22px 0 8px; color: var(--ink-2); }
.tiles { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 12px; }
.tile { background: var(--surface); border: 1px solid var(--ring); border-radius: 8px; padding: 12px 14px; }
.tile .label { color: var(--muted); font-size: 12px; }
.tile .value { font-size: 26px; font-weight: 600; margin-top: 2px; }
.tile .sub { color: var(--ink-2); font-size: 12px; margin-top: 2px; }
.card { background: var(--surface); border: 1px solid var(--ring); border-radius: 8px;
        padding: 14px; margin: 12px 0; overflow-x: auto; }
.legend { display: flex; gap: 16px; font-size: 12px; color: var(--ink-2); margin: 4px 0 8px; }
.legend .swatch { display: inline-block; width: 10px; height: 10px; border-radius: 2px;
                  margin-right: 5px; vertical-align: baseline; }
table.data { border-collapse: collapse; width: 100%; font-size: 13px; }
table.data th { text-align: left; color: var(--muted); font-weight: 500;
                border-bottom: 1px solid var(--baseline); padding: 4px 10px 4px 0; }
table.data td { border-bottom: 1px solid var(--grid); padding: 4px 10px 4px 0;
                font-variant-numeric: tabular-nums; }
details.table { margin-top: 8px; }
details.table summary { color: var(--muted); font-size: 12px; cursor: pointer; }
.empty { color: var(--muted); padding: 18px 0; }
.notice { background: var(--surface); border: 1px solid var(--ring); border-left: 3px solid var(--warning);
          border-radius: 6px; padding: 12px 14px; margin: 12px 0; }
form.login { max-width: 340px; margin: 60px auto; background: var(--surface);
             border: 1px solid var(--ring); border-radius: 8px; padding: 20px; }
form.login input[type=password] { width: 100%; padding: 8px; margin: 8px 0 12px;
             border: 1px solid var(--baseline); border-radius: 6px;
             background: var(--page); color: var(--ink); }
form.login button { padding: 8px 16px; border: 0; border-radius: 6px;
             background: var(--s1); color: #fff; cursor: pointer; }
pre { background: var(--page); border: 1px solid var(--grid); border-radius: 6px;
      padding: 10px 12px; overflow-x: auto; font-size: 12px; line-height: 1.4; }
code { font-size: 12px; }
table.kv { border-collapse: collapse; font-size: 13px; }
table.kv td { padding: 3px 14px 3px 0; vertical-align: top; border-bottom: 1px solid var(--grid); }
table.kv td:first-child { color: var(--muted); white-space: nowrap; width: 200px; }
.badge { display: inline-block; padding: 1px 8px; border-radius: 10px; font-size: 11px;
         background: var(--grid); color: var(--ink-2); margin-left: 6px; vertical-align: middle; }
.badge.warn { background: var(--warning); color: #0b0b0b; }
form.action { display: inline-block; margin: 4px 10px 4px 0; vertical-align: middle; }
form.action label { margin-right: 8px; font-size: 13px; color: var(--ink-2); }
form.action input[type=text], form.action input[type=password], form.action input[type=number],
form.action select { padding: 5px 7px; border: 1px solid var(--baseline); border-radius: 5px;
         background: var(--page); color: var(--ink); margin: 2px 4px 2px 0; }
form.action button { padding: 5px 12px; border: 0; border-radius: 5px;
         background: var(--s1); color: #fff; cursor: pointer; }
form.action button.danger { background: var(--critical); }
.viewonly { color: var(--muted); font-size: 12px; }
a.tilelink { color: inherit; display: block; }
a.tilelink:hover { text-decoration: none; }
a.tilelink:hover .tile { border-color: var(--s1); }
h3 { font-size: 14px; margin: 16px 0 6px; }
h4 { font-size: 13px; margin: 12px 0 4px; color: var(--ink-2); }
.secret { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; word-break: break-all; }
</style>"#;

const W: f64 = 1000.0;
const H: f64 = 230.0;
const PAD_L: f64 = 52.0;
const PAD_R: f64 = 12.0;
const PAD_T: f64 = 10.0;
const PAD_B: f64 = 26.0;

fn fmt_num(v: f64) -> String {
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 10_000.0 {
        format!("{:.0}k", v / 1_000.0)
    } else if v >= 1_000.0 {
        format!("{:.1}k", v / 1_000.0)
    } else if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

/// Short display form of an RFC 3339 bucket label: HH:MM for sub-day
/// buckets, MM-DD elsewise.
fn short_bucket(label: &str, bucket_seconds: i64) -> String {
    if bucket_seconds < 86_400 && label.len() >= 16 {
        if bucket_seconds >= 3_600 && label.len() >= 16 {
            label[5..16].replace('T', " ")
        } else {
            label[11..16].to_string()
        }
    } else if label.len() >= 10 {
        label[5..10].to_string()
    } else {
        label.to_string()
    }
}

/// One named series for `line_chart`.
pub struct Series<'a> {
    pub name: &'a str,
    /// CSS color token, e.g. "var(--s1)".
    pub color: &'a str,
    pub points: Vec<Option<f64>>,
}

/// Multi-series line chart over shared x buckets. 2px lines, hairline grid,
/// muted axis ink, legend for >= 2 series, native <title> hover targets.
pub fn line_chart(x_labels: &[String], bucket_seconds: i64, series: &[Series]) -> String {
    let n = x_labels.len();
    if n == 0 || series.iter().all(|s| s.points.iter().all(|p| p.is_none())) {
        return r#"<div class="empty">no data in this window</div>"#.into();
    }
    let y_max = series
        .iter()
        .flat_map(|s| s.points.iter().flatten())
        .fold(0.0_f64, |a, &b| a.max(b))
        .max(1.0)
        * 1.08;
    let px = |i: usize| -> f64 {
        if n <= 1 {
            PAD_L + (W - PAD_L - PAD_R) / 2.0
        } else {
            PAD_L + (W - PAD_L - PAD_R) * i as f64 / (n - 1) as f64
        }
    };
    let py = |v: f64| -> f64 { H - PAD_B - (H - PAD_T - PAD_B) * (v / y_max) };

    let mut svg = format!(
        r#"<svg viewBox="0 0 {W} {H}" width="100%" role="img" xmlns="http://www.w3.org/2000/svg">"#
    );
    // hairline grid: 4 horizontal lines + muted y labels
    for g in 1..=4 {
        let v = y_max * g as f64 / 4.0;
        let y = py(v);
        svg.push_str(&format!(
            r#"<line x1="{PAD_L}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="var(--grid)" stroke-width="1"/><text x="{:.1}" y="{:.1}" text-anchor="end" font-size="11" fill="var(--muted)">{}</text>"#,
            W - PAD_R,
            PAD_L - 6.0,
            y + 4.0,
            fmt_num(v)
        ));
    }
    // baseline + x labels (about 6, evenly thinned)
    svg.push_str(&format!(
        r#"<line x1="{PAD_L}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="var(--baseline)" stroke-width="1"/>"#,
        H - PAD_B,
        W - PAD_R,
        H - PAD_B
    ));
    let step = (n / 6).max(1);
    for (i, label) in x_labels.iter().enumerate().step_by(step) {
        svg.push_str(&format!(
            r#"<text x="{:.1}" y="{:.1}" text-anchor="middle" font-size="11" fill="var(--muted)">{}</text>"#,
            px(i),
            H - PAD_B + 16.0,
            esc(&short_bucket(label, bucket_seconds))
        ));
    }
    // series lines + hover targets
    for s in series {
        let mut d = String::new();
        let mut pen_down = false;
        for (i, p) in s.points.iter().enumerate() {
            match p {
                Some(v) => {
                    let cmd = if pen_down { 'L' } else { 'M' };
                    d.push_str(&format!("{cmd}{:.1} {:.1} ", px(i), py(*v)));
                    pen_down = true;
                }
                None => pen_down = false,
            }
        }
        svg.push_str(&format!(
            r#"<path d="{}" fill="none" stroke="{}" stroke-width="2" stroke-linejoin="round"/>"#,
            d.trim_end(),
            s.color
        ));
        for (i, p) in s.points.iter().enumerate() {
            if let Some(v) = p {
                svg.push_str(&format!(
                    r#"<circle cx="{:.1}" cy="{:.1}" r="9" fill="transparent"><title>{} — {}: {}</title></circle>"#,
                    px(i),
                    py(*v),
                    esc(&x_labels[i]),
                    esc(s.name),
                    fmt_num(*v)
                ));
            }
        }
    }
    svg.push_str("</svg>");

    let legend = if series.len() >= 2 {
        let items: String = series
            .iter()
            .map(|s| {
                format!(
                    r#"<span><span class="swatch" style="background:{}"></span>{}</span>"#,
                    s.color,
                    esc(s.name)
                )
            })
            .collect();
        format!(r#"<div class="legend">{items}</div>"#)
    } else {
        String::new()
    };
    format!("{legend}{svg}")
}

/// Stacked vertical bars over shared x buckets: 2px surface gaps between
/// segments, per-segment <title> hover, legend, status/series colors by
/// caller. Segment order = series order (bottom-up).
pub fn stacked_bars(x_labels: &[String], bucket_seconds: i64, series: &[Series]) -> String {
    let n = x_labels.len();
    let totals: Vec<f64> = (0..n)
        .map(|i| series.iter().filter_map(|s| s.points[i]).sum())
        .collect();
    let y_max = totals.iter().fold(0.0_f64, |a, &b| a.max(b)).max(1.0) * 1.08;
    if n == 0 || y_max <= 1.081 && totals.iter().all(|t| *t == 0.0) {
        return r#"<div class="empty">no data in this window</div>"#.into();
    }
    let plot_w = W - PAD_L - PAD_R;
    let bw = (plot_w / n as f64 * 0.72).min(48.0);
    let px = |i: usize| PAD_L + plot_w * (i as f64 + 0.5) / n as f64 - bw / 2.0;
    let scale = |v: f64| (H - PAD_T - PAD_B) * (v / y_max);

    let mut svg = format!(
        r#"<svg viewBox="0 0 {W} {H}" width="100%" role="img" xmlns="http://www.w3.org/2000/svg">"#
    );
    for g in 1..=4 {
        let v = y_max * g as f64 / 4.0;
        let y = H - PAD_B - scale(v);
        svg.push_str(&format!(
            r#"<line x1="{PAD_L}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="var(--grid)" stroke-width="1"/><text x="{:.1}" y="{:.1}" text-anchor="end" font-size="11" fill="var(--muted)">{}</text>"#,
            W - PAD_R,
            PAD_L - 6.0,
            y + 4.0,
            fmt_num(v)
        ));
    }
    svg.push_str(&format!(
        r#"<line x1="{PAD_L}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="var(--baseline)" stroke-width="1"/>"#,
        H - PAD_B,
        W - PAD_R,
        H - PAD_B
    ));
    let step = (n / 6).max(1);
    for (i, label) in x_labels.iter().enumerate().step_by(step) {
        svg.push_str(&format!(
            r#"<text x="{:.1}" y="{:.1}" text-anchor="middle" font-size="11" fill="var(--muted)">{}</text>"#,
            px(i) + bw / 2.0,
            H - PAD_B + 16.0,
            esc(&short_bucket(label, bucket_seconds))
        ));
    }
    for (i, label) in x_labels.iter().enumerate() {
        let mut y_cursor = H - PAD_B;
        for s in series {
            let Some(v) = s.points[i] else { continue };
            if v <= 0.0 {
                continue;
            }
            let h = scale(v).max(1.0);
            let y = y_cursor - h;
            svg.push_str(&format!(
                r#"<rect x="{:.1}" y="{y:.1}" width="{bw:.1}" height="{:.1}" fill="{}"><title>{} — {}: {}</title></rect>"#,
                px(i),
                (h - 2.0).max(1.0), // 2px surface gap to the segment below
                s.color,
                esc(label),
                esc(s.name),
                fmt_num(v)
            ));
            y_cursor = y;
        }
    }
    svg.push_str("</svg>");

    let items: String = series
        .iter()
        .map(|s| {
            format!(
                r#"<span><span class="swatch" style="background:{}"></span>{}</span>"#,
                s.color,
                esc(s.name)
            )
        })
        .collect();
    format!(r#"<div class="legend">{items}</div>{svg}"#)
}

/// Horizontal bar rows with direct labels (the relief rule made visible):
/// label left, thin bar, value right.
pub fn hbar_rows(rows: &[(String, f64, String)]) -> String {
    if rows.is_empty() {
        return r#"<div class="empty">no data in this window</div>"#.into();
    }
    let max = rows.iter().fold(0.0_f64, |a, r| a.max(r.1)).max(1.0);
    let mut out = String::from(r#"<table class="data" role="presentation">"#);
    for (label, value, sub) in rows {
        let pct = (value / max * 100.0).clamp(0.5, 100.0);
        out.push_str(&format!(
            r#"<tr><td style="width:38%;max-width:380px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{}</td><td><div style="background:var(--s1);height:10px;border-radius:0 4px 4px 0;width:{pct:.1}%"><span style="display:none">{}</span></div></td><td style="width:120px;text-align:right">{} <span style="color:var(--muted)">{}</span></td></tr>"#,
            esc(label),
            fmt_num(*value),
            fmt_num(*value),
            esc(sub)
        ));
    }
    out.push_str("</table>");
    out
}

/// The table view every chart card carries (accessibility + relief rule),
/// collapsed by default.
pub fn data_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut out = String::from(
        r#"<details class="table"><summary>table view</summary><table class="data"><tr>"#,
    );
    for h in headers {
        out.push_str(&format!("<th>{}</th>", esc(h)));
    }
    out.push_str("</tr>");
    for row in rows {
        out.push_str("<tr>");
        for cell in row {
            out.push_str(&format!("<td>{}</td>", esc(cell)));
        }
        out.push_str("</tr>");
    }
    out.push_str("</table></details>");
    out
}

/// A stat tile (hero number). `value` is pre-formatted by the caller.
pub fn tile(label: &str, value: &str, sub: &str) -> String {
    format!(
        r#"<div class="tile"><div class="label">{}</div><div class="value">{}</div><div class="sub">{}</div></div>"#,
        esc(label),
        esc(value),
        esc(sub)
    )
}

/// Map a runbook/step state to its status token (reserved colors, always
/// rendered beside the state's text label — never color alone).
pub fn state_color(state: &str) -> &'static str {
    match state {
        "done" => "var(--good)",
        "failed" => "var(--critical)",
        "awaiting_approval" | "pending" => "var(--warning)",
        _ => "var(--s1)", // running / in-flight
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_neutralizes_hostile_strings() {
        assert_eq!(
            esc(r#"<script>alert("x")</script>"#),
            "&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;"
        );
        assert_eq!(esc("a&b'c"), "a&amp;b&#39;c");
    }

    #[test]
    fn line_chart_escapes_series_names_and_renders_svg() {
        let out = line_chart(
            &["2026-08-17T10:00:00Z".into(), "2026-08-17T10:01:00Z".into()],
            60,
            &[Series {
                name: "<evil>",
                color: "var(--s1)",
                points: vec![Some(1.0), Some(2.0)],
            }],
        );
        assert!(out.contains("<svg"), "{out}");
        assert!(out.contains("&lt;evil&gt;"));
        assert!(!out.contains("<evil>"));
    }

    #[test]
    fn empty_series_says_so_instead_of_rendering_junk() {
        let out = line_chart(&[], 60, &[]);
        assert!(out.contains("no data"));
    }

    #[test]
    fn stacked_bars_gap_and_titles() {
        let out = stacked_bars(
            &["b1".into()],
            3600,
            &[
                Series {
                    name: "input",
                    color: "var(--s1)",
                    points: vec![Some(10.0)],
                },
                Series {
                    name: "output",
                    color: "var(--s2)",
                    points: vec![Some(5.0)],
                },
            ],
        );
        assert!(out.contains("<title>b1 — input: 10</title>"));
        assert!(out.contains("legend"));
    }

    #[test]
    fn table_view_is_present_and_escaped() {
        let out = data_table(&["uid"], &[vec!["<u>".into()]]);
        assert!(out.contains("table view"));
        assert!(out.contains("&lt;u&gt;"));
    }
}
