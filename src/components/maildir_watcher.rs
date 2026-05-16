// Off-thread MailDir auto-refresh watcher (Phase 4.d).
//
// `MaildirWatcherComponent` wraps a recursive `notify` watcher rooted
// at an account's `maildir_path`. The watcher thread (owned by the
// `notify` backend) forwards every event into an `mpsc::Receiver` that
// `AppRoot` drains each tick via [`Self::drain`]. Drain debounces rapid
// bursts into one [`Msg::MailDirChanged`] per folder, so a sweep of
// `mbsync` writes lands as a single refresh rather than dozens.
//
// We only care about Create / Rename events under `<folder>/cur/` or
// `<folder>/new/` — those are the maildir leaves where new mail
// appears. Everything else (modifies to `tmp/`, access times, etc.) is
// filtered out.
//
// AppRoot owns the component; on `Msg::AccountSelect` it drops the old
// watcher and constructs a fresh one rooted at the new path. Watcher
// init failures surface as [`VulthorError::MailDirWatch`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::{Result, VulthorError};

use super::Msg;

/// Default debounce window. Coalesces bursts of filesystem events from
/// a single `mbsync` sweep into one refresh per folder.
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(250);

/// Watches a MailDir tree recursively. Maintains a per-folder pending
/// timestamp so a burst of events on the same folder collapses to a
/// single [`Msg::MailDirChanged`] once the debounce window closes.
///
/// The `notify` watcher lives in `_watcher`; dropping the component
/// stops the OS-level inotify/FSEvents subscription and the worker
/// thread the backend spawned. The receive channel returns
/// `Disconnected` to a late `drain()` call, which it treats as "no
/// events" — fine for shutdown.
pub struct MaildirWatcherComponent {
    /// Root path the watcher is anchored on. Mostly diagnostic — the
    /// notify backend remembers what it's watching.
    root: PathBuf,
    /// The OS-level watcher. Held to keep the subscription alive; we
    /// never call into it after construction. Dropped on component
    /// teardown so resources clean up promptly.
    _watcher: RecommendedWatcher,
    /// Receive end of the worker → main thread event channel.
    rx: Receiver<Event>,
    /// Folder paths with an outstanding event whose debounce window
    /// has not closed yet. Mapped to the first-seen `Instant` so a
    /// continued burst doesn't reset the clock — once the window
    /// elapses, the folder fires regardless of further activity.
    pending: HashMap<PathBuf, Instant>,
    debounce: Duration,
}

impl MaildirWatcherComponent {
    /// Spawn a recursive watcher rooted at `path`. The first
    /// `Msg::MailDirChanged` fires no sooner than `debounce` after the
    /// first qualifying event.
    pub fn spawn(path: PathBuf, debounce: Duration) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Event>();
        let mut watcher = build_watcher(tx).map_err(|source| VulthorError::MailDirWatch {
            path: path.clone(),
            source,
        })?;
        watcher
            .watch(&path, RecursiveMode::Recursive)
            .map_err(|source| VulthorError::MailDirWatch {
                path: path.clone(),
                source,
            })?;
        Ok(Self {
            root: path,
            _watcher: watcher,
            rx,
            pending: HashMap::new(),
            debounce,
        })
    }

    /// Root path the watcher was constructed against. Tests + the
    /// `Msg::AccountSelect` rebuild path use this to confirm identity.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Drain any events the worker has buffered, fold them into the
    /// per-folder pending map, and emit one [`Msg::MailDirChanged`] per
    /// folder whose debounce window has elapsed.
    ///
    /// Returns immediately when the channel is empty — this is meant
    /// to run inside `AppRoot::tick`, alongside the other off-thread
    /// drains, so it must not block.
    pub fn drain(&mut self) -> Vec<Msg> {
        self.drain_at(Instant::now())
    }

    /// Test seam: drain against an explicit `now`. Lets the unit tests
    /// drive debounce timing without `thread::sleep`.
    pub fn drain_at(&mut self, now: Instant) -> Vec<Msg> {
        loop {
            match self.rx.try_recv() {
                Ok(event) => {
                    if !is_refresh_kind(&event.kind) {
                        continue;
                    }
                    for p in &event.paths {
                        if let Some(folder) = folder_for_event_path(p) {
                            self.pending.entry(folder).or_insert(now);
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }

        let mut ready: Vec<PathBuf> = self
            .pending
            .iter()
            .filter_map(|(p, since)| {
                if now.saturating_duration_since(*since) >= self.debounce {
                    Some(p.clone())
                } else {
                    None
                }
            })
            .collect();
        ready.sort();
        for p in &ready {
            self.pending.remove(p);
        }
        ready.into_iter().map(Msg::MailDirChanged).collect()
    }
}

fn build_watcher(tx: Sender<Event>) -> notify::Result<RecommendedWatcher> {
    notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res {
            // Drop send failures silently: the receiver is dropped
            // when the component goes away, which is the normal
            // teardown path.
            let _ = tx.send(event);
        }
    })
}

/// True when an event kind represents a refresh-worthy change under a
/// MailDir leaf: file creation or a rename (the two-step move mbsync
/// performs to land a message). Modifies to existing files don't
/// surface new mail, so we ignore them.
fn is_refresh_kind(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(notify::event::ModifyKind::Name(_))
    )
}

