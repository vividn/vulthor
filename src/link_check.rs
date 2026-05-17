//! Link-spoofing detection for sanitized email HTML.
//!
//! Phishing emails commonly anchor a trustworthy-looking domain
//! (`paypal.com`) as visible link text while pointing the underlying
//! `href` at attacker-controlled infrastructure. `sanitize_email_html`
//! already neutralizes scripts and remote images, but a plain `<a>`
//! whose text lies about its destination is still dangerous. This
//! module wraps suspicious anchors in `<span class="spoof-warn" ...>`
//! so the renderer can call them out visibly.
//!
//! Runs *after* `ammonia::clean` so the input is well-formed and
//! tag-allowlisted; that lets us walk the string linearly instead of
//! taking a full HTML-parser dependency for one targeted check.

/// Scan sanitized HTML for `<a>` tags whose visible text claims a
/// domain that differs from the `href` host, and wrap each such anchor
/// in a `<span class="spoof-warn">` with a `title` explaining the
/// mismatch. Inline styling is applied because the body renders inside
/// a sandboxed `srcdoc` iframe where the parent stylesheet does not
/// cascade.
pub fn flag_spoofed_links(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut pos = 0;
    let bytes = html.as_bytes();

    while pos < bytes.len() {
        let Some(open_rel) = find_anchor_open(&bytes[pos..]) else {
            out.push_str(&html[pos..]);
            break;
        };
        let open_abs = pos + open_rel;
        out.push_str(&html[pos..open_abs]);

        // Find end of opening tag (first '>' after '<a').
        let Some(gt_rel) = bytes[open_abs..].iter().position(|&b| b == b'>') else {
            out.push_str(&html[open_abs..]);
            break;
        };
        let open_end_abs = open_abs + gt_rel + 1;

        // Find matching </a>.
        let Some(close_rel) = find_anchor_close(&bytes[open_end_abs..]) else {
            out.push_str(&html[open_abs..]);
            break;
        };
        let close_abs = open_end_abs + close_rel;
        let close_end_abs = close_abs + 4; // length of "</a>"

        let opening_tag = &html[open_abs..open_end_abs];
        let inner = &html[open_end_abs..close_abs];
        let full = &html[open_abs..close_end_abs];

        let href = extract_attr(opening_tag, "href");
        let text = strip_tags_and_decode(inner);

        let warning = match (href.as_deref(), find_domain_in_text(&text)) {
            (Some(href_val), Some(text_domain)) => match extract_host(href_val) {
                Some(href_host) if !domains_match(&href_host, &text_domain) => {
                    Some((text_domain, href_host))
                }
                _ => None,
            },
            _ => None,
        };

        if let Some((claimed, actual)) = warning {
            let title = format!(
                "Suspicious link: text claims \"{}\" but href goes to \"{}\"",
                claimed, actual
            );
            out.push_str("<span class=\"spoof-warn\" title=\"");
            out.push_str(&escape_attr(&title));
            out.push_str(
                "\" style=\"background:#ffe6e6;border:1px solid #d9534f;\
                 border-radius:3px;padding:0 4px;\">⚠ ",
            );
            out.push_str(full);
            out.push_str("</span>");
        } else {
            out.push_str(full);
        }

        pos = close_end_abs;
    }
    out
}

fn find_anchor_open(bytes: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'<'
            && (bytes[i + 1] == b'a' || bytes[i + 1] == b'A')
            && matches!(bytes[i + 2], b' ' | b'>' | b'\t' | b'\n' | b'\r' | b'/')
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_anchor_close(bytes: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == b'<'
            && bytes[i + 1] == b'/'
            && (bytes[i + 2] == b'a' || bytes[i + 2] == b'A')
            && bytes[i + 3] == b'>'
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Pull a quoted attribute value out of an opening tag. Case-insensitive
/// on the attribute name; supports double or single quotes.
fn extract_attr(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{}=", attr);
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find(&needle) {
        let name_start = search_from + rel;
        // Ensure the match isn't a suffix of a longer attribute name
        // (e.g. "data-href=" should not match "href=").
        let preceded_ok = name_start == 0
            || matches!(
                lower.as_bytes()[name_start - 1],
                b' ' | b'\t' | b'\n' | b'\r' | b'/'
            );
        if !preceded_ok {
            search_from = name_start + needle.len();
            continue;
        }
        let val_start = name_start + needle.len();
        let rest = &tag[val_start..];
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let after_quote = &rest[1..];
        let end = after_quote.find(quote)?;
        return Some(after_quote[..end].to_string());
    }
    None
}

fn strip_tags_and_decode(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        if in_tag {
            if c == '>' {
                in_tag = false;
            }
        } else if c == '<' {
            in_tag = true;
        } else {
            out.push(c);
        }
    }
    decode_entities(&out)
}

fn decode_entities(s: &str) -> String {
    // Order matters: &amp; must be replaced last so we don't double-decode
    // things like "&amp;lt;" (which represents the literal "&lt;").
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Extract the host from a URL. Accepts `http://`, `https://`, and
/// protocol-relative `//host/...`. Returns `None` for `mailto:`,
/// `tel:`, relative paths, and anything else — those have no host
/// for spoofing comparison purposes.
fn extract_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let rest = if let Some(idx) = lower.find("://") {
        let scheme = &lower[..idx];
        if scheme != "http" && scheme != "https" {
            return None;
        }
        &trimmed[idx + 3..]
    } else if let Some(r) = trimmed.strip_prefix("//") {
        r
    } else {
        return None;
    };
    let host_end = rest
        .find(|c: char| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let host_part = &rest[..host_end];
    // Drop user:pass@ if present.
    let host = host_part.rsplit('@').next().unwrap_or(host_part);
    // Drop :port.
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host.trim_end_matches('.').to_ascii_lowercase())
    }
}

