// Off-thread folder-headers loader.
//
// `MaildirScanner::load_folder_emails_with_limit` used to be called
// from the TUI thread, so every j/k that changed the folder selection
// and every folder-enter blocked on `fs::read_dir` + N × `fs::read` +
// header parse. On a cold NFS mount that adds 200ms–1s per first-touch
// keystroke.
//
// `HeadersLoader` mirrors `BodyLoader`'s shape: a single long-lived OS thread fed
// by an `mpsc` request channel, replying through a second channel that `AppRoot`
// drains each tick. Replies are always sent (even on failure) so the in-flight
// set in `AppRoot` never leaks.
//
// The worker owns its own `MaildirScanner` clone, so it does not race with the
// TUI for the `Mutex<App>`. The route key is `fs_path` (the folder's filesystem
// path) — late replies still find their target folder via `EmailStore::apply_loaded_folder`
// even after the user has navigated past it.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use crate::email::{Email, Folder};
use crate::maildir::MaildirScanner;

/// One unit of work for the headers worker: parse up to `limit`
/// headers under the folder at `fs_path`.
pub struct LoadFolderRequest {
    /// Filesystem path of the folder to scan (typically a child of
    /// the maildir root).
    pub fs_path: PathBuf,
    /// Cap on the number of headers to parse. `None` means "load
    /// every email and mark the folder fully loaded."
    pub limit: Option<usize>,
}

/// Reply payload from the headers worker. AppRoot routes this back
/// into the store via `EmailStore::apply_loaded_folder`.
pub struct LoadedFolder {
    /// Folder path the request named. Used to find the right folder
    /// to update even if the user has navigated past it.
    pub fs_path: PathBuf,
    /// Parsed-header `Email` rows in scan order.
    pub emails: Vec<Email>,
    /// True when the worker finished a complete scan (no limit, or empty
    /// / non-maildir folder). `AppRoot` uses this to flip `Folder::is_loaded`
    /// so future requests for the same path short-circuit.
    pub fully_loaded: bool,
}

/// Handle to the off-thread folder-headers worker. Owns the request /
/// reply channels; the worker thread itself runs to completion only
/// when `tx` is dropped (i.e. on AppRoot shutdown).
pub struct HeadersLoader {
    tx: Sender<LoadFolderRequest>,
    rx: Receiver<LoadedFolder>,
}

impl HeadersLoader {
    /// Spawn the worker thread. The thread exits when the request sender
    /// is dropped (i.e. when `AppRoot` is dropped on shutdown).
    pub fn spawn(scanner: MaildirScanner) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<LoadFolderRequest>();
        let (res_tx, res_rx) = mpsc::channel::<LoadedFolder>();

        thread::spawn(move || {
            while let Ok(LoadFolderRequest { fs_path, limit }) = req_rx.recv() {
                let name = fs_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                let mut folder = Folder::new(name, fs_path.clone());
                let load_err = scanner
                    .load_folder_emails_with_limit(&mut folder, limit)
                    .is_err();
                // Mark fully-loaded when the scanner reports it, or when the
                // folder has no emails after a non-failed scan (empty or
                // non-maildir directory). Prevents AppRoot from re-requesting
                // forever on every selection change.
                let fully_loaded = folder.is_loaded || (!load_err && folder.emails.is_empty());
                if res_tx
                    .send(LoadedFolder {
                        fs_path,
                        emails: std::mem::take(&mut folder.emails),
                        fully_loaded,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        Self {
            tx: req_tx,
            rx: res_rx,
        }
    }

    /// Enqueue a folder-headers request. Fire-and-forget — a closed
    /// channel is treated as silent shutdown. AppRoot dedups in-flight
    /// requests via its own `loading_folder_paths` set.
    pub fn request(&self, req: LoadFolderRequest) {
        let _ = self.tx.send(req);
    }

    /// Non-blocking poll for a finished folder-headers load. Same
    /// `TryRecvError` semantics as [`BodyLoader::try_recv`].
    ///
    /// [`BodyLoader::try_recv`]: crate::components::BodyLoader::try_recv
    pub fn try_recv(&self) -> Result<LoadedFolder, TryRecvError> {
        self.rx.try_recv()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::TestMailDir;
    use std::time::{Duration, Instant};

    fn await_reply(loader: &HeadersLoader) -> LoadedFolder {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match loader.try_recv() {
                Ok(r) => return r,
                Err(TryRecvError::Empty) => {
                    if Instant::now() > deadline {
                        panic!("headers loader did not reply within 2s");
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TryRecvError::Disconnected) => panic!("headers loader thread died"),
            }
        }
    }

    #[test]
    fn loader_returns_email_headers_for_a_real_folder() {
        let test_maildir = TestMailDir::new();
        let inbox_path = test_maildir.get_folder_path("INBOX");

        let scanner = MaildirScanner::new(test_maildir.root_path.clone());
        let loader = HeadersLoader::spawn(scanner);
        loader.request(LoadFolderRequest {
            fs_path: inbox_path.clone(),
            limit: Some(10),
        });

        let reply = await_reply(&loader);
        assert_eq!(reply.fs_path, inbox_path);
        assert!(
            !reply.emails.is_empty(),
            "fixture INBOX must yield at least one email header",
        );
        assert!(
            !reply.fully_loaded,
            "limit=Some means the worker did not exhaust the folder",
        );
    }

    /// A non-maildir / nonexistent path must still produce a reply (with
    /// empty emails, `fully_loaded = true`) so AppRoot can clear its
    /// in-flight slot and stop re-requesting.
    #[test]
    fn loader_replies_for_nonexistent_paths() {
        let scanner = MaildirScanner::new(PathBuf::from("/tmp"));
        let loader = HeadersLoader::spawn(scanner);
        loader.request(LoadFolderRequest {
            fs_path: PathBuf::from("/definitely/not/a/maildir/path"),
            limit: Some(10),
        });

        let reply = await_reply(&loader);
        assert!(reply.emails.is_empty());
        assert!(
            reply.fully_loaded,
            "non-maildir paths must report fully_loaded so the in-flight loop terminates",
        );
    }
}
