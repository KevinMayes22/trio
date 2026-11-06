//! Cleaning of the HTML that Azure DevOps stores in Description / Acceptance Criteria.
//!
//! Screenshots in those fields point at attachment URLs that need the PAT, which the webview
//! does not have. The backend fetches them and swaps each `src` for a `data:` URI so the page
//! never talks to Azure DevOps (and the CSP can stay strict).

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};

static IMG_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<img\b[^>]*>").unwrap());
static SRC_ATTR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\bsrc="([^"]*)""#).unwrap());
static ALT_ATTR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\balt="([^"]*)""#).unwrap());

static HTML_BLOCK_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<(div|p|br|span|ul|ol|li|table|h[1-6]|img|pre|blockquote|a)\b").unwrap());

const IMAGE_MISSING: &str = r#"<span class="img-missing">image unavailable</span>"#;

/// Removes scripts, event handlers, inline styles and anything else unsafe.
pub fn sanitize(html: &str) -> String {
    ammonia::clean(html)
}

/// Fields set to the Markdown format come back from Azure DevOps as the raw Markdown text,
/// not HTML. HTML fields always carry block tags (`<div>`, `<br>`, ...), so text without any
/// is taken to be Markdown. The result still goes through [`sanitize`] afterwards.
pub fn markdown_to_html_if_markdown(raw: &str) -> String {
    if HTML_BLOCK_TAG.is_match(raw) {
        return raw.to_string();
    }
    let options = pulldown_cmark::Options::ENABLE_TABLES
        | pulldown_cmark::Options::ENABLE_STRIKETHROUGH
        | pulldown_cmark::Options::ENABLE_TASKLISTS;
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, pulldown_cmark::Parser::new_ext(raw, options));
    out
}

fn unescape_attr(value: &str) -> String {
    value.replace("&amp;", "&")
}

/// Image URLs in a sanitised document, in order of appearance, without duplicates.
pub fn image_srcs(html: &str) -> Vec<String> {
    let mut seen = Vec::new();
    for tag in IMG_TAG.find_iter(html) {
        if let Some(src) = SRC_ATTR.captures(tag.as_str()) {
            let src = unescape_attr(&src[1]);
            if !seen.contains(&src) {
                seen.push(src);
            }
        }
    }
    seen
}

/// Replaces each image's `src` with its inlined `data:` URI. Images that could not be
/// fetched are replaced by a placeholder, so nothing in the page points at a remote URL.
pub fn inline_images(html: &str, inlined: &HashMap<String, String>) -> String {
    IMG_TAG
        .replace_all(html, |caps: &Captures| {
            let tag = &caps[0];
            let data = SRC_ATTR
                .captures(tag)
                .and_then(|src| inlined.get(&unescape_attr(&src[1])));
            match data {
                Some(data) => {
                    let alt = ALT_ATTR.captures(tag).map(|alt| alt[1].to_string()).unwrap_or_default();
                    format!(r#"<img src="{data}" alt="{alt}">"#)
                }
                None => IMAGE_MISSING.to_string(),
            }
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_fields_become_html() {
        let html = markdown_to_html_if_markdown("# Scope\n\n## 1. Overview\n\n* one\n* two\n\n`dbo.Accounts`");
        assert!(html.contains("<h1>Scope</h1>"), "{html}");
        assert!(html.contains("<h2>1. Overview</h2>"), "{html}");
        assert!(html.contains("<li>two</li>"), "{html}");
        assert!(html.contains("<code>dbo.Accounts</code>"), "{html}");
    }

    #[test]
    fn html_fields_are_left_alone() {
        let raw = "<div>Hello <b>there</b></div><div># not a heading</div>";
        assert_eq!(markdown_to_html_if_markdown(raw), raw);
    }

    #[test]
    fn sanitize_strips_scripts_handlers_and_styles() {
        let dirty = r#"<p style="color:red" onclick="x()">Hi</p><script>alert(1)</script><iframe src="https://evil"></iframe>"#;
        let clean = sanitize(dirty);
        assert!(clean.contains("Hi"));
        for bad in ["script", "onclick", "style", "iframe", "alert"] {
            assert!(!clean.contains(bad), "{bad} survived: {clean}");
        }
    }

    #[test]
    fn sanitize_removes_javascript_urls() {
        let clean = sanitize(r#"<a href="javascript:alert(1)">x</a>"#);
        assert!(!clean.contains("javascript"));
    }

    #[test]
    fn finds_unique_image_srcs_and_unescapes_ampersands() {
        let html = sanitize(
            r#"<img src="https://dev.azure.com/o/_apis/wit/attachments/1?fileName=a.png&download=true">
               <img src="https://dev.azure.com/o/_apis/wit/attachments/1?fileName=a.png&download=true">
               <img src="https://dev.azure.com/o/_apis/wit/attachments/2?fileName=b.png">"#,
        );
        assert_eq!(
            image_srcs(&html),
            vec![
                "https://dev.azure.com/o/_apis/wit/attachments/1?fileName=a.png&download=true",
                "https://dev.azure.com/o/_apis/wit/attachments/2?fileName=b.png",
            ]
        );
    }

    #[test]
    fn inlines_fetched_images_and_placeholders_the_rest() {
        let html = sanitize(
            r#"<p>Before</p><img src="https://dev.azure.com/o/a.png?x=1&y=2" alt="Login error"><img src="https://elsewhere.test/b.png">"#,
        );
        let mut inlined = HashMap::new();
        inlined.insert("https://dev.azure.com/o/a.png?x=1&y=2".to_string(), "data:image/png;base64,AAAA".to_string());

        let out = inline_images(&html, &inlined);

        assert!(out.contains(r#"<img src="data:image/png;base64,AAAA" alt="Login error">"#), "{out}");
        assert!(out.contains("image unavailable"));
        assert!(!out.contains("elsewhere.test"));
        assert!(!out.contains("dev.azure.com"));
    }
}
