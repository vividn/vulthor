//! Folder-scan benches (vu-dcg).
//!
//! Two synthetic MailDir folders (100 and 10k messages) measure the
//! cost of `MaildirScanner::scan` + a full header-only load. The 10k
//! variant is the upper bound called out in the
//! `feedback.daydumb-bench-size` memory — full daily-driver scale
//! without ballooning bench wall time.

mod common;

use criterion::{Criterion, Throughput, black_box, criterion_group, criterion_main};
use tempfile::TempDir;
use vulthor::maildir::MaildirScanner;

use common::build_inbox_with_n_messages;

fn bench_scan_n(c: &mut Criterion, group_name: &str, n: usize) {
    // One temp dir per bench function; reused across iterations so we
    // measure scan cost, not fixture-generation cost.
    let temp = TempDir::new().expect("tempdir");
    let root = build_inbox_with_n_messages(&temp, n);

    let mut group = c.benchmark_group(group_name);
    group.throughput(Throughput::Elements(n as u64));
    group.bench_function("scan_and_load_all", |b| {
        b.iter(|| {
            let scanner = MaildirScanner::new(root.clone());
            let mut tree = scanner.scan().expect("scan");
            let inbox = tree.subfolders.first_mut().expect("inbox subfolder");
            scanner
                .load_folder_emails_with_limit(inbox, None)
                .expect("full load");
            black_box(inbox.emails.len());
        });
    });
    group.finish();
}

fn bench_scan_100_messages(c: &mut Criterion) {
    bench_scan_n(c, "folder_scan_100", 100);
}

fn bench_scan_10k_messages(c: &mut Criterion) {
    bench_scan_n(c, "folder_scan_10k", 10_000);
}

criterion_group!(benches, bench_scan_100_messages, bench_scan_10k_messages);
criterion_main!(benches);
