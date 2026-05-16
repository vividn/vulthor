// Off-thread full-body email parser (Phase 0.3.2, vu-6td).
//
// Audit entries A2 / A3 / B5 called `Email::ensure_fully_loaded` from the
// render thread, so every email selection change paid a `fs::read` + full
// MIME parse on the next frame. For 10-50 MB multipart messages on NFS
// this stalled the TUI for seconds.
//
// `BodyLoader` is a single long-lived OS thread fed by an `mpsc` request
// channel. AppRoot drains the reply channel each tick and writes results
// back into `EmailStore` via `apply_loaded_body`. The render path now
// reads only what's already in memory; the UI shows a "Loading body…"
// placeholder while `load_state == HeadersOnly`.
//
// A `BodyLoader` reply is always sent, even on parse failure, so the
// in-flight set in AppRoot can be reaped without leaking entries.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use crate::email::{Attachment, Email};

/// Result of a body-load attempt. `parsed` is `None` when `parse_from_file`
/// failed; AppRoot still uses the path to free its in-flight slot.
pub struct LoadedBody {
    pub path: PathBuf,
    pub parsed: Option<ParsedBody>,
}

pub struct ParsedBody {
    pub body_text: String,
    pub body_html: Option<String>,
    pub attachments: Vec<Attachment>,
}

pub struct BodyLoader {
    tx: Sender<PathBuf>,
    rx: Receiver<LoadedBody>,
}

impl BodyLoader {
    /// Spawn the worker thread. The thread exits when the request sender
    /// is dropped (i.e. when `AppRoot` is dropped on shutdown).
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<PathBuf>();
        let (res_tx, res_rx) = mpsc::channel::<LoadedBody>();

        thread::spawn(move || {
            while let Ok(path) = req_rx.recv() {
                let mut email = Email::new(path.clone());
                let parsed = match email.parse_from_file() {
                    Ok(()) => Some(ParsedBody {
                        body_text: std::mem::take(&mut email.body_text),
                        body_html: email.body_html.take(),
                        attachments: std::mem::take(&mut email.attachments),
                    }),
                    Err(_) => None,
                };
                if res_tx.send(LoadedBody { path, parsed }).is_err() {
                    break;
                }
            }
        });

        Self {
            tx: req_tx,
            rx: res_rx,
        }
    }

    pub fn request(&self, path: PathBuf) {
        let _ = self.tx.send(path);
    }

    /// Clone of the request channel — lets the web server submit body-load
    /// requests through the same worker the TUI uses, so neither side has
    /// to do an `fs::read` on its own thread (`vu-9ie`, D1-D3).
    pub fn request_sender(&self) -> Sender<PathBuf> {
        self.tx.clone()
    }

    pub fn try_recv(&self) -> Result<LoadedBody, TryRecvError> {
        self.rx.try_recv()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::TestMailDir;
    use std::fs;
    use std::time::{Duration, Instant};

    #[test]
    fn loader_parses_a_real_email_off_thread() {
        let test_maildir = TestMailDir::new();
        let inbox = test_maildir.get_folder_path("INBOX").join("cur");
        let file = fs::read_dir(&inbox)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_file())
            .map(|e| e.path())
            .expect("test fixture must contain at least one email");

        let loader = BodyLoader::spawn();
        loader.request(file.clone());

        // Poll for up to 2s for a response. The parse is in another thread;
        // we never want to block the caller, but the test does need to wait
        // for it to land.
        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            match loader.try_recv() {
                Ok(r) => break r,
                Err(TryRecvError::Empty) => {
                    if Instant::now() > deadline {
                        panic!("body loader did not reply within 2s");
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TryRecvError::Disconnected) => panic!("body loader thread died"),
            }
        };

        assert_eq!(result.path, file);
        let parsed = result.parsed.expect("parse must succeed for valid fixture");
        assert!(
            !parsed.body_text.is_empty(),
            "parsed body must contain text"
        );
    }

    #[test]
    fn loader_replies_even_for_unreadable_paths() {
        // A non-existent path must still produce a `LoadedBody` reply so
        // AppRoot can clear its in-flight slot. `parsed` will be `None`.
        let loader = BodyLoader::spawn();
        loader.request(PathBuf::from("/definitely/does/not/exist/email"));

        let deadline = Instant::now() + Duration::from_secs(2);
        let result = loop {
            match loader.try_recv() {
                Ok(r) => break r,
                Err(TryRecvError::Empty) => {
                    if Instant::now() > deadline {
                        panic!("body loader did not reply within 2s");
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(TryRecvError::Disconnected) => panic!("body loader thread died"),
            }
        };
        assert!(
            result.parsed.is_none(),
            "missing file must yield None parsed body, got Some"
        );
    }
}
