// Off-thread MailDir folder-structure scanner (Phase 0.3.4, vu-w9i).
//
// Audit entry C2 had `MaildirScanner::scan` running on the main thread
// *before* the TUI enabled raw mode. On NFS or a maildir with hundreds
// of folders this stalled the launch for multiple seconds, with only a
// `println!` banner to show for it.
//
// `FolderScannerHandle` spawns a single short-lived OS thread that
// runs the scan and sends its `Result<Folder>` back over an `mpsc`
// channel. `AppRoot` drains the reply each tick and swaps the scanned
// tree into `EmailStore::root_folder`. Until then, the folder pane
// shows a "Scanning folders…" splash via `EmailStore::scanning_folders`.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use crate::email::Folder;
use crate::error::Result;
use crate::maildir::MaildirScanner;

pub struct FolderScannerHandle {
    rx: Receiver<Result<Folder>>,
}

impl FolderScannerHandle {
    /// Spawn the worker thread and return a handle. The thread runs to
    /// completion exactly once: it scans `maildir_path` and sends the
    /// result, then exits. Dropping the handle drops the receiver; if
    /// the send happens after that, it fails silently — fine, because
    /// we only care about reaping the result when someone is listening.
    pub fn spawn(maildir_path: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel::<Result<Folder>>();
        thread::spawn(move || {
            let scanner = MaildirScanner::new(maildir_path);
            let _ = tx.send(scanner.scan());
        });
        Self { rx }
    }

    pub fn try_recv(&self) -> std::result::Result<Result<Folder>, TryRecvError> {
        self.rx.try_recv()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    /// Build a maildir with a deep nested hierarchy to exercise the
    /// recursive `scan_folder_structure_only` path that vu-w9i is
    /// moving off the main thread. Returns the temp dir (must outlive
    /// the scanner) and the path to scan.
    fn build_deep_maildir(depth: usize, branching: usize) -> TempDir {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        // Build a tree where each non-leaf node has `branching` children;
        // every directory is a valid maildir (has cur/new/tmp).
        fn build(base: &std::path::Path, depth: usize, branching: usize) {
            fs::create_dir_all(base.join("cur")).unwrap();
            fs::create_dir_all(base.join("new")).unwrap();
            fs::create_dir_all(base.join("tmp")).unwrap();
            if depth == 0 {
                return;
            }
            for i in 0..branching {
                let child = base.join(format!("sub{}", i));
                build(&child, depth - 1, branching);
            }
        }
        build(&root, depth, branching);
        temp
    }

    fn wait_for<T>(
        mut poll: impl FnMut() -> std::result::Result<T, TryRecvError>,
        timeout: Duration,
    ) -> T {
        let deadline = Instant::now() + timeout;
        loop {
            match poll() {
                Ok(v) => return v,
                Err(TryRecvError::Empty) => {
                    if Instant::now() > deadline {
                        panic!("folder scanner did not reply within {:?}", timeout);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TryRecvError::Disconnected) => panic!("scanner thread died before replying"),
            }
        }
    }

    /// vu-w9i acceptance: a deep maildir hierarchy must come back from
    /// the worker as a populated `Folder` tree. The exact recursion is
    /// tested by `MaildirScanner` itself; here we just prove the
    /// off-thread handoff delivers the same shape.
    #[test]
    fn scanner_returns_deep_hierarchy_off_thread() {
        let temp = build_deep_maildir(3, 3);
        let handle = FolderScannerHandle::spawn(temp.path().to_path_buf());

        let result = wait_for(|| handle.try_recv(), Duration::from_secs(5));
        let root = result.expect("scan must succeed on a valid maildir");
        assert_eq!(root.name, "Mail");
        // Three top-level subfolders (sub0, sub1, sub2); each has three
        // children, etc. We only assert the top-level count — deeper
        // shape is covered by `MaildirScanner` tests.
        assert_eq!(root.subfolders.len(), 3);
    }

    /// Missing path errors must propagate through the channel rather
    /// than panic the worker. The `Err` arm lets the caller put up a
    /// status message instead of silently sitting in "Scanning…".
    #[test]
    fn scanner_propagates_missing_path_error() {
        let missing = PathBuf::from("/definitely/does/not/exist/vu-w9i");
        let handle = FolderScannerHandle::spawn(missing);
        let result = wait_for(|| handle.try_recv(), Duration::from_secs(2));
        assert!(result.is_err(), "missing path must yield Err, got Ok");
    }
}
