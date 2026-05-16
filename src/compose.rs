// Compose / send pipeline — Phase 2.a.
//
// `Compose` is the in-memory message under construction. `launch_editor`
// drops the user into `$EDITOR` with a header/body template and parses
// the result. `serialize_rfc822` produces a wire-format RFC 5322
// message. `send` pipes the wire format to the account's configured
// SMTP command (typically `msmtp -a <name>`) and, on success, files a
// copy under `<maildir>/Sent/cur/`.
//
// Suspend/restore of the TUI around the editor invocation is the
// CALLER's responsibility — `launch_editor` only knows about files
// and child processes.

// Phase 2.b (pre-send pane UI) and 2.d (reply variants) are the
// consumers of this module. Until they land, the public surface is
// only exercised by tests.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::AccountConfig;
use crate::error::{Result, VulthorError};

/// Trailing newline appended to bodies when serializing, so the file
/// always ends with a newline (RFC 5322 allows but does not require).
const BODY_TRAILER: &str = "\n";

/// Source-of-truth domain for synthesized Message-IDs when we can't
/// determine a real hostname. RFC 5322 requires the right-hand side to
/// be a valid domain literal; `vulthor.local` is reserved-style and
/// will not collide with public DNS.
const MESSAGE_ID_DOMAIN: &str = "vulthor.local";

/// Process-wide counter that makes Message-IDs and Sent filenames
/// distinct even when two calls happen within the same microsecond.
static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// An outgoing message under construction. Fields mirror the user's
/// editing surface plus enough context for serialization.
///
/// `from` is not in the original VISION.md field list but is needed by
/// `serialize_rfc822` — the From header has to come from somewhere and
/// the caller is the one who knows the active account.
///
/// `attachments` is captured but NOT yet emitted by `serialize_rfc822`
/// (would require MIME multipart, out of Phase 2.a scope).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compose {
    pub from: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub attachments: Vec<PathBuf>,
    pub signature: Option<String>,
}

impl Compose {
    /// Empty compose with no fields populated. Useful as a starting
    /// point in tests and reply-builders (Phase 2.d).
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the compose into a wire-format RFC 5322 message. The
    /// returned string includes a trailing newline.
    ///
    /// Headers emitted: Date, Message-ID, From, To, [Cc], [Bcc],
    /// Subject, [In-Reply-To, References], MIME-Version,
    /// Content-Type, Content-Transfer-Encoding.
    pub fn serialize_rfc822(&self) -> String {
        let mut out = String::new();

        out.push_str(&format!("Date: {}\r\n", current_rfc2822_date()));
        out.push_str(&format!("Message-ID: <{}>\r\n", new_message_id()));
        out.push_str(&format!("From: {}\r\n", self.from));
        out.push_str(&format!("To: {}\r\n", self.to));
        if !self.cc.is_empty() {
            out.push_str(&format!("Cc: {}\r\n", self.cc));
        }
        if !self.bcc.is_empty() {
            out.push_str(&format!("Bcc: {}\r\n", self.bcc));
        }
        out.push_str(&format!("Subject: {}\r\n", self.subject));
        if let Some(irt) = &self.in_reply_to {
            out.push_str(&format!("In-Reply-To: {}\r\n", irt));
            // Simple References chain — a single parent. Threaded
            // multi-hop References will land with reply variants (2.d).
            out.push_str(&format!("References: {}\r\n", irt));
        }
        out.push_str("MIME-Version: 1.0\r\n");
        out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
        out.push_str("Content-Transfer-Encoding: 8bit\r\n");
        out.push_str("\r\n");
        out.push_str(&self.body);
        if !self.body.ends_with('\n') {
            out.push_str(BODY_TRAILER);
        }

        out
    }
}

/// Build the editor template a fresh draft starts from. Header lines
/// (with empty values) above the blank separator; the body, then the
/// signature, below it. Callers building reply templates (Phase 2.d)
/// will use their own template builders that pre-populate
/// In-Reply-To and quoted body.
pub fn default_template(compose: &Compose) -> String {
    let irt = compose.in_reply_to.as_deref().unwrap_or("");
    let mut t = String::new();
    t.push_str(&format!("To: {}\n", compose.to));
    t.push_str(&format!("Cc: {}\n", compose.cc));
    t.push_str(&format!("Bcc: {}\n", compose.bcc));
    t.push_str(&format!("Subject: {}\n", compose.subject));
    t.push_str(&format!("In-Reply-To: {}\n", irt));
    t.push('\n');
    t.push_str(&compose.body);
    if let Some(sig) = &compose.signature
        && !sig.is_empty()
    {
        if !compose.body.ends_with('\n') {
            t.push('\n');
        }
        t.push_str("-- \n");
        t.push_str(sig);
    }
    t
}