/// Look for a domain-shaped substring in the visible link text. If the
/// text is itself a URL, returns the URL's host; otherwise returns the
/// first bare-domain occurrence (e.g. `paypal.com`, `mail.bank.co.uk`).
/// The TLD label must be all-alphabetic and at least two characters to
/// reduce false positives on things like version strings (`1.2.3`).
fn find_domain_in_text(text: &str) -> Option<String> {
    if let Some(host) = extract_host(text) {
        return Some(host);
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if is_label_start(bytes[i]) {
            let start = i;
            let mut end = i;
            let mut saw_dot = false;
            loop {
                while end < bytes.len() && is_label_char(bytes[end]) {
                    end += 1;
                }
                if end + 1 < bytes.len()
                    && bytes[end] == b'.'
                    && is_label_start(bytes[end + 1])
                {
                    saw_dot = true;
                    end += 1;
                } else {
                    break;
                }
            }
            if saw_dot {
                let candidate = &text[start..end];
                let tld = candidate.rsplit('.').next().unwrap_or("");
                if tld.len() >= 2 && tld.bytes().all(|b| b.is_ascii_alphabetic()) {
                    return Some(candidate.to_ascii_lowercase());
                }
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

fn is_label_start(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

fn is_label_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

/// Compare two hostnames by their registrable-ish base domain (last
/// two labels), case-insensitive, ignoring a leading `www.`. This
/// treats `mail.paypal.com` and `paypal.com` as the same site and
/// `paypal.com` vs `paypal.evil.com` as different.
///
/// We don't carry a public-suffix list, so `foo.co.uk` and `bar.co.uk`
/// collapse to `co.uk` and would compare equal. False negatives are
/// acceptable here — the cost of missing a spoof is lower than the
/// cost of a noisy warning on legitimate `co.uk` mail.
fn domains_match(href_host: &str, text_domain: &str) -> bool {
    base_domain(href_host) == base_domain(text_domain)
}

fn base_domain(host: &str) -> String {
    let host = host.strip_prefix("www.").unwrap_or(host).to_ascii_lowercase();
    let parts: Vec<&str> = host.split('.').filter(|s| !s.is_empty()).collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[parts.len() - 2], parts[parts.len() - 1])
    } else {
        host
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canonical phishing shape: visible text says `paypal.com` but the
    /// `href` points at an unrelated domain. Must produce a `spoof-warn`
    /// wrapper carrying both domains in the `title`.
    #[test]
    fn mismatched_domain_is_flagged() {
        let input = r#"<a href="https://evil.tld/login">paypal.com</a>"#;
        let out = flag_spoofed_links(input);
        assert!(out.contains("spoof-warn"), "missing wrapper: {}", out);
        assert!(out.contains("paypal.com"), "claimed domain lost: {}", out);
        assert!(out.contains("evil.tld"), "actual host lost: {}", out);
        assert!(out.contains("title=\""), "title attribute missing: {}", out);
    }

    /// When the visible domain matches the `href` host (including
    /// subdomain / www variants), no warning should be emitted — the
    /// link is honest about where it's going.
    #[test]
    fn matching_domain_is_not_flagged() {
        let cases = [
            r#"<a href="https://paypal.com/x">paypal.com</a>"#,
            r#"<a href="https://www.paypal.com/x">paypal.com</a>"#,
            r#"<a href="https://mail.paypal.com/inbox">paypal.com</a>"#,
            r#"<a href="https://paypal.com">go to https://paypal.com/login</a>"#,
        ];
        for input in cases {
            let out = flag_spoofed_links(input);
            assert!(
                !out.contains("spoof-warn"),
                "false positive on honest link: input={} out={}",
                input,
                out
            );
        }
    }

    /// An anchor with no `href` must not panic and must not be flagged
    /// (there is nothing to compare against).
    #[test]
    fn anchor_without_href_does_not_crash_or_flag() {
        let input = r#"<a>paypal.com</a>"#;
        let out = flag_spoofed_links(input);
        assert!(!out.contains("spoof-warn"), "should skip hrefless: {}", out);
        assert!(out.contains("paypal.com"));
    }

    /// Anchors whose visible text isn't a domain shouldn't fire even
    /// if the `href` host is arbitrary — there's no claim to spoof.
    #[test]
    fn non_domain_text_is_not_flagged() {
        let input = r#"<a href="https://evil.tld/">click here</a>"#;
        let out = flag_spoofed_links(input);
        assert!(!out.contains("spoof-warn"), "false positive: {}", out);
    }

    /// Relative URLs (`/path`, `#anchor`) and other non-http schemes
    /// have no host to compare; they must pass through unflagged
    /// rather than triggering noisy warnings.
    #[test]
    fn non_http_href_is_skipped() {
        let cases = [
            r#"<a href="/local/path">paypal.com</a>"#,
            r#"<a href="#section">paypal.com</a>"#,
            r#"<a href="mailto:user@paypal.com">paypal.com</a>"#,
            r#"<a href="tel:+1-555-1234">call paypal.com</a>"#,
        ];
        for input in cases {
            let out = flag_spoofed_links(input);
            assert!(
                !out.contains("spoof-warn"),
                "non-http href triggered: input={} out={}",
                input,
                out
            );
        }
    }

    /// Visible text that *is* itself a URL with a different host must
    /// also flag — `https://paypal.com/foo` as text + attacker host
    /// is the most explicit form of the attack.
    #[test]
    fn url_text_with_different_host_is_flagged() {
        let input = r#"<a href="https://attacker.example/">https://paypal.com/login</a>"#;
        let out = flag_spoofed_links(input);
        assert!(out.contains("spoof-warn"), "url-text spoof missed: {}", out);
    }

    /// The visible-text domain may live inside nested formatting tags
    /// (`<strong>`, `<span>`). The tag stripper must surface it so the
    /// comparison still runs.
    #[test]
    fn domain_inside_nested_tags_is_detected() {
        let input = r#"<a href="https://evil.tld/"><strong>paypal.com</strong></a>"#;
        let out = flag_spoofed_links(input);
        assert!(out.contains("spoof-warn"), "nested text missed: {}", out);
    }

    /// Plain dotted numbers (version strings, IPs in copy) must not
    /// be mistaken for domains — the TLD heuristic requires letters.
    #[test]
    fn version_strings_are_not_treated_as_domains() {
        assert!(find_domain_in_text("v1.2.3 release").is_none());
        assert!(find_domain_in_text("call 555.123.4567 today").is_none());
    }

    /// Multiple anchors in one document must each be evaluated
    /// independently — one flagged, one not — without contamination.
    #[test]
    fn multiple_anchors_handled_independently() {
        let input = r#"<p>see <a href="https://paypal.com">paypal.com</a> or \
                       <a href="https://evil.tld">paypal.com</a></p>"#;
        let out = flag_spoofed_links(input);
        let count = out.matches("spoof-warn").count();
        assert_eq!(count, 1, "expected exactly one wrap, got {}: {}", count, out);
    }

    /// Surrounding text outside anchors must round-trip unchanged so
    /// the spoof pass doesn't disturb the rest of the message body.
    #[test]
    fn non_anchor_content_is_untouched() {
        let input = "<p>hello <strong>world</strong></p>";
        assert_eq!(flag_spoofed_links(input), input);
    }

    /// Attribute extraction must not match prefixes of longer attr
    /// names (`data-href=` is not `href=`).
    #[test]
    fn href_extraction_ignores_lookalike_attrs() {
        let tag = r#"<a data-href="x" href="https://evil.tld">paypal.com</a>"#;
        let out = flag_spoofed_links(tag);
        assert!(out.contains("spoof-warn"), "real href missed: {}", out);
        // The actual host in the title should be evil.tld, not x.
        assert!(out.contains("evil.tld"));
    }

    /// `domains_match` collapses subdomains to the registrable-ish
    /// base, so legitimate marketing mail from `mail.paypal.com`
    /// claiming `paypal.com` is treated as honest.
    #[test]
    fn base_domain_collapses_subdomains() {
        assert_eq!(base_domain("mail.paypal.com"), "paypal.com");
        assert_eq!(base_domain("www.paypal.com"), "paypal.com");
        assert_eq!(base_domain("paypal.com"), "paypal.com");
    }
}
