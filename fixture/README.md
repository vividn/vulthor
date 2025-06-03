# Test MailDir Fixture

This directory contains a sample MailDir structure for testing Vulthor without using production email data.

## Structure

```
maildir/
├── INBOX/           # Main inbox with example emails
├── Sent/            # Sent messages
├── Drafts/          # Draft emails
├── Archive/         # Archived emails with year subfolders
│   ├── 2023/
│   └── 2024/
├── Work/            # Work-related emails
│   └── Projects/    # Project-specific emails
└── Personal/        # Personal emails
```

## Usage

There are three ways to run Vulthor with the test MailDir:

### 1. Direct command with -m flag
```bash
cargo run -- -m ./fixture/maildir
```

### 2. Shell script (creates temporary copy)
```bash
./run-test-maildir.sh
```
This script creates a temporary copy of the fixture maildir and automatically cleans it up when you exit.

### 3. Rust binary (creates temporary copy)
```bash
cargo run --bin test-maildir
```
This also creates a temporary copy and provides the cleanest integration.

You can pass additional arguments to either script:
```bash
./run-test-maildir.sh -p 9000  # Use port 9000
cargo run --bin test-maildir -- -p 9000  # Same with Rust binary
```

## Email Examples

The fixture includes various types of emails:
- Multipart HTML/plain text emails
- Emails with attachments (PDF)
- Reply chains with In-Reply-To headers
- Emails with various flags (Read, Seen, Flagged, Draft)
- Unicode content and subjects
- Different date ranges for testing sorting

## Adding More Test Emails

To add more test emails, create files in the appropriate `cur/` or `new/` directories following the MailDir naming convention:
```
{timestamp}.M{microseconds}P{process}.{hostname}:2,{flags}
```

Common flags:
- S = Seen
- R = Replied
- F = Flagged
- D = Draft
- T = Trashed