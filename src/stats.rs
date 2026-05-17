//! `vulthor stats` — per-account MailDir summary.
//!
//! Walks each configured account's MailDir (or the legacy top-level
//! `maildir_path` when no `[accounts.*]` table is set) and reports
//! totals the user is most likely to want from a CLI one-shot:
//! message count, unread count, on-disk size, oldest/newest message
//! dates, and the top-5 senders by message count. Read-only — never
//! mutates the MailDir.
//!
//! Pairs with `vulthor doctor` (runtime health). Both subcommands fork
//! out of `main.rs` before any TUI / web / scanner state is set up so
//! they never race with the live runtime.

use crate::config::Config;
use chrono::{DateTime, Utc};
use mail_parser::MessageParser;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// One row of the stats report — one per configured account, or a
/// single synthetic `"(default)"` row when the legacy `maildir_path`
/// is the only configured source. All fields are pre-aggregated so the
/// renderer / JSON writer is dumb.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatLine {
    /// TOML key of the `[accounts.<key>]` section, or `"(default)"`
    /// when falling back to the top-level `maildir_path`.
    pub account_id: String,
    /// Human-facing display name for the account.
    pub account_name: String,
    /// Resolved MailDir root walked to produce this row.
    pub maildir_path: PathBuf,
    /// Count of files under any `cur/` or `new/` (skipping `tmp/` and
    /// hidden entries). One row per message.
    pub total_messages: usize,
    /// Subset of `total_messages` living under `new/` (Maildir
    /// convention: not-yet-`S`-flagged).
    pub unread_count: usize,
    /// Sum of file sizes for every counted message, in bytes. Excludes
    /// `tmp/` and directory overhead — this is the answer to "how big
    /// is my mail on disk" within an order of magnitude.
    pub total_size_bytes: u64,
    /// Earliest RFC-3339 `Date:` header found across counted messages,
    /// or `None` when the MailDir is empty / no dates parsed.
    pub oldest: Option<String>,
    /// Latest RFC-3339 `Date:` header found across counted messages.
    pub newest: Option<String>,
    /// Top-5 senders by message count, in descending-count order
    /// (ties broken alphabetically). Each entry is the formatted
    /// `From:` string (`"Name <addr>"`, `"addr"`, or `"Unknown"`).
    /// May be shorter than 5 when fewer distinct senders exist.
    pub top_senders: Vec<SenderCount>,
}

/// `(sender, count)` tuple as a struct so the JSON shape stays stable
/// (`{"sender": ..., "count": ...}`) and so downstream consumers don't
/// need to track positional fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SenderCount {
    /// Formatted `From:` string, e.g. `"Alice <a@b.tld>"`.
    pub sender: String,
    /// Number of messages with this `From:` value across the account.
    pub count: usize,
}

/// Walk every configured account and return one [`StatLine`] each.
///
/// Iteration order matches `Config::ordered_accounts` (alphabetical by
/// TOML key) so two runs over the same config produce byte-identical
/// output. When no `[accounts.*]` tables are configured, returns a
/// single synthetic `"(default)"` row backed by `config.maildir_path`
/// — this keeps single-account / pre-Phase-4.a installs working.
pub fn run_stats(config: &Config) -> Vec<StatLine> {
    if config.accounts.is_empty() {
        return vec![collect_account(
            "(default)".to_string(),
            "(default)".to_string(),
            &config.maildir_path,
        )];
    }
    config
        .ordered_accounts()
        .into_iter()
        .map(|(id, acct)| collect_account(id, acct.name, &acct.maildir_path))
        .collect()
}

/// Print a colorless aligned table to stdout. Format is intentionally
/// boring and pipeline-friendly: one header per account, then a small
/// labeled block. Sender list is indented two spaces.
pub fn print_human(lines: &[StatLine]) {
    if lines.is_empty() {
        println!("(no accounts configured)");
        return;
    }
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!(
            "account: {} ({})  [{}]",
            line.account_name,
            line.account_id,
            line.maildir_path.display(),
        );
        println!("  messages    : {}", line.total_messages);
        println!("  unread      : {}", line.unread_count);
        println!("  size        : {}", format_bytes(line.total_size_bytes));
        println!("  oldest      : {}", line.oldest.as_deref().unwrap_or("-"),);
        println!("  newest      : {}", line.newest.as_deref().unwrap_or("-"),);
        if line.top_senders.is_empty() {
            println!("  top senders : -");
        } else {
            println!("  top senders :");
            for s in &line.top_senders {
                println!("    {:>5}  {}", s.count, s.sender);
            }
        }
    }
}

