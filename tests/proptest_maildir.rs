//! Property-based fuzz tests for the MailDir filename parser (vu-wb0).
//!
//! Maildir filenames carry info flags in a `:2,FLAGS` suffix. Random
//! filenames — including weird suffixes, non-ASCII, missing `:2,`, etc.
//! — must never panic the flag detector. This file also fuzzes the
//! `MaildirScanner` over tempdirs populated with junk filenames to make
//! sure folder-scan path handling is panic-free.

use std::fs;
use std::path::PathBuf;

use proptest::prelude::*;
use tempfile::TempDir;
use vulthor::email::maildir_flag_in_filename;
use vulthor::maildir::MaildirScanner;

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 512,
        .. ProptestConfig::default()
    })]

    /// Arbitrary printable filename strings (including odd flag suffixes
    /// like `S:2,RT`, `:2,`, no suffix at all, lone colons) must never
    /// panic the flag detector. We sample the flag char from a wide set
    /// to catch lookups that assume ASCII-only.
    #[test]
    fn maildir_filename_parser_robust(
        name in "[a-zA-Z0-9._,:!@#$%^&*()=+\\- ]{0,128}",
        flag in any::<char>(),
    ) {
        let path = PathBuf::from(name);
        let _ = maildir_flag_in_filename(&path, flag);
    }

    /// Same property, but over fully random unicode filenames. Tests
    /// that the non-UTF-8 / odd-path safe-defaults paths hold.
    #[test]
    fn maildir_filename_parser_robust_unicode(
        name in ".{0,128}",
        flag in any::<char>(),
    ) {
        let path = PathBuf::from(name);
        let _ = maildir_flag_in_filename(&path, flag);
    }
}

/// Strategy emitting plausibly-shaped maildir filenames: a unique-id
/// segment plus an optional `:2,FLAGS` suffix.
fn maildir_filename() -> impl Strategy<Value = String> {
    ("[a-zA-Z0-9.]{1,32}", prop::option::of("[A-Za-z]{0,8}")).prop_map(
        |(stem, flags)| match flags {
            Some(f) => format!("{stem}:2,{f}"),
            None => stem,
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        .. ProptestConfig::default()
    })]

    /// Build a tempdir maildir (`cur/`, `new/`, `tmp/`) populated with
    /// random filenames, scan it, and assert no panic. Empty body files
    /// — the scanner only reads headers in `load_more_folder_emails`,
    /// not in this top-level `scan`, but exercising scan() guards the
    /// directory-walk path against odd entry names.
    #[test]
    fn scanner_handles_random_filenames(names in proptest::collection::vec(maildir_filename(), 0..8)) {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path().join("Inbox");
        fs::create_dir_all(root.join("cur")).expect("cur");
        fs::create_dir_all(root.join("new")).expect("new");
        fs::create_dir_all(root.join("tmp")).expect("tmp");
        for name in &names {
            // Sanitize: no path separators inside the filename component.
            if name.contains('/') || name.is_empty() {
                continue;
            }
            let _ = fs::write(root.join("cur").join(name), b"");
        }
        let scanner = MaildirScanner::new(dir.path().to_path_buf());
        let _ = scanner.scan();
    }
}
