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
}
