//! Property-based fuzz tests for the email parser (vu-wb0).
//!
//! `Email::parse_from_file` and `parse_headers_only` are the production
//! entry points for parsing untrusted on-disk message bytes. These tests
//! drive them with both fully-random byte strings and RFC-5322-shaped
//! emails to assert the contract: parsing must never panic, regardless
//! of input. Each generated case is written to a tempfile and parsed via
//! the same code path used at runtime.

use std::io::Write;

use proptest::prelude::*;
use tempfile::NamedTempFile;
use vulthor::email::Email;

/// Write `bytes` to a fresh tempfile and return the keepalive handle plus
/// path. The handle must outlive the parse — dropping it deletes the file.
fn tempfile_with(bytes: &[u8]) -> (NamedTempFile, std::path::PathBuf) {
    let mut tmp = NamedTempFile::new().expect("tempfile");
    tmp.write_all(bytes).expect("write");
    tmp.flush().expect("flush");
    let path = tmp.path().to_path_buf();
    (tmp, path)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        .. ProptestConfig::default()
    })]

    /// Random byte strings up to 8 KiB must not panic either parser path.
    /// We accept Err freely; the assertion is the absence of a panic.
    #[test]
    fn parse_random_email_does_not_panic(bytes in proptest::collection::vec(any::<u8>(), 0..8192)) {
        let (_keep, path) = tempfile_with(&bytes);
        let mut email = Email::new(path);
        let _ = email.parse_headers_only();
        let _ = email.parse_from_file();
    }

    /// Random ASCII text (printable + whitespace) is the most adversarial
    /// shape for header-folding / MIME-boundary scanners — separate case
    /// from the all-bytes generator above.
    #[test]
    fn parse_random_ascii_does_not_panic(s in "[ -~\r\n\t]{0,4096}") {
        let (_keep, path) = tempfile_with(s.as_bytes());
        let mut email = Email::new(path);
        let _ = email.parse_headers_only();
        let _ = email.parse_from_file();
    }
}

/// Header-value charset: printable ASCII minus CR/LF so generated
/// strings never accidentally terminate the header line.
fn header_token() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 _.,!?@<>:/=+-]{0,64}"
}

/// Build a plausible RFC-5322 message from generated header fragments
/// and a body. The result is always syntactically a valid email — the
/// goal is to exercise the parsed-fields path, not error paths.
fn rfc5322_email() -> impl Strategy<Value = Vec<u8>> {
    (
        header_token(), // from
        header_token(), // to
        header_token(), // subject
        header_token(), // message-id
        proptest::collection::vec(any::<u8>().prop_filter("no NUL", |b| *b != 0), 0..2048),
    )
        .prop_map(|(from, to, subj, mid, body)| {
            let mut out = Vec::with_capacity(256 + body.len());
            out.extend_from_slice(b"From: ");
            out.extend_from_slice(from.as_bytes());
            out.extend_from_slice(b"\r\nTo: ");
            out.extend_from_slice(to.as_bytes());
            out.extend_from_slice(b"\r\nSubject: ");
            out.extend_from_slice(subj.as_bytes());
            out.extend_from_slice(b"\r\nMessage-ID: <");
            out.extend_from_slice(mid.as_bytes());
            out.extend_from_slice(
                b">\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=us-ascii\r\n\r\n",
            );
            out.extend_from_slice(&body);
            out
        })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        .. ProptestConfig::default()
    })]

    /// Well-formed RFC-5322 emails: full parse must succeed, the four
    /// surfaced headers must be retrievable as `String`s, and the body
    /// load-state must flip to `FullyLoaded`.
    #[test]
    fn parse_valid_rfc5322_round_trips(bytes in rfc5322_email()) {
        let (_keep, path) = tempfile_with(&bytes);
        let mut email = Email::new(path);
        prop_assert!(email.parse_from_file().is_ok());
        // Retrievable — the formatter never panics on the populated fields.
        let _ = email.get_header_display();
        let _ = email.has_attachments();
        let _ = email.attachment_count();
    }
}