/// Parse the editor result back into a `Compose`. Recognizes the same
/// header lines emitted by `default_template`. Lines before the first
/// blank line are headers; everything after is body (verbatim).
///
/// Unknown header lines are ignored (forward-compatible). A missing
/// blank-line separator means the entire text is treated as headers
/// — that yields an empty body, which is a recoverable user error.
pub fn parse_compose_from_text(text: &str) -> Result<Compose> {
    let (headers, body) = match text.find("\n\n") {
        Some(idx) => (&text[..idx], &text[idx + 2..]),
        // No blank line: treat the whole thing as headers. Body stays
        // empty. The caller may surface this as a draft validation
        // error; we don't reject it here.
        None => (text, ""),
    };

    let mut compose = Compose::new();
    compose.body = body.to_string();

    for line in headers.lines() {
        // Defensive: ignore blank lines inside the header block (e.g.,
        // CRLF artifacts from some editors).
        if line.trim().is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(VulthorError::ComposeParseFailed(format!(
                "header line missing ':' separator: {:?}",
                line
            )));
        };
        let value = value.trim().to_string();
        match name.trim().to_ascii_lowercase().as_str() {
            "to" => compose.to = value,
            "cc" => compose.cc = value,
            "bcc" => compose.bcc = value,
            "subject" => compose.subject = value,
            "in-reply-to" => {
                compose.in_reply_to = if value.is_empty() { None } else { Some(value) };
            }
            _ => { /* ignore unknown headers */ }
        }
    }

    Ok(compose)
}

/// Drop the user into `$EDITOR` with `template` pre-loaded and parse
/// the result. Resolution order: `$EDITOR`, `$VISUAL`, then `vi`. The
/// caller MUST suspend the TUI (LeaveAlternateScreen, disable raw
/// mode) before calling and restore afterward; this function inherits
/// stdio so the editor takes over the terminal directly.
pub fn launch_editor(template: &str) -> Result<Compose> {
    let editor = resolve_editor();
    launch_editor_with(template, &editor)
}

/// Test seam for `launch_editor`. Splits out env-var resolution so
/// tests can pass an explicit editor binary without touching the
/// process-wide environment (which would race with parallel tests).
pub(crate) fn launch_editor_with(template: &str, editor: &str) -> Result<Compose> {
    let tempfile = tempfile::NamedTempFile::new()
        .map_err(|e| VulthorError::ComposeEditorFailed(format!("tempfile: {}", e)))?;
    let path = tempfile.path().to_path_buf();

    std::fs::write(&path, template)
        .map_err(|e| VulthorError::ComposeEditorFailed(format!("write template: {}", e)))?;

    // `sh -c "<editor> <quoted-path>"` lets users put `vim -c '…'` or
    // other flag-bearing strings into $EDITOR. The path is single-
    // quoted with embedded single-quotes escaped.
    let cmd = format!("{} {}", editor, shell_quote(&path));
    let status = Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .status()
        .map_err(|e| VulthorError::ComposeEditorFailed(format!("spawn '{}': {}", editor, e)))?;

    if !status.success() {
        return Err(VulthorError::ComposeEditorFailed(format!(
            "editor exited with status {:?}",
            status.code()
        )));
    }

    let edited = std::fs::read_to_string(&path)
        .map_err(|e| VulthorError::ComposeEditorFailed(format!("read back: {}", e)))?;

    parse_compose_from_text(&edited)
}

/// Pipe the RFC 5322 representation of `compose` to the account's
/// SMTP command, then file a copy in `<maildir>/Sent/cur/`. Returns
/// the Sent path on success.
///
/// On SMTP failure the Sent copy is NOT written, so the user's draft
/// is preserved upstream (the caller still owns the `Compose`).
pub fn send(compose: &Compose, account: &AccountConfig) -> Result<PathBuf> {
    let smtp_cmd = resolve_smtp_command(account);

    let rfc822 = compose.serialize_rfc822();

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&smtp_cmd)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| VulthorError::SendFailed(format!("spawn '{}': {}", smtp_cmd, e)))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| VulthorError::SendFailed("stdin not piped".to_string()))?;
        stdin
            .write_all(rfc822.as_bytes())
            .map_err(|e| VulthorError::SendFailed(format!("write to stdin: {}", e)))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| VulthorError::SendFailed(format!("wait: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(VulthorError::SendFailed(format!(
            "{} exited with status {:?}: {}",
            smtp_cmd,
            output.status.code(),
            stderr.trim()
        )));
    }

    write_to_sent(&account.maildir_path, &rfc822)
}

