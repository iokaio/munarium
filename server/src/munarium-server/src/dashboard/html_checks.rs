// SPDX-License-Identifier: Apache-2.0
//! Page-level checks over rendered console HTML (ClearView U13 and U14).
//!
//! The console is server-rendered and zero-JS, so most of what a browser
//! test would establish about structure can be read from the response body:
//! which controls exist, how they are labelled, which forms post and with
//! what, the order focus would move in, and which classes the stylesheet
//! must define. This module is a small, strict tag scanner for the HTML the
//! console itself generates. It is not a general HTML parser: it assumes
//! quoted attributes and escaped text, which every console page uses because
//! every value passes through `charts::esc`.
//!
//! What it cannot establish, and a real browser test would: computed styles
//! and contrast, the visible focus ring, layout at the AIRK acceptance
//! viewports and 200% zoom, the accessible names a screen reader announces,
//! and actual focus movement under Tab and Shift+Tab.

/// One start or end tag.
#[derive(Debug, Clone)]
pub(super) struct Tag {
    pub name: String,
    pub closing: bool,
    pub attrs: Vec<(String, Option<String>)>,
    /// Byte offset of the `<`.
    pub at: usize,
}

impl Tag {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_deref().unwrap_or(""))
    }

    pub fn has(&self, name: &str) -> bool {
        self.attrs.iter().any(|(k, _)| k == name)
    }
}

/// Every tag in document order. Comments and the doctype are skipped; the
/// contents of `<style>` are skipped (CSS selectors contain `>`).
pub(super) fn tags(html: &str) -> Vec<Tag> {
    let b = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = html[i..].find('<') {
        let at = i + off;
        let mut j = at + 1;
        if b.get(j) == Some(&b'!') {
            i = html[j..].find('>').map(|e| j + e + 1).unwrap_or(b.len());
            continue;
        }
        let closing = b.get(j) == Some(&b'/');
        if closing {
            j += 1;
        }
        let start = j;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'-') {
            j += 1;
        }
        let name = html[start..j].to_ascii_lowercase();
        assert!(!name.is_empty(), "stray '<' at byte {at}: {}", &html[at..]);
        let mut attrs = Vec::new();
        loop {
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            assert!(j < b.len(), "unterminated tag at byte {at}");
            if b[j] == b'>' {
                j += 1;
                break;
            }
            if b[j] == b'/' && b.get(j + 1) == Some(&b'>') {
                j += 2;
                break;
            }
            let k0 = j;
            while j < b.len() && !b[j].is_ascii_whitespace() && !matches!(b[j], b'=' | b'>' | b'/')
            {
                j += 1;
            }
            let key = html[k0..j].to_ascii_lowercase();
            assert!(!key.is_empty(), "malformed attribute in tag at byte {at}");
            if b.get(j) == Some(&b'=') {
                j += 1;
                let q = b[j];
                assert!(
                    q == b'"' || q == b'\'',
                    "unquoted attribute {key} in tag at byte {at}"
                );
                let v0 = j + 1;
                let end = html[v0..].find(q as char).expect("closing quote") + v0;
                attrs.push((key, Some(html[v0..end].to_string())));
                j = end + 1;
            } else {
                attrs.push((key, None));
            }
        }
        let is_style = name == "style" && !closing;
        out.push(Tag {
            name,
            closing,
            attrs,
            at,
        });
        i = j;
        if is_style {
            i = html[i..].find("</style>").map(|e| i + e).unwrap_or(b.len());
        }
    }
    out
}

/// Every class name used on the page.
pub(super) fn classes(html: &str) -> std::collections::BTreeSet<String> {
    tags(html)
        .iter()
        .filter_map(|t| t.attr("class"))
        .flat_map(|c| c.split_whitespace().map(String::from).collect::<Vec<_>>())
        .collect()
}

