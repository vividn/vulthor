//! Startup-path benches (vu-dcg).
//!
//! Measures the work vulthor performs between `main()` and the first
//! interactive TUI frame: build the [`MaildirScanner`], walk the
//! directory tree, hydrate the [`EmailStore`], page-load the auto-
//! selected INBOX, and build the drafts index. These three benches
//! exist so a regression in any of those legs shows up at PR time
//! against a checked-in fixture.
//!
//! Note: criterion measures wall-clock per iteration. The "cold" vs
//! "warm" split is OS-cache-relative — criterion's warmup will hot-load
//! the fixture into the page cache before measurement, so both numbers
//! reflect already-cached I/O. The split still surfaces work duplication:
//! the cold bench rebuilds the scanner + store from scratch each iter,
//! while the warm bench re-runs only the cheap "already loaded" path.

mod common;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::sync::{Arc, Mutex};
use vulthor::email::EmailStore;
use vulthor::maildir::MaildirScanner;

use common::{build_inbox_with_n_messages, project_fixture_maildir};
use tempfile::TempDir;

const INBOX_LOAD_LIMIT: usize = 50;

/// Cold path: from-scratch scanner + store every iteration, against the
/// project's checked-in `fixture/maildir/`. Mirrors the work `main.rs`
/// does between `Config::load` and the first `terminal.draw` call.
fn bench_full_startup_cold(c: &mut Criterion) {
    let maildir_path = project_fixture_maildir();
    assert!(
        maildir_path.exists(),
        "missing fixture maildir at {}",
        maildir_path.display(),
    );

    c.bench_function("full_startup_cold", |b| {
        b.iter(|| {
            let scanner = MaildirScanner::new(maildir_path.clone());
            let mut root = scanner.scan().expect("scan");
            // First child is the auto-INBOX in our fixture; lazy-load it.
            if let Some(folder) = root.subfolders.first_mut() {
                scanner
                    .load_folder_emails_with_limit(folder, Some(INBOX_LOAD_LIMIT))
                    .expect("inbox load");
            }
            let drafts = scanner.build_drafts_index();
            let store = EmailStore::new(maildir_path.clone());
            black_box((root, drafts, store));
        });
    });
}

/// Warm path: pre-build the scanner + store + drafts index once, then
/// re-issue the "ensure folder loaded" call per iteration. The
/// scanner's `load_more_folder_emails` / `load_folder_emails_with_limit`
/// short-circuit on already-loaded folders, so this isolates that
/// fast-path cost from the full cold rebuild above.
fn bench_full_startup_warm(c: &mut Criterion) {
    let maildir_path = project_fixture_maildir();
    let scanner = MaildirScanner::new(maildir_path.clone());
    let mut root = scanner.scan().expect("scan");
    if let Some(folder) = root.subfolders.first_mut() {
        scanner
            .load_folder_emails_with_limit(folder, None)
            .expect("warm load");
    }
    let drafts = scanner.build_drafts_index();
    let store = Arc::new(Mutex::new(EmailStore::new(maildir_path.clone())));

    c.bench_function("full_startup_warm", |b| {
        b.iter(|| {
            // Hot path the TUI hits on every focus-change / scroll:
            // ask the scanner to load more and observe the no-op.
            let mut root_clone = root.clone();
            if let Some(folder) = root_clone.subfolders.first_mut() {
                let added = scanner
                    .load_more_folder_emails(folder, INBOX_LOAD_LIMIT)
                    .expect("warm load more");
                black_box(added);
            }
            black_box(&store);
            black_box(&drafts);
        });
    });
}

/// First-paint bench: time from a synthetic "binary start" snapshot to
/// the data being ready for the first ratatui frame. We can't trigger
/// the real `terminal.draw` without owning stdout (AppRoot::render is
/// hardcoded to `CrosstermBackend<Stdout>`), so this benches the work
/// that gates first paint: scanner + scan + INBOX page-load.
///
/// Uses a synthetic 100-message INBOX so the number reflects a realistic
/// daily-driver folder size rather than the 10-message project fixture.
fn bench_first_paint(c: &mut Criterion) {
    let temp = TempDir::new().expect("tempdir");
    let maildir_path = build_inbox_with_n_messages(&temp, 100);

    c.bench_function("first_paint", |b| {
        b.iter(|| {
            let scanner = MaildirScanner::new(maildir_path.clone());
            let mut root = scanner.scan().expect("scan");
            if let Some(folder) = root.subfolders.first_mut() {
                scanner
                    .load_folder_emails_with_limit(folder, Some(INBOX_LOAD_LIMIT))
                    .expect("inbox load");
            }
            black_box(root);
        });
    });
}

criterion_group!(
    benches,
    bench_full_startup_cold,
    bench_full_startup_warm,
    bench_first_paint,
);
criterion_main!(benches);
