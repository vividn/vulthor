//! Loads every fixture in `tests/fixtures/maildir/` and asserts the
//! expected parsed shape. Doubles as a smoke test that the fixtures
//! themselves are valid RFC5322 + Maildir — adding a malformed one
//! here will fail fast.

use std::path::PathBuf;

use vulthor::email::Email;

fn fixture_dir(rel: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures/maildir");
    p.push(rel);
    p
}

fn fixture(rel: &str) -> Email {
    let path = fixture_dir(rel);
    let mut email = Email::new(path);
    email
        .parse_from_file()
        .expect("fixture parses as RFC5322 email");
    email
}

#[test]
fn plain_text_fixture_populates_body_plain_only() {
    let email = fixture("Inbox/cur/01-plain-text.eml:2,S");
    assert!(
        email.body_plain.as_deref().is_some_and(|b| b.contains("Alice")),
        "plain fixture must populate body_plain",
    );
    assert!(
        email.body_html.is_none(),
        "plain fixture must not populate body_html",
    );
    assert!(email.attachments.is_empty());
    assert!(email.inline_images.is_empty());
}

#[test]
fn html_only_fixture_populates_body_html_only() {
    let email = fixture("Inbox/cur/02-html-only.eml:2,S");
    assert!(email.body_plain.is_none(), "html-only must not have plain");
    assert!(
        email
            .body_html
            .as_deref()
            .is_some_and(|b| b.contains("HTML-only message")),
        "html-only fixture must populate body_html",
    );
}

#[test]
fn multipart_alternative_fixture_populates_both_bodies() {
    let email = fixture("Inbox/cur/03-multipart-alternative.eml:2,S");
    assert!(
        email
            .body_plain
            .as_deref()
            .is_some_and(|b| b.contains("two flavors")),
        "alt fixture must populate body_plain",
    );
    assert!(
        email
            .body_html
            .as_deref()
            .is_some_and(|b| b.contains("two flavors")),
        "alt fixture must populate body_html",
    );
}

#[test]
fn attachment_fixture_lists_the_attachment_separately() {
    let email = fixture("Inbox/cur/04-with-attachment.eml:2,S");
    assert!(
        email.body_plain.as_deref().is_some_and(|b| b.contains("Dave")),
        "attachment fixture's text part must populate body_plain",
    );
    assert_eq!(
        email.attachments.len(),
        1,
        "attachment fixture must surface exactly one attachment, got {:?}",
        email
            .attachments
            .iter()
            .map(|a| &a.filename)
            .collect::<Vec<_>>(),
    );
    assert_eq!(email.attachments[0].filename, "report.pdf");
    assert_eq!(email.attachments[0].content_type, "application/pdf");
}

#[test]
fn phishing_fixture_loads_and_carries_mismatched_link() {
    let email = fixture("Inbox/new/05-phishing-link.eml:2,");
    let html = email.body_html.as_deref().expect("phishing fixture is HTML");
    // The sanitized output retains the visible text "paypal.com" but the
    // href points at attacker.evil.test — vu-6yi's link-check pass is
    // what flags the mismatch downstream.
    assert!(html.contains("paypal.com"));
    assert!(html.contains("attacker.evil.test"));
}

#[test]
fn multipart_related_fixture_preserves_inline_image() {
    let email = fixture("Inbox/cur/06-multipart-related.eml:2,S");
    assert_eq!(
        email.inline_images.len(),
        1,
        "related fixture must preserve its single inline image",
    );
    assert_eq!(email.inline_images[0].content_id, "pixel@v.test");
    assert_eq!(email.inline_images[0].content_type, "image/png");
    assert!(
        email.attachments.is_empty(),
        "inline-image parts must not pollute the attachment list",
    );
}

#[test]
fn large_body_fixture_parses_and_keeps_full_text() {
    let email = fixture("Inbox/new/07-large-body.eml:2,");
    let plain = email
        .body_plain
        .as_deref()
        .expect("large fixture must populate body_plain");
    // The fixture has ~80 repeated lines; assert we round-tripped them.
    assert!(
        plain.matches("Lorem ipsum").count() >= 50,
        "large-body fixture must preserve the full repeated body",
    );
}