/// Write a successfully-sent message into `<maildir>/Sent/cur/` with
/// a standard MailDir-2 filename. `S` (seen) is set because outgoing
/// mail isn't unread.
fn write_to_sent(maildir_root: &Path, rfc822: &str) -> Result<PathBuf> {
    let sent_dir = maildir_root.join("Sent").join("cur");
    std::fs::create_dir_all(&sent_dir).map_err(|e| VulthorError::SentFolderWriteFailed {
        path: sent_dir.clone(),
        source: e,
    })?;

    let filename = maildir_filename();
    let path = sent_dir.join(filename);
    std::fs::write(&path, rfc822).map_err(|e| VulthorError::SentFolderWriteFailed {
        path: path.clone(),
        source: e,
    })?;
    Ok(path)
}

/// SMTP command for `account` — `smtp_command` if set, otherwise the
/// synthesized default `msmtp -a <account.name>`.
pub fn resolve_smtp_command(account: &AccountConfig) -> String {
    account
        .smtp_command
        .clone()
        .unwrap_or_else(|| format!("msmtp -a {}", account.name))
}

fn resolve_editor() -> String {
    std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string())
}

fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    // Single-quote, escaping any embedded single-quotes by closing
    // the quote, inserting `\'`, and reopening.
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn current_rfc2822_date() -> String {
    chrono::Utc::now().to_rfc2822()
}

fn new_message_id() -> String {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("{}.{}.{}@{}", micros, pid, counter, MESSAGE_ID_DOMAIN)
}

