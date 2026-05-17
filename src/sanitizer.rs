//! HTML sanitization for email bodies before they reach the render pane.
//!
//! Email is a hostile input: a sender can ship `<script>`, `<iframe>`,
//! inline event handlers, or remote tracking images and have the browser
//! execute or fetch them when the user opens the message. Vulthor's web
//! pane writes the body via `innerHTML`, so any unsanitized markup is a
//! direct XSS / exfiltration channel.
//!
//! `sanitize_email_html` is the single boundary that strips this attack
//! surface. It runs once at parse time so the unsanitized string never
//! reaches the in-memory `Email::body_html`. See `email.rs::parse_body`.
//!
//! Policy:
//! - Conservative tag allowlist (formatting + structure + links + images).
//! - Inline event handlers are stripped (ammonia default).
//! - `<a>` is forced to `target="_blank" rel="noopener noreferrer"`.
//! - `<img src>` keeps `data:` and `cid:` URIs and rewrites every other
//!   scheme to a 1×1 transparent placeholder so remote trackers cannot
//!   fire. This is the boundary the `images-hidden` setting also relies on.

use ammonia::Builder;
use std::borrow::Cow;
use std::collections::HashSet;

/// 1×1 transparent GIF used to replace remote `<img src>` so the
/// browser does not fetch tracking pixels. Inline so the placeholder
/// itself never hits the network.
const REMOTE_IMG_PLACEHOLDER: &str =
    "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

