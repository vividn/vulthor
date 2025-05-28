# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Vulthor is a simple TUI (Terminal User Interface) email client with HTML display capabilities. The application is designed to:

- Load configuration files specifying local MailDir locations
- Navigate through emails in folders using vim-style key bindings
- Serve HTML versions of selected emails on a specified port (via `-p` flag)

## Development Commands

```bash
# Build the project
cargo build

# Run the application
cargo run

# Run with port specification (planned feature)
cargo run -- -p 8080

# Run tests
cargo test

# Check code formatting
cargo fmt --check

# Apply code formatting
cargo fmt

# Run clippy linter
cargo clippy

# Build for release
cargo build --release
```

## Architecture

This is an early-stage Rust project using Cargo edition 2024. The main application entry point is in `src/main.rs`. The project is structured as a single binary crate with plans to implement:

1. **Configuration handling** - Loading MailDir specifications from config files
2. **TUI interface** - Terminal-based email navigation with vim key bindings
3. **HTML server** - Port-based serving of email content in HTML format
4. **Email parsing** - MailDir format handling and email content processing

The project currently has no external dependencies but will likely need crates for TUI (e.g., `ratatui`), email parsing (e.g., `mailparse`), and HTTP serving (e.g., `axum` or `warp`) as development progresses.