/// Print the stat lines as a pretty-printed JSON array. Round-trips
/// through `serde_json` — the test suite parses this back into
/// `Vec<StatLine>` to prove the contract.
pub fn print_json(lines: &[StatLine]) {
    // unwrap is safe: StatLine derives Serialize over only owned
    // strings / numbers / Vec, none of which can fail to serialize.
    let s = serde_json::to_string_pretty(lines).expect("StatLine serialization is infallible");
    println!("{}", s);
}

/// Format `bytes` with a binary IEC suffix (`KiB`, `MiB`, `GiB`).
/// Used by the human renderer only — the JSON path emits raw u64.
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[unit])
    }
}

/// Walk `root` once, accumulating counts/sizes/dates/senders. Missing
/// or non-directory roots return a zeroed row — never panic — so a
/// half-configured account doesn't kill the whole report.
fn collect_account(account_id: String, account_name: String, root: &Path) -> StatLine {
    let mut total = 0usize;
    let mut unread = 0usize;
    let mut size = 0u64;
    let mut oldest: Option<DateTime<Utc>> = None;
    let mut newest: Option<DateTime<Utc>> = None;
    let mut senders: HashMap<String, usize> = HashMap::new();

    if root.is_dir() {
        let walker = WalkDir::new(root).into_iter().filter_entry(|entry| {
            // Don't filter the root itself — depth() == 0 is the seed.
            if entry.depth() == 0 {
                return true;
            }
            let name = match entry.file_name().to_str() {
                Some(n) => n,
                None => return false,
            };
            // Skip hidden entries (`.notmuch`, `.beads`, etc.) and the
            // `tmp/` staging dir at any depth — `tmp/` holds in-flight
            // deliveries that aren't real messages yet.
            if name.starts_with('.') {
                return false;
            }
            if entry.file_type().is_dir() && name == "tmp" {
                return false;
            }
            true
        });

        for entry in walker.flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            // Only count files whose immediate parent is `cur/` or
            // `new/` — anything else is debris (config files,
            // .notmuch indexes that slipped a filter, etc.).
            let parent_name = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str());
            let is_new = match parent_name {
                Some("cur") => false,
                Some("new") => true,
                _ => continue,
            };
            // Skip MailDir transient artifacts.
            let fname = entry.file_name().to_str().unwrap_or("");
            if fname.starts_with('.') || fname.ends_with(".lock") || fname.ends_with(".tmp") {
                continue;
            }

            total += 1;
            if is_new {
                unread += 1;
            }
            if let Ok(meta) = entry.metadata() {
                size = size.saturating_add(meta.len());
            }

            // Parse headers only — full body parse would dominate
            // wall-clock on large maildirs. Failures degrade silently
            // so a single broken message doesn't poison the report.
            if let Ok(content) = fs::read(path)
                && let Some(message) = MessageParser::default().parse(&content)
            {
                if let Some(date) = message.date()
                    && let Ok(parsed) = DateTime::parse_from_rfc3339(&date.to_rfc3339())
                        .map(|d| d.with_timezone(&Utc))
                {
                    oldest = Some(match oldest {
                        Some(prev) if prev <= parsed => prev,
                        _ => parsed,
                    });
                    newest = Some(match newest {
                        Some(prev) if prev >= parsed => prev,
                        _ => parsed,
                    });
                }
                let sender = message
                    .from()
                    .and_then(|addr| addr.first())
                    .map(|a| match (a.name(), a.address()) {
                        (Some(name), Some(email)) => format!("{} <{}>", name, email),
                        (None, Some(email)) => email.to_string(),
                        (Some(name), None) => name.to_string(),
                        _ => "Unknown".to_string(),
                    })
                    .unwrap_or_else(|| "Unknown".to_string());
                *senders.entry(sender).or_insert(0) += 1;
            }
        }
    }

    StatLine {
        account_id,
        account_name,
        maildir_path: root.to_path_buf(),
        total_messages: total,
        unread_count: unread,
        total_size_bytes: size,
        oldest: oldest.map(|d| d.to_rfc3339()),
        newest: newest.map(|d| d.to_rfc3339()),
        top_senders: top_n_senders(senders, 5),
    }
}