/// Map a filesystem path reported by `notify` to the folder it
/// belongs to. Returns `None` when the path is not inside a
/// `cur/` or `new/` leaf — we don't refresh on `tmp/` activity
/// because mbsync writes there mid-transfer and renames into
/// `new/` once complete.
///
/// Example: `/home/x/Mail/INBOX/cur/1234.M.foo` → `/home/x/Mail/INBOX`.
fn folder_for_event_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let leaf = parent.file_name().and_then(|n| n.to_str())?;
    if leaf != "cur" && leaf != "new" {
        return None;
    }
    parent.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Build a minimal maildir under `root`: `<root>/INBOX/{cur,new,tmp}`.
    fn make_maildir(root: &Path) -> PathBuf {
        let inbox = root.join("INBOX");
        for sub in ["cur", "new", "tmp"] {
            fs::create_dir_all(inbox.join(sub)).unwrap();
        }
        inbox
    }

    /// Pump the watcher until `cond` returns Some(value) or the
    /// timeout elapses. Lets us wait on the OS event delivery without
    /// hard-coding a sleep that's flaky on slow CI.
    fn await_drain<F, T>(
        watcher: &mut MaildirWatcherComponent,
        timeout: Duration,
        mut cond: F,
    ) -> Option<T>
    where
        F: FnMut(&[Msg]) -> Option<T>,
    {
        let deadline = Instant::now() + timeout;
        loop {
            // Drain with a clock far enough in the future that any
            // pending event escapes the debounce window — these tests
            // care about "did the event reach us at all", not the
            // debounce timing (covered separately below).
            let msgs = watcher.drain_at(Instant::now() + Duration::from_secs(1));
            if let Some(v) = cond(&msgs) {
                return Some(v);
            }
            if Instant::now() > deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn folder_path_extracts_parent_of_cur_and_new_leaves() {
        let cur = PathBuf::from("/m/INBOX/cur/123.M.x");
        assert_eq!(folder_for_event_path(&cur), Some(PathBuf::from("/m/INBOX")),);
        let new = PathBuf::from("/m/INBOX/new/456.M.y");
        assert_eq!(folder_for_event_path(&new), Some(PathBuf::from("/m/INBOX")),);
    }

    #[test]
    fn folder_path_ignores_tmp_and_unrelated_paths() {
        // `tmp/` is mbsync's transfer buffer; refreshing on activity
        // there would fire before the message is actually delivered.
        assert!(folder_for_event_path(&PathBuf::from("/m/INBOX/tmp/x")).is_none());
        // Anything not in cur/new is ignored too.
        assert!(folder_for_event_path(&PathBuf::from("/m/INBOX/foo")).is_none());
        // A bare root has no parent → no folder.
        assert!(folder_for_event_path(&PathBuf::from("/")).is_none());
    }

    #[test]
    fn watcher_emits_msg_on_file_create_under_cur() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        let inbox = make_maildir(&root);

        let mut watcher = MaildirWatcherComponent::spawn(root.clone(), Duration::from_millis(0))
            .expect("watcher must spawn against a real maildir");

        // Give the backend a beat to register before we touch files.
        // notify's inotify backend can lose events that race the watch.
        thread::sleep(Duration::from_millis(100));

        let target = inbox.join("cur").join("1700000000.M0P0Q0.host:2,S");
        fs::write(&target, b"From: a@b\r\n\r\nbody").unwrap();

        let got = await_drain(&mut watcher, Duration::from_secs(3), |msgs| {
            msgs.iter().find_map(|m| match m {
                Msg::MailDirChanged(p) if *p == inbox => Some(p.clone()),
                _ => None,
            })
        });
        assert!(
            got.is_some(),
            "watcher must emit MailDirChanged for {:?} after file create",
            inbox,
        );
    }

    #[test]
    fn debounce_coalesces_rapid_creates_into_one_msg() {
        // Stage multiple "events" by hand so we can drive the
        // debounce clock deterministically. Real-event coverage lives
        // in `watcher_emits_msg_on_file_create_under_cur`.
        let temp = TempDir::new().unwrap();
        let inbox = make_maildir(temp.path());
        let mut watcher =
            MaildirWatcherComponent::spawn(temp.path().to_path_buf(), Duration::from_millis(250))
                .unwrap();

        let t0 = Instant::now();
        // Simulate a burst by pre-populating the pending map at t0.
        watcher.pending.insert(inbox.clone(), t0);
        // Inside the debounce window: nothing fires.
        let early = watcher.drain_at(t0 + Duration::from_millis(100));
        assert!(early.is_empty(), "debounce must suppress early drains");
        // Past the window: one Msg fires (not one per event).
        let late = watcher.drain_at(t0 + Duration::from_millis(300));
        assert_eq!(
            late,
            vec![Msg::MailDirChanged(inbox.clone())],
            "debounced burst must coalesce into a single Msg",
        );
        // Subsequent drains with no new events stay empty.
        let after = watcher.drain_at(t0 + Duration::from_millis(400));
        assert!(after.is_empty());
    }

    #[test]
    fn watcher_root_reflects_constructor_path() {
        // Account switch (`Msg::AccountSelect`) drops the old watcher
        // and spawns a fresh one. Tests outside this module check that
        // path-swap via this getter.
        let temp = TempDir::new().unwrap();
        let watcher =
            MaildirWatcherComponent::spawn(temp.path().to_path_buf(), DEFAULT_DEBOUNCE).unwrap();
        assert_eq!(watcher.root(), temp.path());
    }

    #[test]
    fn drop_tears_down_resources_cleanly() {
        // We can't peek inside notify, but we can prove no panic /
        // hang occurs across the drop boundary by spawning many
        // watchers in sequence on the same path.
        let temp = TempDir::new().unwrap();
        make_maildir(temp.path());
        for _ in 0..5 {
            let w = MaildirWatcherComponent::spawn(temp.path().to_path_buf(), DEFAULT_DEBOUNCE)
                .unwrap();
            drop(w);
        }
    }

    #[test]
    fn missing_root_returns_maildir_watch_error() {
        // The error must carry the offending path and the underlying
        // notify error so the status-bar message is actionable.
        let missing = PathBuf::from("/definitely/not/here/vulthor-watch");
        match MaildirWatcherComponent::spawn(missing.clone(), DEFAULT_DEBOUNCE) {
            Ok(_) => panic!("watch on missing path must fail"),
            Err(VulthorError::MailDirWatch { path, .. }) => assert_eq!(path, missing),
            Err(other) => panic!("expected MailDirWatch error, got {:?}", other),
        }
    }
}
