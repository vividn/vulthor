# Changelog

All notable changes to Vulthor are documented in this file.

The format is based on [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Component-based TUI architecture (Accounts, Folders, Messages, Content, Draft).
- Multi-account support with per-account MailDir configuration.
- Read, reply, forward, archive, delete, move, and flag actions.
- Session-only undo stack for reversible message actions.
- Drafts surfaced via `In-Reply-To` chips in the message list.
- HTML email rendering pane served over a local web view.
- `msmtp` send pipeline for composed and replied messages.
- `vulthor.toml` configuration covering accounts, keybindings, theme, AI, and web settings.
- GitHub Actions CI pipeline running fmt, clippy, and tests on every push.