/// Pick the top `n` `(sender, count)` pairs by descending count, with
/// alphabetical tie-break so output is deterministic across runs.
fn top_n_senders(counts: HashMap<String, usize>, n: usize) -> Vec<SenderCount> {
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.into_iter()
        .take(n)
        .map(|(sender, count)| SenderCount { sender, count })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccountConfig;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    /// Build a minimal RFC-822 message body for the fixture.
    fn make_msg(from_name: &str, from_addr: &str, date: &str, subject: &str) -> String {
        format!(
            "From: {} <{}>\r\nTo: u@x.tld\r\nSubject: {}\r\nDate: {}\r\nMessage-ID: <{}@x.tld>\r\n\r\nbody\r\n",
            from_name, from_addr, subject, date, subject,
        )
    }

    fn make_maildir(root: &Path, folder: &str) {
        for sub in ["cur", "new", "tmp"] {
            fs::create_dir_all(root.join(folder).join(sub)).unwrap();
        }
    }

    /// Acceptance: a populated fixture maildir returns counts, sizes,
    /// dates, and the top-sender list we'd expect. Single-account
    /// path (legacy `maildir_path`, no `[accounts.*]` tables).
    #[test]
    fn run_stats_counts_messages_and_top_senders() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_maildir(root, "INBOX");
        make_maildir(root, "Work");

        // Three from Alice in INBOX/cur, two from Bob in INBOX/new
        // (so unread = 2), one from Alice in Work/cur. Total = 6.
        fs::write(
            root.join("INBOX/cur/1"),
            make_msg("Alice", "a@x.tld", "Mon, 01 Jan 2024 12:00:00 +0000", "s1"),
        )
        .unwrap();
        fs::write(
            root.join("INBOX/cur/2"),
            make_msg("Alice", "a@x.tld", "Tue, 02 Jan 2024 12:00:00 +0000", "s2"),
        )
        .unwrap();
        fs::write(
            root.join("INBOX/cur/3"),
            make_msg("Alice", "a@x.tld", "Wed, 03 Jan 2024 12:00:00 +0000", "s3"),
        )
        .unwrap();
        fs::write(
            root.join("INBOX/new/4"),
            make_msg("Bob", "b@x.tld", "Thu, 04 Jan 2024 12:00:00 +0000", "s4"),
        )
        .unwrap();
        fs::write(
            root.join("INBOX/new/5"),
            make_msg("Bob", "b@x.tld", "Fri, 05 Jan 2024 12:00:00 +0000", "s5"),
        )
        .unwrap();
        fs::write(
            root.join("Work/cur/6"),
            make_msg("Alice", "a@x.tld", "Sat, 06 Jan 2024 12:00:00 +0000", "s6"),
        )
        .unwrap();

        let cfg = Config {
            maildir_path: root.to_path_buf(),
            ..Config::default()
        };
        let lines = run_stats(&cfg);
        assert_eq!(lines.len(), 1, "single synthetic account row");
        let line = &lines[0];
        assert_eq!(line.account_id, "(default)");
        assert_eq!(line.total_messages, 6);
        assert_eq!(line.unread_count, 2);
        assert!(line.total_size_bytes > 0);
        // Dates parse to RFC-3339; oldest is Jan 1, newest is Jan 6.
        assert!(line.oldest.as_deref().unwrap().starts_with("2024-01-01"));
        assert!(line.newest.as_deref().unwrap().starts_with("2024-01-06"));
        // Top-2: Alice (4), Bob (2). Truncated list, alphabetical
        // tie-break would matter only on equal counts.
        assert_eq!(line.top_senders.len(), 2);
        assert_eq!(line.top_senders[0].sender, "Alice <a@x.tld>");
        assert_eq!(line.top_senders[0].count, 4);
        assert_eq!(line.top_senders[1].sender, "Bob <b@x.tld>");
        assert_eq!(line.top_senders[1].count, 2);
    }

    /// Acceptance: empty maildir returns a zero-counts row without
    /// panic. Covers both "directory exists but holds nothing" and the
    /// downstream renderers/JSON path.
    #[test]
    fn run_stats_empty_maildir_returns_zeroes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_maildir(root, "INBOX");

        let cfg = Config {
            maildir_path: root.to_path_buf(),
            ..Config::default()
        };
        let lines = run_stats(&cfg);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].total_messages, 0);
        assert_eq!(lines[0].unread_count, 0);
        assert_eq!(lines[0].total_size_bytes, 0);
        assert!(lines[0].oldest.is_none());
        assert!(lines[0].newest.is_none());
        assert!(lines[0].top_senders.is_empty());
    }

    /// Missing maildir path → zeroed row, never panics. The single-
    /// account user who points `maildir_path` at a not-yet-created
    /// dir should still get a usable report.
    #[test]
    fn run_stats_missing_path_is_silent() {
        let cfg = Config {
            maildir_path: PathBuf::from("/__vulthor_stats_missing__/mail"),
            ..Config::default()
        };
        let lines = run_stats(&cfg);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].total_messages, 0);
    }

    /// Acceptance: `--json` output round-trips through `serde_json`.
    /// Proves the on-disk shape is stable and re-parseable, which is
    /// the only contract scripted consumers can rely on.
    #[test]
    fn json_output_round_trips() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_maildir(root, "INBOX");
        fs::write(
            root.join("INBOX/cur/1"),
            make_msg("Alice", "a@x.tld", "Mon, 01 Jan 2024 12:00:00 +0000", "s1"),
        )
        .unwrap();

        let cfg = Config {
            maildir_path: root.to_path_buf(),
            ..Config::default()
        };
        let lines = run_stats(&cfg);
        let json = serde_json::to_string_pretty(&lines).expect("serialize");
        let reparsed: Vec<StatLine> = serde_json::from_str(&json).expect("parse");
        assert_eq!(reparsed, lines);
    }

    /// Multi-account: each `[accounts.*]` table gets its own row, in
    /// alphabetical key order (mirrors `ordered_accounts`).
    #[test]
    fn multi_account_emits_one_row_per_account_in_alphabetical_order() {
        let tmp = TempDir::new().unwrap();
        let work_root = tmp.path().join("work");
        let personal_root = tmp.path().join("personal");
        make_maildir(&work_root, "INBOX");
        make_maildir(&personal_root, "INBOX");
        fs::write(
            work_root.join("INBOX/cur/1"),
            make_msg("W", "w@x.tld", "Mon, 01 Jan 2024 12:00:00 +0000", "s"),
        )
        .unwrap();
        fs::write(
            personal_root.join("INBOX/cur/1"),
            make_msg("P", "p@x.tld", "Mon, 01 Jan 2024 12:00:00 +0000", "s"),
        )
        .unwrap();
        fs::write(
            personal_root.join("INBOX/cur/2"),
            make_msg("P", "p@x.tld", "Mon, 01 Jan 2024 12:00:00 +0000", "s"),
        )
        .unwrap();

        let mut accounts = BTreeMap::new();
        accounts.insert(
            "work".to_string(),
            AccountConfig {
                name: "Work".into(),
                email: "w@x.tld".into(),
                maildir_path: work_root.clone(),
                smtp_command: None,
                signature: None,
            },
        );
        accounts.insert(
            "personal".to_string(),
            AccountConfig {
                name: "Personal".into(),
                email: "p@x.tld".into(),
                maildir_path: personal_root.clone(),
                smtp_command: None,
                signature: None,
            },
        );
        let cfg = Config {
            accounts,
            ..Config::default()
        };

        let lines = run_stats(&cfg);
        assert_eq!(lines.len(), 2);
        // BTreeMap iteration → alphabetical: personal before work.
        assert_eq!(lines[0].account_id, "personal");
        assert_eq!(lines[0].total_messages, 2);
        assert_eq!(lines[1].account_id, "work");
        assert_eq!(lines[1].total_messages, 1);
    }

    /// `tmp/` subdirectories and hidden entries must not be counted —
    /// `tmp/` holds in-flight deliveries; `.notmuch` etc. are indexes.
    #[test]
    fn tmp_and_hidden_dirs_are_skipped() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        make_maildir(root, "INBOX");
        fs::write(
            root.join("INBOX/cur/real"),
            make_msg("R", "r@x.tld", "Mon, 01 Jan 2024 12:00:00 +0000", "s"),
        )
        .unwrap();
        // `tmp/` should be ignored entirely.
        fs::write(
            root.join("INBOX/tmp/in-flight"),
            make_msg("X", "x@x.tld", "Mon, 01 Jan 2024 12:00:00 +0000", "s"),
        )
        .unwrap();
        // Hidden index — ignored.
        fs::create_dir_all(root.join(".notmuch")).unwrap();
        fs::write(root.join(".notmuch/index"), b"not-an-email").unwrap();

        let cfg = Config {
            maildir_path: root.to_path_buf(),
            ..Config::default()
        };
        let lines = run_stats(&cfg);
        assert_eq!(lines[0].total_messages, 1);
        assert_eq!(lines[0].top_senders.len(), 1);
        assert_eq!(lines[0].top_senders[0].sender, "R <r@x.tld>");
    }
}