/// Whether the stylesheet has a selector naming `.class`.
pub(super) fn styled(css: &str, class: &str) -> bool {
    let needle = format!(".{class}");
    css.match_indices(&needle).any(|(i, _)| {
        !css[i + needle.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}

/// Controls a keyboard user can reach with Tab, in document order.
pub(super) fn focusables(tags: &[Tag]) -> Vec<&Tag> {
    tags.iter()
        .filter(|t| !t.closing)
        .filter(|t| match t.name.as_str() {
            "a" => t.has("href"),
            "button" | "select" | "textarea" | "summary" => true,
            "input" => t.attr("type") != Some("hidden"),
            _ => false,
        })
        .filter(|t| !t.has("disabled"))
        .collect()
}

/// The text between a start tag and the next tag.
fn text_after(html: &str, t: &Tag) -> String {
    let open = html[t.at..].find('>').expect("tag end") + t.at + 1;
    let end = html[open..].find('<').map_or(html.len(), |e| open + e);
    html[open..end].trim().to_string()
}

/// The structural and keyboard checks every console page must pass
/// (ClearView U14). Panics with the page path and the offending tag.
pub(super) fn assert_operable(path: &str, html: &str) {
    let tags = tags(html);
    fn starts<'a>(tags: &'a [Tag], name: &'a str) -> impl Iterator<Item = &'a Tag> + 'a {
        tags.iter().filter(move |t| !t.closing && t.name == name)
    }
    let starts = |name: &'static str| starts(&tags, name);
    let full_page = starts("html").count() == 1;
    assert!(full_page, "{path}: not one <html>");
    assert_eq!(
        starts("html").next().unwrap().attr("lang"),
        Some("en"),
        "{path}: <html> has no lang"
    );
    assert_eq!(starts("h1").count(), 1, "{path}: not exactly one h1");
    // One document title in <head>; charts carry their own SVG <title>
    // tooltips in the body.
    let head_end = tags
        .iter()
        .find(|t| t.closing && t.name == "head")
        .map(|t| t.at)
        .expect("</head>");
    assert_eq!(
        starts("title").filter(|t| t.at < head_end).count(),
        1,
        "{path}: not one document <title>"
    );

    // Zero-JS: no script, no inline handlers, no javascript: URLs. With no
    // script, nothing can capture focus, so there is no keyboard trap.
    assert_eq!(starts("script").count(), 0, "{path}: <script>");
    for t in &tags {
        for (k, v) in &t.attrs {
            assert!(
                !(k.starts_with("on") && k.len() > 2),
                "{path}: inline handler {k} on <{}>",
                t.name
            );
            if let Some(v) = v {
                assert!(
                    !v.trim_start()
                        .to_ascii_lowercase()
                        .starts_with("javascript:"),
                    "{path}: javascript: URL"
                );
            }
        }
        // Tab order is document order: no positive tabindex anywhere.
        if let Some(ti) = t.attr("tabindex") {
            let n: i32 = ti.trim().parse().expect("numeric tabindex");
            assert!(n <= 0, "{path}: positive tabindex on <{}>", t.name);
        }
    }

    // Unique ids, and every label `for` names one.
    let mut ids = std::collections::BTreeSet::new();
    for t in tags.iter().filter(|t| !t.closing) {
        if let Some(id) = t.attr("id") {
            assert!(ids.insert(id.to_string()), "{path}: duplicate id {id}");
        }
    }
    let label_for: std::collections::BTreeSet<&str> =
        starts("label").filter_map(|t| t.attr("for")).collect();
    for f in &label_for {
        assert!(ids.contains(*f), "{path}: <label for={f}> names nothing");
    }

    // Every control has a label: wrapped in <label>, named by `for`, or
    // given aria-label. Every post form carries the CSRF token.
    let mut label_depth = 0usize;
    let mut form: Option<(&Tag, bool)> = None;
    for t in &tags {
        match (t.name.as_str(), t.closing) {
            ("label", false) => label_depth += 1,
            ("label", true) => label_depth = label_depth.saturating_sub(1),
            ("form", false) => {
                assert!(form.is_none(), "{path}: nested form");
                form = Some((t, false));
            }
            ("form", true) => {
                let (f, csrf) = form.take().expect("form open");
                let method = f.attr("method").unwrap_or("get").to_ascii_lowercase();
                assert!(
                    method == "get" || method == "post",
                    "{path}: form method {method}"
                );
                let login = f.attr("action") == Some("/admin/login");
                if method == "post" && !login {
                    assert!(csrf, "{path}: post form without _csrf at byte {}", f.at);
                }
                assert!(f.has("action"), "{path}: form without action");
            }
            ("input", false) if t.attr("type") == Some("hidden") => {
                if t.attr("name") == Some("_csrf") {
                    assert!(
                        !t.attr("value").unwrap_or("").is_empty(),
                        "{path}: empty _csrf"
                    );
                    if let Some((_, csrf)) = form.as_mut() {
                        *csrf = true;
                    }
                }
            }
            ("input" | "select" | "textarea", false) => {
                let labelled = label_depth > 0
                    || t.attr("id").is_some_and(|id| label_for.contains(id))
                    || t.attr("aria-label").is_some_and(|l| !l.trim().is_empty());
                assert!(
                    labelled,
                    "{path}: unlabelled <{}> name={:?}",
                    t.name,
                    t.attr("name")
                );
                assert!(form.is_some(), "{path}: control outside a form");
            }
            ("button", false) => {
                let named = !text_after(html, t).is_empty()
                    || t.attr("aria-label").is_some_and(|l| !l.trim().is_empty());
                assert!(named, "{path}: button without a name");
                assert!(form.is_some(), "{path}: button outside a form");
            }
            ("a", false) => {
                assert!(t.has("href"), "{path}: <a> without href");
                let named = !text_after(html, t).is_empty()
                    || t.attr("aria-label").is_some_and(|l| !l.trim().is_empty());
                // A tile link's name is the tile's text, inside child tags.
                let tile = t.attr("class") == Some("tilelink");
                assert!(named || tile, "{path}: link without text at byte {}", t.at);
            }
            ("details", false) => {
                let next = tags.iter().find(|n| n.at > t.at).expect("details content");
                assert_eq!(next.name, "summary", "{path}: <details> without <summary>");
            }
            _ => {}
        }
    }
    assert!(form.is_none(), "{path}: unclosed form");

    // Focus order: the skip link first, then the navigation, then the page.
    // Login has no navigation and no skip link.
    if starts("nav").count() > 0 {
        let order = focusables(&tags);
        let first = order.first().expect("something focusable");
        assert_eq!(
            first.attr("href"),
            Some("#main"),
            "{path}: skip link not first"
        );
        assert!(ids.contains("main"), "{path}: skip target missing");
        let main_at = starts("main").next().expect("<main>").at;
        let nav_end = tags
            .iter()
            .filter(|t| t.closing && t.name == "nav")
            .map(|t| t.at)
            .max()
            .unwrap();
        assert!(nav_end < main_at, "{path}: navigation after main content");
        let in_main = order.iter().position(|t| t.at > main_at);
        if let Some(i) = in_main {
            assert!(
                order[..i]
                    .iter()
                    .all(|t| t.at < nav_end || t.attr("href") == Some("#main")),
                "{path}: focus leaves the navigation out of order"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scanner_reads_console_markup() {
        let html = r#"<!doctype html><html lang="en"><head><style>a > b { x: 1 }</style></head>
<body><form class="action stack" method="post"><label>uid <input type="text" name="uid" required></label>
<button type="submit" disabled>go</button></form><br/></body></html>"#;
        let t = tags(html);
        let names: Vec<String> = t
            .iter()
            .map(|t| format!("{}{}", if t.closing { "/" } else { "" }, t.name))
            .collect();
        assert_eq!(
            names,
            [
                "html", "head", "style", "/style", "/head", "body", "form", "label", "input",
                "/label", "button", "/button", "/form", "br", "/body", "/html"
            ]
        );
        let input = t.iter().find(|t| t.name == "input").unwrap();
        assert_eq!(input.attr("name"), Some("uid"));
        assert!(input.has("required"));
        assert!(classes(html).contains("stack"));
        assert!(styled(".stack{}", "stack"));
        assert!(!styled(".stacked{}", "stack"));
    }

    /// A page that passes, and one mutation of it per rule: each mutation
    /// must be refused, so a check that silently stopped looking would fail
    /// here.
    #[test]
    fn assert_operable_refuses_each_violation() {
        const OK: &str = r##"<!doctype html><html lang="en"><head><title>t</title></head><body><a class="skip" href="#main">skip to content</a><nav aria-label="console"><a href="/admin">overview</a></nav><main id="main"><h1>T</h1><form class="action" method="post" action="/x"><input type="hidden" name="_csrf" value="c"><label>uid <input type="text" name="uid"></label><button type="submit">go</button></form><details><summary>more</summary>x</details></main></body></html>"##;
        assert_operable("ok", OK);
        let cases = [
            ("no lang", OK.replace(r#" lang="en""#, "")),
            ("two h1", OK.replace("<h1>T</h1>", "<h1>T</h1><h1>U</h1>")),
            ("no csrf", OK.replace(r#"<input type="hidden" name="_csrf" value="c">"#, "")),
            ("empty csrf", OK.replace(r#"value="c""#, r#"value="""#)),
            (
                "unlabelled",
                OK.replace(r#"<label>uid <input type="text" name="uid"></label>"#, r#"<input type="text" name="uid">"#),
            ),
            ("positive tabindex", OK.replace(r#"<a href="/admin">"#, r#"<a href="/admin" tabindex="2">"#)),
            ("script", OK.replace("</main>", "<script>x()</script></main>")),
            ("handler", OK.replace(r#"<button type="submit">"#, r#"<button type="submit" onclick="x()">"#)),
            ("javascript url", OK.replace(r#"href="/admin""#, r#"href="javascript:x()""#)),
            ("nameless button", OK.replace(">go</button>", "></button>")),
            ("details without summary", OK.replace("<summary>more</summary>", "")),
            ("skip link not first", OK.replace(r##"<a class="skip" href="#main">skip to content</a>"##, "")),
            ("duplicate id", OK.replace("<h1>T</h1>", r#"<h1 id="main">T</h1>"#)),
            (
                "content before navigation",
                OK.replace(r#"<nav aria-label="console"><a href="/admin">overview</a></nav><main id="main">"#,
                           r#"<main id="main"><nav aria-label="console"><a href="/admin">overview</a></nav>"#),
            ),
        ];
        for (rule, html) in cases {
            assert_ne!(html, OK, "{rule}: the mutation did not apply");
            let refused = std::panic::catch_unwind(|| assert_operable(rule, &html)).is_err();
            assert!(refused, "{rule} was not refused");
        }
    }
}