fn maildir_filename() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let micros = now.subsec_micros();
    let pid = std::process::id();
    let counter = UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}.M{}P{}Q{}.vulthor:2,S", secs, micros, pid, counter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn account(smtp: Option<&str>, maildir: PathBuf) -> AccountConfig {
        AccountConfig {
            name: "test".to_string(),
            email: "tester@example.com".to_string(),
            maildir_path: maildir,
            smtp_command: smtp.map(String::from),
            signature: None,
        }
    }

    // ---- parse_compose_from_text ----

    #[test]
    fn parse_extracts_headers_and_body() {
        let text = "To: alice@example.com\nCc: bob@example.com\nBcc: \nSubject: hi\nIn-Reply-To: \n\nhello world\n";
        let c = parse_compose_from_text(text).unwrap();
        assert_eq!(c.to, "alice@example.com");
        assert_eq!(c.cc, "bob@example.com");
        assert_eq!(c.bcc, "");
        assert_eq!(c.subject, "hi");
        assert!(c.in_reply_to.is_none());
        assert_eq!(c.body, "hello world\n");
    }

    #[test]
    fn parse_preserves_multi_line_body_verbatim() {
        let text = "To: a@b\nSubject: s\n\nline one\nline two\n\nline four\n";
        let c = parse_compose_from_text(text).unwrap();
        assert_eq!(c.body, "line one\nline two\n\nline four\n");
    }

    #[test]
    fn parse_captures_in_reply_to_when_present() {
        let text = "To: a@b\nSubject: re: hi\nIn-Reply-To: <abc@host>\n\nbody\n";
        let c = parse_compose_from_text(text).unwrap();
        assert_eq!(c.in_reply_to.as_deref(), Some("<abc@host>"));
    }

    #[test]
    fn parse_ignores_unknown_headers() {
        let text = "To: a@b\nX-Mailer: vulthor\nSubject: s\n\nbody\n";
        let c = parse_compose_from_text(text).unwrap();
        assert_eq!(c.to, "a@b");
        assert_eq!(c.subject, "s");
    }

    #[test]
    fn parse_fails_on_header_line_without_colon() {
        let text = "To a@b\n\nbody";
        let err = parse_compose_from_text(text).unwrap_err();
        assert!(matches!(err, VulthorError::ComposeParseFailed(_)));
    }

    #[test]
    fn parse_is_case_insensitive_for_header_names() {
        let text = "to: a@b\nSUBJECT: hi\n\nbody\n";
        let c = parse_compose_from_text(text).unwrap();
        assert_eq!(c.to, "a@b");
        assert_eq!(c.subject, "hi");
    }

    // ---- serialize_rfc822 ----

    #[test]
    fn serialize_emits_required_headers() {
        let c = Compose {
            from: "Tester <t@example.com>".into(),
            to: "alice@example.com".into(),
            subject: "hi".into(),
            body: "hello\n".into(),
            ..Compose::new()
        };
        let s = c.serialize_rfc822();
        assert!(s.contains("Date: "), "missing Date header: {}", s);
        assert!(s.contains("Message-ID: <"), "missing Message-ID: {}", s);
        assert!(s.contains("From: Tester <t@example.com>"));
        assert!(s.contains("To: alice@example.com"));
        assert!(s.contains("Subject: hi"));
        assert!(s.contains("MIME-Version: 1.0"));
        // Headers separated from body by a blank CRLF line.
        assert!(s.contains("\r\n\r\nhello\n"));
    }

    #[test]
    fn serialize_emits_in_reply_to_and_references_together() {
        let c = Compose {
            from: "t@example.com".into(),
            to: "a@b".into(),
            subject: "re: hi".into(),
            body: "ok".into(),
            in_reply_to: Some("<parent@host>".into()),
            ..Compose::new()
        };
        let s = c.serialize_rfc822();
        assert!(s.contains("In-Reply-To: <parent@host>"));
        assert!(s.contains("References: <parent@host>"));
    }

    #[test]
    fn serialize_omits_in_reply_to_when_none() {
        let c = Compose {
            from: "t@e".into(),
            to: "a@b".into(),
            subject: "s".into(),
            body: "ok".into(),
            ..Compose::new()
        };
        let s = c.serialize_rfc822();
        assert!(!s.contains("In-Reply-To:"));
        assert!(!s.contains("References:"));
    }

    #[test]
    fn serialize_omits_empty_cc_and_bcc() {
        let c = Compose {
            from: "t@e".into(),
            to: "a@b".into(),
            subject: "s".into(),
            body: "ok".into(),
            ..Compose::new()
        };
        let s = c.serialize_rfc822();
        assert!(!s.contains("Cc:"));
        assert!(!s.contains("Bcc:"));
    }

    #[test]
    fn serialize_round_trips_through_mail_parser() {
        // The wire format must parse back as a valid email. This is the
        // contract that protects us from accidental header malformation.
        let c = Compose {
            from: "Tester <t@example.com>".into(),
            to: "alice@example.com".into(),
            cc: "bob@example.com".into(),
            subject: "Round trip".into(),
            body: "Hello, world.\n".into(),
            in_reply_to: Some("<parent@host>".into()),
            ..Compose::new()
        };
        let s = c.serialize_rfc822();
        let parsed = mail_parser::MessageParser::default()
            .parse(s.as_bytes())
            .expect("must parse");
        assert_eq!(parsed.subject(), Some("Round trip"));
        assert_eq!(
            parsed.in_reply_to().as_text_list().unwrap()[0],
            "parent@host"
        );
        // Body comes back without the headers.
        assert_eq!(parsed.body_text(0).as_deref(), Some("Hello, world.\n"));
    }

    // ---- launch_editor ----

    #[test]
    fn launch_editor_with_true_returns_unmodified_template() {
        // `true` is the canonical no-op binary: it ignores its args,
        // doesn't touch the file, and exits 0. So the parser sees the
        // template we wrote in.
        let template = "To: alice@example.com\nSubject: hi\n\nbody\n";
        let c = launch_editor_with(template, "true").expect("editor ok");
        assert_eq!(c.to, "alice@example.com");
        assert_eq!(c.subject, "hi");
        assert_eq!(c.body, "body\n");
    }

    #[test]
    fn launch_editor_propagates_editor_failure() {
        // `false` exits non-zero — we must surface that, not silently
        // parse whatever's in the file.
        let err = launch_editor_with("To: x\n\nbody", "false").unwrap_err();
        assert!(matches!(err, VulthorError::ComposeEditorFailed(_)));
    }

    #[test]
    fn launch_editor_handles_missing_editor_binary() {
        // Resolves to spawn failure, not a panic.
        let err = launch_editor_with("To: x\n\nbody", "/no/such/editor_binary_xyz").unwrap_err();
        assert!(matches!(err, VulthorError::ComposeEditorFailed(_)));
    }

    // ---- send ----

    /// Build a stub `msmtp` script at `dir/msmtp` that captures stdin
    /// to `dir/captured.eml`. Returns the path to the captured file.
    fn stub_msmtp(dir: &Path, exit_code: i32) -> PathBuf {
        let script_path = dir.join("msmtp");
        let captured = dir.join("captured.eml");
        let script = format!(
            "#!/bin/sh\ncat > '{}'\nexit {}\n",
            captured.display(),
            exit_code,
        );
        std::fs::write(&script_path, script).unwrap();
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();
        captured
    }

    #[test]
    fn send_pipes_rfc822_into_smtp_command_and_writes_sent_copy() {
        let tmp = TempDir::new().unwrap();
        let captured = stub_msmtp(tmp.path(), 0);

        // Point `smtp_command` directly at the stub by absolute path —
        // avoids mutating the process-wide PATH (which would race with
        // parallel tests).
        let stub = tmp.path().join("msmtp");
        let maildir = tmp.path().join("Mail");
        std::fs::create_dir_all(&maildir).unwrap();
        let acct = account(
            Some(&format!("{} -a test", stub.display())),
            maildir.clone(),
        );

        let c = Compose {
            from: "t@example.com".into(),
            to: "a@b".into(),
            subject: "hi".into(),
            body: "hello\n".into(),
            ..Compose::new()
        };

        let sent_path = send(&c, &acct).expect("send ok");

        // The stub captured the exact bytes we piped in.
        let captured_bytes = std::fs::read_to_string(&captured).unwrap();
        assert!(captured_bytes.contains("Subject: hi"));
        assert!(captured_bytes.contains("\r\n\r\nhello\n"));

        // Sent copy landed under <maildir>/Sent/cur/.
        let sent_dir = maildir.join("Sent").join("cur");
        assert!(sent_path.starts_with(&sent_dir), "{:?}", sent_path);
        let on_disk = std::fs::read_to_string(&sent_path).unwrap();
        assert_eq!(on_disk, captured_bytes);
    }

    #[test]
    fn send_surfaces_smtp_failure_and_skips_sent_copy() {
        let tmp = TempDir::new().unwrap();
        let _captured = stub_msmtp(tmp.path(), 1);
        let stub = tmp.path().join("msmtp");
        let maildir = tmp.path().join("Mail");
        let acct = account(
            Some(&format!("{} -a test", stub.display())),
            maildir.clone(),
        );

        let c = Compose {
            from: "t@e".into(),
            to: "a@b".into(),
            subject: "s".into(),
            body: "x".into(),
            ..Compose::new()
        };

        let err = send(&c, &acct).unwrap_err();
        assert!(matches!(err, VulthorError::SendFailed(_)));

        // No Sent folder created on failure — the user's draft is the
        // only surviving copy, owned by the caller.
        assert!(
            !maildir.join("Sent").exists(),
            "Sent folder should not exist when SMTP fails"
        );
    }

    #[test]
    fn resolve_smtp_command_uses_explicit_value_when_set() {
        let acct = account(Some("/usr/bin/sendmail -t"), PathBuf::from("/tmp"));
        assert_eq!(resolve_smtp_command(&acct), "/usr/bin/sendmail -t");
    }

    #[test]
    fn resolve_smtp_command_synthesizes_msmtp_default_from_account_name() {
        // When smtp_command is None, we synthesize `msmtp -a <name>`.
        // Asserted on the pure helper so the test doesn't depend on
        // whether `msmtp` happens to be installed on the test host.
        let acct = account(None, PathBuf::from("/tmp"));
        assert_eq!(resolve_smtp_command(&acct), "msmtp -a test");
    }

    // ---- helpers ----

    #[test]
    fn default_template_round_trips_through_parser() {
        let c = Compose {
            to: "a@b".into(),
            cc: "c@d".into(),
            subject: "hi".into(),
            body: "hello\n".into(),
            in_reply_to: Some("<p@h>".into()),
            ..Compose::new()
        };
        let template = default_template(&c);
        let back = parse_compose_from_text(&template).unwrap();
        assert_eq!(back.to, "a@b");
        assert_eq!(back.cc, "c@d");
        assert_eq!(back.subject, "hi");
        assert_eq!(back.in_reply_to.as_deref(), Some("<p@h>"));
        assert_eq!(back.body, "hello\n");
    }

    #[test]
    fn default_template_appends_signature_block() {
        let c = Compose {
            to: "a@b".into(),
            subject: "hi".into(),
            body: "hello".into(),
            signature: Some("Tester".into()),
            ..Compose::new()
        };
        let template = default_template(&c);
        assert!(template.contains("\n-- \nTester"), "got: {}", template);
    }

    #[test]
    fn maildir_filename_is_unique_across_calls() {
        let a = maildir_filename();
        let b = maildir_filename();
        assert_ne!(a, b);
        assert!(a.ends_with(":2,S"));
    }

    #[test]
    fn new_message_id_is_unique_across_calls() {
        let a = new_message_id();
        let b = new_message_id();
        assert_ne!(a, b);
        assert!(a.ends_with(&format!("@{}", MESSAGE_ID_DOMAIN)));
    }
}
