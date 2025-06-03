# 📧 Vulthor

A modern TUI (Terminal User Interface) email client with integrated HTML display, built in Rust.

Vulthor provides a powerful vim-style email reading experience in your terminal while simultaneously serving beautiful HTML versions of your emails in a web browser.

## ✨ Features

### 🖥️ Terminal Interface
- **3-pane layout**: Folders, email list, and content view
- **Vim-style navigation**: Use `j`/`k` to navigate, `Alt+h`/`Alt+l` to switch panes
- **Collapsible panes**: Toggle folder pane (`Alt+e`) and content pane (`Alt+c`)
- **Email list**: Shows unread indicators (📧) and attachment icons (📎)
- **Content viewing**: Markdown rendering with scrollable content
- **Attachment support**: View attachment list with `Alt+a`
- **Help system**: Press `?` for keybinding reference

### 🌐 Web Interface
- **Real-time sync**: Web view updates as you navigate in the terminal
- **Beautiful styling**: Professional gradient design with responsive layout
- **HTML email support**: Proper rendering of HTML emails with attachments
- **Mobile friendly**: Responsive design works on all screen sizes
- **Welcome page**: Helpful keybinding reference when no email is selected

### 📁 Email Support
- **MailDir format**: Full support for standard MailDir directory structure
- **Email parsing**: Handles multipart messages, attachments, and various encodings
- **Header extraction**: From, To, Subject, Date, Message-ID fields
- **HTML to Markdown**: Converts HTML emails to readable markdown in terminal
- **Attachment detection**: Identifies and displays file attachments with metadata

## 🚀 Quick Start

### Installation
```bash
git clone https://github.com/yourusername/vulthor.git
cd vulthor
cargo build --release
```

### Basic Usage
```bash
# Start with default settings (serves on port 8080)
cargo run

# Specify a custom port
cargo run -- -p 3000

# Use a specific config file
cargo run -- -c /path/to/config.toml
```

### Configuration

Vulthor looks for configuration in these locations (in order):
1. Path specified with `-c` flag
2. `~/.config/vulthor/config.toml`
3. `./vulthor.toml`
4. Default: `~/Mail`

Example configuration file:
```toml
# ~/.config/vulthor/config.toml
maildir_path = "/home/user/Mail"
```

## ⌨️ Keybindings

### Navigation
- `j` / `k` - Move up/down in current pane
- `Alt+h` / `Alt+l` - Switch between panes
- `Enter` - Select folder or email
- `Backspace` - Go back to parent folder

### Pane Control
- `Alt+e` - Toggle folder pane visibility
- `Alt+c` - Toggle content pane visibility

### Email Actions
- `Alt+a` - View attachments (when email has attachments)

### General
- `?` - Show help screen
- `q` - Quit application

## 🏗️ Architecture

Vulthor is built with modern Rust technologies:

- **[ratatui](https://ratatui.rs/)** - Terminal user interface framework
- **[crossterm](https://github.com/crossterm-rs/crossterm)** - Cross-platform terminal manipulation
- **[tokio](https://tokio.rs/)** - Async runtime for concurrent TUI and web server
- **[axum](https://github.com/tokio-rs/axum)** - Web framework for HTML email serving
- **[mail-parser](https://docs.rs/mail-parser)** - Email parsing and multipart handling

### Project Structure
```
vulthor/
├── src/
│   ├── main.rs         # CLI args, main event loop
│   ├── config.rs       # Configuration handling  
│   ├── email.rs        # Email data structures
│   ├── app.rs          # Application state management
│   ├── maildir.rs      # MailDir scanning and parsing
│   ├── ui.rs           # TUI 3-pane interface
│   ├── input.rs        # Vim-style input handling
│   └── web.rs          # Axum web server
├── static/
│   └── styles.css      # Responsive web styling
└── README.md
```

## 🧪 Developer Guide

### Getting Started with Development

1. **Clone and setup**:
   ```bash
   git clone https://github.com/yourusername/vulthor.git
   cd vulthor
   cargo build
   ```

2. **Running in development**:
   ```bash
   # Run with auto-reload (requires cargo-watch)
   cargo watch -x run
   
   # Run with debug logging
   RUST_LOG=debug cargo run
   ```

### Testing Philosophy

Vulthor follows a **Test-Driven Development (TDD)** approach:

1. **Write tests first** - Before implementing a feature, write tests that define its behavior
2. **Make tests pass** - Implement the minimal code needed to pass the tests
3. **Refactor** - Clean up the implementation while keeping tests green
4. **Regression prevention** - If a bug is found, write a test that catches it before fixing

```bash
# Run all tests
cargo test

# Run tests with output
cargo test -- --nocapture

# Run specific test
cargo test test_folder_navigation

# Run tests continuously
cargo watch -x test
```

### Code Quality Standards

```bash
# Format code (required before commits)
cargo fmt

# Run linter with suggestions
cargo clippy -- -W clippy::pedantic

# Check for common mistakes
cargo check

# Run all quality checks
cargo fmt && cargo clippy && cargo test
```

### Architecture Overview

Vulthor is transitioning to a **component-based architecture**:

- **Components**: Each UI pane (folders, list, content) will be an independent component
- **Message Passing**: Components communicate through events rather than shared state
- **Async-first**: Moving towards full async to eliminate blocking operations
- **Performance**: Prioritizing startup time and basic email info display speed

### Performance Profiling

```bash
# Build with profiling
cargo build --release --features profiling

# Run with performance tracking
cargo run --release -- --profile

# Benchmark critical paths
cargo bench
```

### Contributing Guidelines

1. **Check existing issues** before starting work
2. **Write tests first** following TDD principles
3. **Keep commits atomic** - one logical change per commit
4. **Update documentation** - especially README.md for user-facing changes
5. **Run quality checks** - `cargo fmt && cargo clippy && cargo test`
6. **Performance matters** - profile changes that might affect startup time

### Common Development Tasks

#### Adding a New Keybinding
1. Write test in `src/input.rs` defining the expected behavior
2. Add the key handler in the match statement
3. Update the help screen in `src/ui.rs`
4. Document in README.md keybindings section

#### Adding a New UI Component
1. Create component struct implementing a common trait
2. Add message types for component communication
3. Write comprehensive tests for component behavior
4. Integrate with existing layout system

#### Debugging Tips
- Use `dbg!()` macro for quick debugging
- Enable `RUST_LOG=trace` for detailed logs
- Use `cargo expand` to see macro expansions
- Profile with `cargo flamegraph` for performance issues

### Release Process

1. Update version in `Cargo.toml`
2. Run full test suite: `cargo test --release`
3. Update CHANGELOG.md
4. Tag release: `git tag -a v0.x.x -m "Release version 0.x.x"`
5. Build releases: `cargo build --release --target x86_64-unknown-linux-gnu`

## 📋 Requirements

- **Rust 1.70+** with Cargo (nightly recommended for edition 2024)
- **MailDir-compatible email storage** (Thunderbird, mutt, etc.)
- **Terminal** supporting modern features (most terminals work)
- **Web browser** for HTML email viewing

## 🔧 MailDir Setup

Vulthor works with any MailDir-compatible email setup. Popular options:

### Thunderbird
Thunderbird stores emails in MailDir format by default on Linux. Point Vulthor to your Thunderbird profile's Mail directory.

### mutt + offlineimap/mbsync
```bash
# Example offlineimap config snippet
[Repository Remote]
type = IMAP
# ... your IMAP settings

[Repository Local]  
type = Maildir
localfolders = ~/Mail
```

### Manual MailDir Structure
```
Mail/
├── INBOX/
│   ├── cur/     # Read emails
│   ├── new/     # Unread emails  
│   └── tmp/     # Temporary files
├── Sent/
│   ├── cur/
│   ├── new/
│   └── tmp/
└── ...
```

## 🤝 Contributing

Contributions are welcome! Please feel free to submit issues, feature requests, or pull requests.

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🙏 Acknowledgments

- Inspired by classic email clients like mutt and alpine
- Built on the excellent Rust ecosystem
- Thanks to the ratatui community for the amazing TUI framework