/// Sanitize an email body's HTML for safe rendering in the web pane.
///
/// Returns a string that contains only allowlisted tags and attributes,
/// with remote image sources neutralized and anchors forced to open in
/// a new tab with `rel="noopener noreferrer"`. The input is treated as
/// adversarial; the output is safe to inject via `innerHTML`.
pub fn sanitize_email_html(raw: &str) -> String {
    let allowed_tags: HashSet<&str> = [
        "p",
        "br",
        "hr",
        "b",
        "i",
        "em",
        "strong",
        "u",
        "s",
        "small",
        "sub",
        "sup",
        "a",
        "ul",
        "ol",
        "li",
        "blockquote",
        "pre",
        "code",
        "table",
        "thead",
        "tbody",
        "tfoot",
        "tr",
        "td",
        "th",
        "caption",
        "colgroup",
        "col",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "span",
        "div",
        "img",
    ]
    .into_iter()
    .collect();

    // `data:` and `cid:` must reach the attribute filter so the filter
    // can keep legitimate inline images while rewriting remote ones.
    // Ammonia's default scheme allowlist rejects them before the filter
    // ever runs, so widen it here. We then re-narrow `data:` to just
    // `data:image/*` for `<img src>` and reject it outright for
    // `<a href>` (a `data:text/html,...` link would re-introduce XSS).
    let url_schemes: HashSet<&str> = [
        "http", "https", "mailto", "tel", "ftp", "ftps", "data", "cid",
    ]
    .into_iter()
    .collect();

    Builder::default()
        .tags(allowed_tags)
        .url_schemes(url_schemes)
        .link_rel(Some("noopener noreferrer"))
        .set_tag_attribute_value("a", "target", "_blank")
        .attribute_filter(|element, attribute, value| match (element, attribute) {
            ("img", "src") => {
                let lower = value.trim_start().to_ascii_lowercase();
                if lower.starts_with("data:image/") || lower.starts_with("cid:") {
                    Some(Cow::Owned(value.to_string()))
                } else {
                    Some(Cow::Borrowed(REMOTE_IMG_PLACEHOLDER))
                }
            }
            ("a", "href") => {
                let lower = value.trim_start().to_ascii_lowercase();
                if lower.starts_with("data:") || lower.starts_with("cid:") {
                    None
                } else {
                    Some(Cow::Owned(value.to_string()))
                }
            }
            _ => Some(Cow::Owned(value.to_string())),
        })
        .clean(raw)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `<script>` is the canonical XSS vector — the sanitizer must strip
    /// the tag and its contents so injected JS never reaches the DOM.
    #[test]
    fn script_tag_is_stripped() {
        let out = sanitize_email_html("<p>hi</p><script>alert(1)</script>");
        assert!(!out.contains("<script"));
        assert!(!out.contains("alert(1)"));
        assert!(out.contains("<p>hi</p>"));
    }

    /// Inline data-URI images are the legitimate way email embeds
    /// pictures, so they must round-trip through the sanitizer intact.
    #[test]
    fn data_uri_image_survives() {
        let src = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
        let html = format!("<img src=\"{}\" alt=\"pixel\">", src);
        let out = sanitize_email_html(&html);
        assert!(
            out.contains(src),
            "data: URI must survive sanitization, got: {}",
            out
        );
    }

    /// Remote `<img src>` is a tracking-pixel vector. The sanitizer
    /// must replace it with the inline placeholder so the browser
    /// never issues the request.
    #[test]
    fn remote_image_becomes_placeholder() {
        let out = sanitize_email_html("<img src=\"https://evil.example/track.png\">");
        assert!(
            !out.contains("evil.example"),
            "remote host must be stripped, got: {}",
            out
        );
        assert!(
            out.contains(REMOTE_IMG_PLACEHOLDER),
            "remote src must be rewritten to placeholder, got: {}",
            out
        );
    }

    /// Anchors must open in a new tab with `rel=noopener noreferrer`
    /// so a clicked link cannot reach back into the email's window
    /// (`window.opener`) and cannot leak the referrer.
    #[test]
    fn anchor_gets_target_blank_and_noopener_noreferrer() {
        let out = sanitize_email_html("<a href=\"https://example.com\">link</a>");
        assert!(out.contains("href=\"https://example.com\""));
        assert!(
            out.contains("target=\"_blank\""),
            "anchor must be forced to target=_blank, got: {}",
            out
        );
        assert!(
            out.contains("noopener") && out.contains("noreferrer"),
            "anchor must carry rel=noopener noreferrer, got: {}",
            out
        );
    }

    /// Inline event handlers (`onclick`, `onerror`, …) execute attacker
    /// JS even without a `<script>` tag, so they must be stripped from
    /// every element ammonia keeps.
    #[test]
    fn inline_event_handlers_are_stripped() {
        let out = sanitize_email_html(
            "<a href=\"#\" onclick=\"alert(1)\">x</a><div onerror=\"alert(2)\">y</div>",
        );
        assert!(
            !out.contains("onclick"),
            "onclick must be stripped: {}",
            out
        );
        assert!(
            !out.contains("onerror"),
            "onerror must be stripped: {}",
            out
        );
        assert!(
            !out.contains("alert("),
            "handler body must be stripped: {}",
            out
        );
    }

    /// `<iframe>` is not on the allowlist — both the tag and any
    /// `src` URL must be dropped so the browser cannot load a third-
    /// party document into the email pane.
    #[test]
    fn iframe_is_stripped_entirely() {
        let out = sanitize_email_html("<iframe src=\"https://evil.example/\"></iframe>");
        assert!(
            !out.contains("<iframe"),
            "iframe tag must be stripped: {}",
            out
        );
        assert!(
            !out.contains("evil.example"),
            "iframe src must not leak: {}",
            out
        );
    }

    /// `<style>`, `<object>`, `<embed>`, `<base>`, `<form>`, `<input>`,
    /// `<button>`, and `<link>` are all explicitly named in the bead
    /// as dangerous tags. They must not survive sanitization.
    #[test]
    fn other_dangerous_tags_are_stripped() {
        let cases = [
            "<style>body{display:none}</style>",
            "<object data=\"evil\"></object>",
            "<embed src=\"evil\">",
            "<base href=\"https://evil.example/\">",
            "<form action=\"/x\"><input name=\"a\"><button>go</button></form>",
            "<link rel=\"stylesheet\" href=\"https://evil.example/x.css\">",
        ];
        for input in cases {
            let out = sanitize_email_html(input);
            assert!(
                !out.contains("evil.example") && !out.contains("display:none"),
                "dangerous tag attribute must not leak through: input={} out={}",
                input,
                out,
            );
            for tag in [
                "<style", "<object", "<embed", "<base", "<form", "<input", "<button", "<link",
            ] {
                assert!(
                    !out.contains(tag),
                    "{} must be stripped: input={} out={}",
                    tag,
                    input,
                    out
                );
            }
        }
    }

    /// `javascript:` URLs in anchors are a classic XSS bypass when
    /// `<script>` is blocked. Ammonia drops them by default; this
    /// test pins the behavior so a future config change can't
    /// regress it silently.
    #[test]
    fn javascript_href_is_stripped() {
        let out = sanitize_email_html("<a href=\"javascript:alert(1)\">x</a>");
        assert!(
            !out.contains("javascript:"),
            "javascript: href must be stripped, got: {}",
            out
        );
    }

    /// Standard formatting markup is the bulk of legitimate email
    /// HTML; it must round-trip unchanged so the sanitizer doesn't
    /// degrade rendering for safe content.
    #[test]
    fn formatting_markup_is_preserved() {
        let input = "<p><strong>bold</strong> <em>italic</em></p>\
                     <ul><li>a</li><li>b</li></ul>\
                     <table><thead><tr><th>h</th></tr></thead>\
                     <tbody><tr><td>c</td></tr></tbody></table>";
        let out = sanitize_email_html(input);
        for fragment in [
            "<strong>bold</strong>",
            "<em>italic</em>",
            "<ul>",
            "<li>a</li>",
            "<table>",
            "<th>h</th>",
            "<td>c</td>",
        ] {
            assert!(out.contains(fragment), "expected {} in {}", fragment, out);
        }
    }
}
