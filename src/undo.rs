// Session-only undo stack.
//
// `Mutation` records each reversible action the user takes (mark-read,
// archive, delete, move, toggle-star, mark-unread). The action-key
// handlers push mutations onto `AppRoot.undo_stack` after performing
// their filesystem op; the `u` key pops the stack and calls
// `Mutation::reverse`, which either renames the file back to `from` or
// flips the Maildir `F` flag.
//
// `reverse` is best-effort by design (VISION.md "Undo"): if the file
// has been rewritten by `mbsync` or otherwise vanished from its
// post-action location, we return `Skipped::FileMoved` and `AppRoot`
// surfaces "Could not undo: file moved" via the status line. The stack
// is in-memory only and discarded at quit.

use std::fs;
use std::path::{Path, PathBuf};

/// One reversible user action. `msg` is the file path at the time the
/// action completed; `from`/`to` capture the pre- and post-action
/// locations for path-move mutations. `ToggleStar.prev_flag` is the
/// `F`-flag state *before* the toggle so undo can restore it directly.
///
/// Variants `MarkRead`, `MarkUnread`, and `ToggleStar` are not yet
/// constructed in production code — the action-key handlers will
/// produce them. Keeping them here so the undo surface stays one whole
/// shape rather than dribbling in per key.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mutation {
    MarkRead {
        msg: PathBuf,
        from: PathBuf,
        to: PathBuf,
    },
    Archive {
        msg: PathBuf,
        from: PathBuf,
        to: PathBuf,
    },
    Delete {
        msg: PathBuf,
        from: PathBuf,
        to: PathBuf,
    },
    Move {
        msg: PathBuf,
        from: PathBuf,
        to: PathBuf,
    },
    ToggleStar {
        msg: PathBuf,
        prev_flag: bool,
    },
    MarkUnread {
        msg: PathBuf,
        from: PathBuf,
        to: PathBuf,
    },
}

/// Result of a best-effort `reverse`. Path-move variants succeed with
/// the restored path; flag-flip variants succeed with the renamed file
/// path (Maildir bakes flags into the filename). `Skipped` means we
/// could not safely undo (file vanished, mbsync rewrote it, etc.) and
/// the caller should show the status without panicking.
#[derive(Debug)]
pub enum Reversed {
    /// File was renamed back to its pre-action location.
    PathRestored { old: PathBuf, new: PathBuf },
    /// Maildir `F` flag was flipped; the file path may have changed.
    FlagRestored { old: PathBuf, new: PathBuf },
    /// Could not undo (file moved/deleted by something else).
    Skipped,
}

impl Mutation {
    /// Best-effort reversal. See module docs for the "file moved"
    /// contract — never panics, never returns I/O errors to the caller.
    pub fn reverse(&self) -> Reversed {
        match self {
            Mutation::MarkRead { to, from, .. }
            | Mutation::Archive { to, from, .. }
            | Mutation::Delete { to, from, .. }
            | Mutation::Move { to, from, .. }
            | Mutation::MarkUnread { to, from, .. } => move_back(to, from),
            Mutation::ToggleStar { msg, prev_flag } => flip_flag_f(msg, *prev_flag),
        }
    }
}

fn move_back(to: &Path, from: &Path) -> Reversed {
    if !to.exists() {
        return Reversed::Skipped;
    }
    if let Some(parent) = from.parent() {
        // Best-effort: `mbsync` will have created the parent already.
        // If creating it fails (permission, ENOSPC) the rename below
        // surfaces the same error and we report Skipped.
        let _ = fs::create_dir_all(parent);
    }
    match fs::rename(to, from) {
        Ok(()) => Reversed::PathRestored {
            old: to.to_path_buf(),
            new: from.to_path_buf(),
        },
        Err(_) => Reversed::Skipped,
    }
}

