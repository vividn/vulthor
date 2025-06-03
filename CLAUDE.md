# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Vulthor is a modern TUI (Terminal User Interface) email client with integrated HTML display capabilities. The application is designed to:

- Load configuration files specifying local MailDir locations
- Navigate through emails in folders using vim-style key bindings
- Serve HTML versions of selected emails on a specified port (via `-p` flag)
- Support multiple email accounts (future)
- Integrate with notmuch for search functionality (future)

## Architecture Decisions & Future Direction

### Current Architecture
- **State Management**: Currently uses state machine pattern with AppState enum
- **Async/Sync Split**: TUI runs sync, web server runs async with tokio
- **Error Handling**: Uses `Box<dyn Error>` for simplicity
- **Performance**: Lazy loading of emails for fast startup

### Future Architecture Goals
1. **Component-Based State Management**: Migrate from global AppState to independent components with message passing
2. **Full Async**: Long-term goal to unify everything under async for better performance
3. **Custom Error Types**: Migrate to thiserror-based error handling for better error context
4. **Account Management**: Add panel to left of folders for multiple MailDir sources
5. **Search Integration**: Delegate to notmuch for powerful search capabilities

## Development Philosophy

### Ask Clarification Questions
**ALWAYS ask for clarification when**:
- Requirements are ambiguous or incomplete
- Multiple implementation approaches exist and the best path is unclear
- Performance vs functionality tradeoffs need to be made
- User's intent doesn't match technical constraints
- Architecture decisions could impact future features

Don't make assumptions - it's better to ask and get it right the first time.

### Testing Strategy (CRITICAL)
**USE TEST-DRIVEN DEVELOPMENT (TDD)**:
1. **Write tests FIRST** - Before implementing any feature, write tests that define expected behavior
2. **Make tests pass** - Implement minimal code to pass tests
3. **Refactor** - Clean up while keeping tests green
4. **Regression prevention** - If user reports something not working despite passing tests, the test needs updating

Test requirements:
- Tests should be well-documented and easily readable
- Every feature must have corresponding tests
- Focus on behavior, not implementation details
- Use descriptive test names that explain what is being tested

Test utilities:
- Use `test_fixtures.rs` module for creating test data
- Always test with realistic email structures
- Test edge cases (malformed emails, deep folders, etc.)

### Performance Priorities
1. **Startup time must be extremely fast** - Use lazy loading everywhere possible
2. **Basic email info (attachments, from, date) must display instantly**
3. **Optimize for speed over memory usage**
4. **Profile any changes that might affect performance**

### Code Style Preferences

#### Comments
- **File-level comments**: Add architectural decisions at top of files
- **Function comments**: Only when behavior is non-obvious from the name
- **NO redundant comments**: Don't comment what the code clearly shows
- **NO removal comments**: Don't leave "removed X" comments - use git history
- **Large-scale chunking**: OK for complex logic blocks, but keep it meaningful

#### Error Handling (Future)
Currently using `Box<dyn Error>` but will migrate to:
```rust
#[derive(thiserror::Error, Debug)]
enum VulthorError {
    #[error("IO error in {context}: {source}")]
    Io { context: String, #[source] source: io::Error },
    // ... other variants
}
```

## Development Commands

```bash
# Build the project
cargo build

# Run the application
cargo run

# Run with port specification
cargo run -- -p 8080

# Run tests (DO THIS FIRST when implementing features)
cargo test

# Run specific test
cargo test test_name

# Check code formatting
cargo fmt --check

# Apply code formatting
cargo fmt

# Run clippy linter
cargo clippy

# Build for release
cargo build --release
```

## Important Implementation Notes

### When Adding Features
1. Write tests first (TDD approach)
2. Check performance impact, especially on startup
3. Update README.md if user-facing
4. Ensure feature works with future component-based architecture

### When Fixing Bugs
1. Write a test that reproduces the bug
2. Fix the bug
3. Ensure test now passes
4. Check for similar issues elsewhere

### Documentation Maintenance
**ALWAYS update README.md when**:
- Adding new user-facing features
- Changing keybindings
- Modifying configuration options
- Adding new dependencies with user impact

Keep README.md structure:
- User documentation first (installation, usage, configuration)
- Developer documentation at the end
- Clear separation between user and developer content

### Current Module Structure
- `main.rs` - CLI args, main event loop, terminal setup
- `app.rs` - Application state management (will be refactored to components)
- `config.rs` - Configuration file handling
- `email.rs` - Email data structures and parsing
- `maildir.rs` - MailDir filesystem operations
- `ui.rs` - TUI rendering (will be split into components)
- `web.rs` - HTML email server
- `input.rs` - Keyboard input handling
- `test_fixtures.rs` - Test data generation
- `static/styles.css` - Web interface styling
- `assets/` - Application assets (logo, etc.)

### Keybinding Conventions
- Vim-style navigation (j/k/h/l)
- Alt+[key] for pane operations
- Single letters for common actions
- '?' for help (universal convention)

### Performance Considerations
- Email loading is lazy - only load headers initially
- Folder scanning should be fast - limit depth if needed
- Web server uses SSE for efficient updates
- Consider caching parsed emails in memory

## Common Pitfalls to Avoid

1. **Don't load all emails at startup** - Will kill performance
2. **Don't block the TUI thread** - User input must stay responsive
3. **Don't trust email content** - Always sanitize for web display
4. **Don't modify MailDir directly** - Read-only for safety
5. **Don't assume terminal capabilities** - Use crossterm abstractions

## Future Feature Planning

### Notmuch Integration
- Will handle all search functionality
- Don't implement custom search algorithms
- Focus on query interface and result display

### Account Management
- New panel to left of folders
- Each account has separate MailDir
- Unified inbox view across accounts
- Per-account configuration

### Email Operations
- Reply/Forward (future)
- Mark as read/unread
- Delete/Move operations
- Draft composition

## Testing Utilities

Use `test_fixtures.rs` for:
- Creating test MailDir structures
- Generating sample emails
- Testing edge cases (malformed emails, deep folders, etc.)

## Dependencies and Their Purposes

- `ratatui` - TUI framework (may evaluate alternatives for async support)
- `crossterm` - Cross-platform terminal handling
- `tokio` - Async runtime (will expand usage)
- `axum` - Web framework for HTML serving
- `mail-parser` - Robust email parsing
- `walkdir` - Efficient directory traversal
- `clap` - CLI argument parsing
- `serde`/`toml` - Configuration handling

Remember: Optimize for developer velocity while maintaining code quality. When in doubt, write a test!