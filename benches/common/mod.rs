//! Shared helpers for vulthor's criterion benchmarks.
//!
//! Benches are compiled as standalone integration targets, so they
//! cannot reach `src/test_fixtures.rs` (gated to `#[cfg(test)]`). This
//! module re-implements the small subset of fixture generation that the
//! perf suite needs — building synthetic MailDir folders sized for the
//! 100 / 10k regression checks called out in vu-dcg.
//!
//! Every helper writes into a caller-owned `TempDir` and returns the
//! root path so benches stay self-contained and parallel-safe.

#![allow(dead_code)] // helpers are conditionally used per bench file

use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Project-checked-in MailDir fixture (`fixture/maildir/`) used by
/// `bench_full_startup_cold` / `_warm`. Resolved off `CARGO_MANIFEST_DIR`
/// so benches work from any cwd `cargo bench` launches us in.
pub fn project_fixture_maildir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixture/maildir")
}

/// Build a single-folder MailDir under `temp` containing `n` synthetic
/// RFC-822 messages in `cur/`. Returns the maildir root path (the temp
/// dir itself); the folder is named `INBOX`.
///
/// Messages are minimal but include the headers the scanner reads
/// (`From`, `To`, `Subject`, `Date`, `Message-ID`) so `parse_headers_only`
/// runs the same code path as production.
pub fn build_inbox_with_n_messages(temp: &TempDir, n: usize) -> PathBuf {
    let root = temp.path().to_path_buf();
    let cur = root.join("INBOX").join("cur");
    fs::create_dir_all(&cur).expect("create cur");
    fs::create_dir_all(root.join("INBOX/new")).expect("create new");
    fs::create_dir_all(root.join("INBOX/tmp")).expect("create tmp");

    for i in 0..n {
        let body = format!(
            "From: sender{i}@bench.test\r\n\
             To: vulthor@bench.test\r\n\
             Subject: bench message {i}\r\n\
             Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n\
             Message-ID: <{i}@bench.test>\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=UTF-8\r\n\
             \r\n\
             Bench body for message {i}.\r\n",
            i = i
        );
        fs::write(cur.join(format!("{:08}.eml", i)), body).expect("write msg");
    }
    root
}

/// Build a short plain-text RFC-822 message (~1 KB) for the
/// `bench_load_short_body` bench. Returned as bytes so benches can hand
/// them to `mail_parser` without an extra clone.
pub fn build_short_plain_message() -> Vec<u8> {
    let filler = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(15);
    format!(
        "From: alice@bench.test\r\n\
         To: bob@bench.test\r\n\
         Subject: short plain body\r\n\
         Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n\
         Message-ID: <short@bench.test>\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=UTF-8\r\n\
         \r\n\
         {filler}\r\n"
    )
    .into_bytes()
}

/// Build an HTML email body ~50 KB with `remote_img_count` external
/// `<img src="https://…">` tags. The sanitizer rewrites every remote
/// `src` to the inline transparent-pixel placeholder, so this exercises
/// the slowest realistic body-load path.
pub fn build_html_message_with_remote_imgs(remote_img_count: usize) -> Vec<u8> {
    // ~50 KB of body. Each filler block is ~200 bytes; aim for 250 blocks.
    let block = "<p>Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                 Sed do eiusmod tempor incididunt ut labore et dolore magna \
                 aliqua. Ut enim ad minim veniam, quis nostrud exercitation.</p>\n";
    let mut body = String::with_capacity(60_000);
    body.push_str("<html><body>\n");
    for _ in 0..250 {
        body.push_str(block);
    }
    for i in 0..remote_img_count {
        body.push_str(&format!(
            "<img src=\"https://tracker{i}.example.com/pixel.gif\" alt=\"\" />\n"
        ));
    }
    body.push_str("</body></html>\n");

    format!(
        "From: news@bench.test\r\n\
         To: vulthor@bench.test\r\n\
         Subject: html body with remote imgs\r\n\
         Date: Mon, 01 Jan 2024 00:00:00 +0000\r\n\
         Message-ID: <html@bench.test>\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: text/html; charset=UTF-8\r\n\
         \r\n\
         {body}"
    )
    .into_bytes()
}

/// Helper: bench file paths inside `dir` (one walkdir pass). Used by
/// body-load benches that want to point at the pre-built short message.
pub fn write_message_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, bytes).expect("write bench message");
    p
}
