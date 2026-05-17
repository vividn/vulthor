//! Body-load benches (vu-dcg).
//!
//! `bench_load_short_body` exercises the common case (small plain-text
//! reply) end-to-end: read the message from disk, parse via
//! `mail_parser`, populate the `Email`'s body fields.
//!
//! `bench_load_html_body_with_remote_img` exercises the worst-case
//! render-pane path: a ~50 KB HTML body with 10 remote `<img>` tags
//! that the sanitizer must each rewrite to the inline placeholder.

mod common;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::fs;
use tempfile::TempDir;
use vulthor::email::Email;
use vulthor::sanitizer::sanitize_email_html;

use common::{build_html_message_with_remote_imgs, build_short_plain_message, write_message_file};

fn bench_load_short_body(c: &mut Criterion) {
    let temp = TempDir::new().expect("tempdir");
    let path = write_message_file(temp.path(), "short.eml", &build_short_plain_message());

    c.bench_function("load_short_body", |b| {
        b.iter(|| {
            let mut email = Email::new(path.clone());
            email.parse_from_file().expect("parse");
            black_box(email.body_plain.as_deref().map(|s| s.len()).unwrap_or(0));
        });
    });
}

fn bench_load_html_body_with_remote_img(c: &mut Criterion) {
    let temp = TempDir::new().expect("tempdir");
    let raw = build_html_message_with_remote_imgs(10);
    let path = write_message_file(temp.path(), "html.eml", &raw);

    // Sanity: confirm fixture is the size we advertised. Off-by-an-order
    // is the kind of thing that silently breaks perf comparisons.
    let on_disk = fs::metadata(&path).expect("stat").len();
    assert!(
        on_disk > 30_000 && on_disk < 80_000,
        "html bench fixture is {} bytes, expected ~50 KB",
        on_disk,
    );

    c.bench_function("load_html_body_with_remote_img", |b| {
        b.iter(|| {
            let mut email = Email::new(path.clone());
            email.parse_from_file().expect("parse");
            // `parse_from_file` already runs the body through the
            // sanitizer for HTML parts, but exercise the sanitizer
            // directly too — the web pane re-sanitizes any string it
            // serves, and a regression in either path is a regression
            // in the user-visible render-pane latency.
            if let Some(html) = email.body_html.as_deref() {
                black_box(sanitize_email_html(html));
            }
        });
    });
}

criterion_group!(
    benches,
    bench_load_short_body,
    bench_load_html_body_with_remote_img,
);
criterion_main!(benches);