fn flip_flag_f(msg: &Path, want: bool) -> Reversed {
    if !msg.exists() {
        return Reversed::Skipped;
    }
    match set_maildir_flag(msg, 'F', want) {
        Ok(new) => Reversed::FlagRestored {
            old: msg.to_path_buf(),
            new,
        },
        Err(_) => Reversed::Skipped,
    }
}

/// Add/remove a Maildir info flag (the letters after `:2,` in the
/// filename) and rename the file. Flags are kept ASCII-sorted per the
/// Maildir spec. Returns the new path (or the unchanged input if the
/// flag was already in the desired state).
pub(crate) fn set_maildir_flag(path: &Path, flag: char, want: bool) -> std::io::Result<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| std::io::Error::other("non-utf8 maildir filename"))?
        .to_string();
    let (base, flags) = match name.split_once(":2,") {
        Some((b, f)) => (b.to_string(), f.to_string()),
        None => (name.clone(), String::new()),
    };
    let has = flags.contains(flag);
    if has == want {
        return Ok(path.to_path_buf());
    }
    let new_flags: String = if want {
        let mut chars: Vec<char> = flags.chars().chain(std::iter::once(flag)).collect();
        chars.sort_unstable();
        chars.dedup();
        chars.into_iter().collect()
    } else {
        flags.chars().filter(|c| *c != flag).collect()
    };
    let new_name = format!("{}:2,{}", base, new_flags);
    let new_path = parent.join(new_name);
    fs::rename(path, &new_path)?;
    Ok(new_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_msg(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    #[test]
    fn mark_read_round_trip_moves_file_back_to_new() {
        let temp = TempDir::new().unwrap();
        let new = temp.path().join("INBOX/new/msg1");
        let cur = temp.path().join("INBOX/cur/msg1");
        write_msg(&cur, "body"); // post-action state: in cur/
        let m = Mutation::MarkRead {
            msg: cur.clone(),
            from: new.clone(),
            to: cur.clone(),
        };
        match m.reverse() {
            Reversed::PathRestored { .. } => {}
            other => panic!("expected PathRestored, got {:?}", other),
        }
        assert!(new.exists(), "file must be back in new/");
        assert!(!cur.exists(), "file must no longer be in cur/");
    }

    #[test]
    fn archive_round_trip_moves_file_back_to_inbox() {
        let temp = TempDir::new().unwrap();
        let inbox = temp.path().join("INBOX/cur/msg1");
        let archive = temp.path().join("Archive/cur/msg1");
        write_msg(&archive, "body");
        let m = Mutation::Archive {
            msg: archive.clone(),
            from: inbox.clone(),
            to: archive.clone(),
        };
        m.reverse();
        assert!(inbox.exists());
        assert!(!archive.exists());
    }

    #[test]
    fn delete_round_trip_moves_file_back_from_trash() {
        let temp = TempDir::new().unwrap();
        let inbox = temp.path().join("INBOX/cur/msg1");
        let trash = temp.path().join("Trash/cur/msg1");
        write_msg(&trash, "body");
        let m = Mutation::Delete {
            msg: trash.clone(),
            from: inbox.clone(),
            to: trash.clone(),
        };
        m.reverse();
        assert!(inbox.exists());
        assert!(!trash.exists());
    }

    #[test]
    fn move_round_trip_moves_file_back_to_origin() {
        let temp = TempDir::new().unwrap();
        let from = temp.path().join("INBOX/cur/msg1");
        let to = temp.path().join("Projects/cur/msg1");
        write_msg(&to, "body");
        let m = Mutation::Move {
            msg: to.clone(),
            from: from.clone(),
            to: to.clone(),
        };
        m.reverse();
        assert!(from.exists());
        assert!(!to.exists());
    }

    #[test]
    fn mark_unread_round_trip_moves_file_back_to_cur() {
        let temp = TempDir::new().unwrap();
        let cur = temp.path().join("INBOX/cur/msg1");
        let new = temp.path().join("INBOX/new/msg1");
        write_msg(&new, "body");
        let m = Mutation::MarkUnread {
            msg: new.clone(),
            from: cur.clone(),
            to: new.clone(),
        };
        m.reverse();
        assert!(cur.exists());
        assert!(!new.exists());
    }

    #[test]
    fn toggle_star_round_trip_removes_f_flag() {
        let temp = TempDir::new().unwrap();
        // Post-action: F flag has been added (was unstarred, now starred).
        let starred = temp.path().join("INBOX/cur/msg1:2,FS");
        write_msg(&starred, "body");
        let m = Mutation::ToggleStar {
            msg: starred.clone(),
            prev_flag: false,
        };
        match m.reverse() {
            Reversed::FlagRestored { new, .. } => {
                assert!(new.exists());
                let fname = new.file_name().unwrap().to_string_lossy().into_owned();
                let (_base, flags) = fname.split_once(":2,").unwrap();
                assert!(!flags.contains('F'), "flags '{}' must not contain F", flags);
            }
            other => panic!("expected FlagRestored, got {:?}", other),
        }
        assert!(!starred.exists(), "old F-flagged path must be gone");
    }

    #[test]
    fn toggle_star_round_trip_adds_f_flag() {
        let temp = TempDir::new().unwrap();
        // Post-action: F flag has been removed (was starred, now unstarred).
        let unstarred = temp.path().join("INBOX/cur/msg1:2,S");
        write_msg(&unstarred, "body");
        let m = Mutation::ToggleStar {
            msg: unstarred.clone(),
            prev_flag: true,
        };
        match m.reverse() {
            Reversed::FlagRestored { new, .. } => {
                assert!(new.exists());
                let fname = new.file_name().unwrap().to_string_lossy().into_owned();
                let (_base, flags) = fname.split_once(":2,").unwrap();
                assert!(flags.contains('F'), "flags '{}' must contain F", flags);
            }
            other => panic!("expected FlagRestored, got {:?}", other),
        }
    }

    #[test]
    fn reverse_of_missing_file_is_skipped() {
        let temp = TempDir::new().unwrap();
        let from = temp.path().join("INBOX/cur/msg1");
        let to = temp.path().join("Archive/cur/msg1");
        // Note: we never create `to`.
        let m = Mutation::Archive {
            msg: to.clone(),
            from: from.clone(),
            to: to.clone(),
        };
        match m.reverse() {
            Reversed::Skipped => {}
            other => panic!("expected Skipped, got {:?}", other),
        }
        assert!(!from.exists(), "must not have fabricated the source");
    }

    #[test]
    fn toggle_star_reverse_of_missing_file_is_skipped() {
        let temp = TempDir::new().unwrap();
        let m = Mutation::ToggleStar {
            msg: temp.path().join("INBOX/cur/nope:2,F"),
            prev_flag: false,
        };
        match m.reverse() {
            Reversed::Skipped => {}
            other => panic!("expected Skipped, got {:?}", other),
        }
    }

    #[test]
    fn set_maildir_flag_sorts_and_dedups() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("msg:2,SR");
        write_msg(&path, "x");
        let new = set_maildir_flag(&path, 'F', true).unwrap();
        // Flags should be ASCII-sorted: F, R, S.
        assert!(new.to_string_lossy().ends_with(":2,FRS"));
        assert!(new.exists());
    }

    #[test]
    fn set_maildir_flag_no_op_when_state_matches() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("msg:2,F");
        write_msg(&path, "x");
        let new = set_maildir_flag(&path, 'F', true).unwrap();
        assert_eq!(new, path);
    }

    #[test]
    fn set_maildir_flag_handles_missing_suffix() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("msg");
        write_msg(&path, "x");
        let new = set_maildir_flag(&path, 'F', true).unwrap();
        assert!(new.to_string_lossy().ends_with(":2,F"));
    }
}
