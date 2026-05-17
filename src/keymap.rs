// `[keybindings]` overrides → resolved `KeyEvent → Action` table.
//
// VISION.md §Configuration Schema requires every action key to be
// user-rebindable from `[keybindings]` in `vulthor.toml`. This module
// owns the contract: the `Action` enum (closed set of remappable
// intents), the `DEFAULT_KEYMAP` mirroring VISION.md §Action
// Keybindings, and `resolve_keymap` which folds user overrides over the
// defaults and rejects conflicts.
//
// The resolver returns a `Keymap` with two tables — single-key and
// sequence — so AppRoot can dispatch atomic keys via the fast path
// while sequences (gg, gj, gk, gr) still flow through component-local
// prefix logic.

use std::collections::{BTreeMap, HashMap};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::error::{Result, VulthorError};

/// Closed set of user-rebindable intents. Mirrors VISION.md §Action
/// Keybindings. Components translate context-sensitive keys (e.g.
/// `Enter` in Messages opens a mail, in Folders enters a folder) at
/// dispatch time; the enum stays intent-level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    // Navigation
    MoveDown,
    MoveUp,
    PageDown,
    PageUp,
    ViewPrev,
    ViewNext,
    FocusNext,
    FocusPrev,
    Confirm,
    Back,
    JumpTop,
    JumpBottom,
    JumpNextUnread,
    JumpPrevUnread,
    // Email actions
    Archive,
    Star,
    Delete,
    AcceptSuggestion,
    Undo,
    ReplyAll,
    Reply,
    ReplyLater,
    Forward,
    MoveToFolder,
    ToggleFlag,
    MarkUnread,
    OpenAttachment,
    // Search
    Search,
    SearchNext,
    SearchPrev,
    // View control
    ToggleContentPane,
    ToggleViewer,
    /// vu-c1s: force the content pane to render `body_plain` even when
    /// the email also carries an HTML alternative. Toggled per-session;
    /// startup default comes from `[render].prefer_plaintext`.
    ToggleHtmlOff,
    ToggleHelp,
    Quit,
    // Draft pane
    DraftSend,
    DraftEdit,
    DraftDiscard,
}

impl Action {
    /// Canonical TOML name, used as the key in `[keybindings]`.
    pub const fn name(self) -> &'static str {
        match self {
            Action::MoveDown => "move_down",
            Action::MoveUp => "move_up",
            Action::PageDown => "page_down",
            Action::PageUp => "page_up",
            Action::ViewPrev => "view_prev",
            Action::ViewNext => "view_next",
            Action::FocusNext => "focus_next",
            Action::FocusPrev => "focus_prev",
            Action::Confirm => "confirm",
            Action::Back => "back",
            Action::JumpTop => "jump_top",
            Action::JumpBottom => "jump_bottom",
            Action::JumpNextUnread => "jump_next_unread",
            Action::JumpPrevUnread => "jump_prev_unread",
            Action::Archive => "archive",
            Action::Star => "star",
            Action::Delete => "delete",
            Action::AcceptSuggestion => "accept_suggestion",
            Action::Undo => "undo",
            Action::ReplyAll => "reply_all",
            Action::Reply => "reply",
            Action::ReplyLater => "reply_later",
            Action::Forward => "forward",
            Action::MoveToFolder => "move_to_folder",
            Action::ToggleFlag => "toggle_flag",
            Action::MarkUnread => "mark_unread",
            Action::OpenAttachment => "open_attachment",
            Action::Search => "search",
            Action::SearchNext => "search_next",
            Action::SearchPrev => "search_prev",
            Action::ToggleContentPane => "toggle_content_pane",
            Action::ToggleViewer => "toggle_viewer",
            Action::ToggleHelp => "toggle_help",
            Action::Quit => "quit",
            Action::DraftSend => "draft_send",
            Action::DraftEdit => "draft_edit",
            Action::DraftDiscard => "draft_discard",
        }
    }

    /// All actions, in declaration order. Used by tests asserting
    /// default-keymap coverage and by `from_name` to drive lookup
    /// without a hand-maintained reverse table.
    pub const fn all() -> &'static [Action] {
        &[
            Action::MoveDown,
            Action::MoveUp,
            Action::PageDown,
            Action::PageUp,
            Action::ViewPrev,
            Action::ViewNext,
            Action::FocusNext,
            Action::FocusPrev,
            Action::Confirm,
            Action::Back,
            Action::JumpTop,
            Action::JumpBottom,
            Action::JumpNextUnread,
            Action::JumpPrevUnread,
            Action::Archive,
            Action::Star,
            Action::Delete,
            Action::AcceptSuggestion,
            Action::Undo,
            Action::ReplyAll,
            Action::Reply,
            Action::ReplyLater,
            Action::Forward,
            Action::MoveToFolder,
            Action::ToggleFlag,
            Action::MarkUnread,
            Action::OpenAttachment,
            Action::Search,
            Action::SearchNext,
            Action::SearchPrev,
            Action::ToggleContentPane,
            Action::ToggleViewer,
            Action::ToggleHelp,
            Action::Quit,
            Action::DraftSend,
            Action::DraftEdit,
            Action::DraftDiscard,
        ]
    }

    /// Parse a TOML name back into an `Action`. Returns `None` for
    /// unknown names so the resolver can raise a structured error.
    pub fn from_name(name: &str) -> Option<Action> {
        Action::all().iter().copied().find(|a| a.name() == name)
    }
}

/// Default key bindings, mirroring VISION.md §Action Keybindings.
/// Sequence bindings (`gg`, `G`, `gj`, `gk`, `gr`) are encoded
/// verbatim; the parser splits them into multi-event chords.
///
/// An action may appear multiple times — e.g. `MoveDown` is bound to
/// both `j` and `Down` by default so arrow-key users don't have to
/// rebind. A user override in `[keybindings]` replaces every default
/// binding for that action with a single chosen key (one key per
/// override entry — multi-key overrides aren't a Vulthor concept).
pub const DEFAULT_KEYMAP: &[(Action, &str)] = &[
    // Navigation
    (Action::MoveDown, "j"),
    (Action::MoveDown, "Down"),
    (Action::MoveUp, "k"),
    (Action::MoveUp, "Up"),
    (Action::PageDown, "PageDown"),
    (Action::PageUp, "PageUp"),
    (Action::ViewPrev, "h"),
    (Action::ViewNext, "l"),
    (Action::FocusNext, "Tab"),
    (Action::FocusPrev, "BackTab"),
    (Action::Confirm, "Enter"),
    (Action::Back, "Backspace"),
    (Action::JumpTop, "gg"),
    (Action::JumpBottom, "G"),
    (Action::JumpNextUnread, "gj"),
    (Action::JumpPrevUnread, "gk"),
    // Email actions
    (Action::Archive, "a"),
    (Action::Star, "s"),
    (Action::Delete, "d"),
    (Action::AcceptSuggestion, ";"),
    (Action::Undo, "u"),
    (Action::ReplyAll, "r"),
    (Action::Reply, "gr"),
    (Action::ReplyLater, "R"),
    (Action::Forward, "f"),
    (Action::MoveToFolder, "m"),
    (Action::ToggleFlag, "F"),
    (Action::MarkUnread, "U"),
    (Action::OpenAttachment, "o"),
    // Search
    (Action::Search, "/"),
    (Action::SearchNext, "n"),
    (Action::SearchPrev, "N"),
    // View control
    (Action::ToggleContentPane, "Alt+c"),
    (Action::ToggleViewer, "v"),
    (Action::ToggleHelp, "?"),
    (Action::Quit, "q"),
    // Draft pane
    (Action::DraftSend, "S"),
    (Action::DraftEdit, "e"),
    (Action::DraftDiscard, "Esc"),
];

/// Parse a key-string into a sequence of `KeyEvent`s. Length 1 for
/// atomic keys (including modified ones); length > 1 for sequences
/// like `gr` or `gg`.
///
/// Recognised forms:
/// - single printable: `a`, `;`, `/`, `?`, `G`
/// - named: `Enter`, `Backspace`, `Tab`, `BackTab`, `Esc`, `Up`/`Down`/`Left`/`Right`
/// - modified atomic: `Alt+c`, `Ctrl+x`, `Shift+Tab`
/// - sequence: any other multi-character ASCII string is decomposed
///   into one `Char` event per character (e.g. `gr` → `g` then `r`).
pub fn parse_key_string(s: &str) -> std::result::Result<Vec<KeyEvent>, String> {
    if s.is_empty() {
        return Err("empty key string".into());
    }
    if let Some(rest) = s.strip_prefix("Alt+") {
        return Ok(vec![parse_modified(rest, KeyModifiers::ALT)?]);
    }
    if let Some(rest) = s.strip_prefix("Ctrl+") {
        return Ok(vec![parse_modified(rest, KeyModifiers::CONTROL)?]);
    }
    if let Some(rest) = s.strip_prefix("Shift+") {
        return Ok(vec![parse_modified(rest, KeyModifiers::SHIFT)?]);
    }
    if let Some(ev) = parse_named(s) {
        return Ok(vec![ev]);
    }
    if s.chars().count() == 1 {
        let c = s.chars().next().unwrap();
        return Ok(vec![char_event(c)]);
    }
    // Multi-character string with no recognised name or modifier: a
    // sequence of single-char chords (e.g. `gr`, `gg`).
    let mut out = Vec::with_capacity(s.chars().count());
    for c in s.chars() {
        out.push(char_event(c));
    }
    Ok(out)
}

fn parse_modified(rest: &str, mods: KeyModifiers) -> std::result::Result<KeyEvent, String> {
    if let Some(named) = parse_named(rest) {
        return Ok(KeyEvent::new(named.code, mods));
    }
    if rest.chars().count() == 1 {
        return Ok(KeyEvent::new(
            KeyCode::Char(rest.chars().next().unwrap()),
            mods,
        ));
    }
    Err(format!("cannot apply modifier to multi-char key '{rest}'"))
}

fn parse_named(s: &str) -> Option<KeyEvent> {
    let code = match s {
        "Enter" => KeyCode::Enter,
        "Backspace" => KeyCode::Backspace,
        "Esc" => KeyCode::Esc,
        "Tab" => KeyCode::Tab,
        "BackTab" => KeyCode::BackTab,
        "Up" => KeyCode::Up,
        "Down" => KeyCode::Down,
        "Left" => KeyCode::Left,
        "Right" => KeyCode::Right,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        "Space" => KeyCode::Char(' '),
        _ => return None,
    };
    Some(KeyEvent::new(code, KeyModifiers::NONE))
}

fn char_event(c: char) -> KeyEvent {
    // Canonical char events carry no modifiers; uppercase ASCII
    // letters arrive bare in our map. `Keymap::normalize` strips SHIFT
    // at lookup time so terminals that *do* report it still hit.
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

/// Resolved keymap: defaults overlaid with user overrides, split into
/// single-key (fast-path dispatch) and sequence (prefix dispatch)
/// tables. Built via [`resolve_keymap`].
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    single: HashMap<KeyEvent, Action>,
    sequences: HashMap<Vec<KeyEvent>, Action>,
}

impl Keymap {
    /// Look up the action bound to an atomic key, normalising
    /// terminal-quirky SHIFT reports on uppercase ASCII letters.
    pub fn lookup_single(&self, key: KeyEvent) -> Option<Action> {
        let canon = Self::normalize(key);
        self.single.get(&canon).copied()
    }

    /// Look up the action bound to a key sequence (no normalisation —
    /// sequences are user-typed lowercase letters in practice).
    pub fn lookup_sequence(&self, keys: &[KeyEvent]) -> Option<Action> {
        self.sequences.get(keys).copied()
    }

    /// Iterate actions whose sequence binding has `prefix` as a strict
    /// prefix (i.e. the bound sequence is longer than `prefix` and
    /// starts with it). AppRoot uses this to decide whether to hold the
    /// pending key buffer — if no meaningful sequence remains to type,
    /// the buffered prefix is dropped and dispatch falls through to the
    /// single-key table.
    pub fn sequences_with_prefix<'a>(
        &'a self,
        prefix: &'a [KeyEvent],
    ) -> impl Iterator<Item = Action> + 'a {
        self.sequences
            .iter()
            .filter(move |(seq, _)| seq.len() > prefix.len() && seq.starts_with(prefix))
            .map(|(_, a)| *a)
    }

    /// Iterate every (key, action) pair across both tables. Tests use
    /// this to assert coverage of the default action set.
    #[allow(dead_code)]
    pub fn all_actions(&self) -> impl Iterator<Item = Action> + '_ {
        self.single.values().chain(self.sequences.values()).copied()
    }

    /// Crossterm reports uppercase ASCII letters as `Char(c)` with
    /// `SHIFT` on some terminals and `NONE` on others. We canonicalise
    /// to NONE so DEFAULT_KEYMAP entries match either way.
    fn normalize(mut key: KeyEvent) -> KeyEvent {
        if let KeyCode::Char(c) = key.code
            && c.is_ascii_uppercase()
            && key.modifiers == KeyModifiers::SHIFT
        {
            key.modifiers = KeyModifiers::NONE;
        }
        key
    }
}

/// Build a [`Keymap`] from the static defaults plus user overrides.
///
/// A user override replaces *every* default key for that action with a
/// single chosen key, so e.g. binding `move_down = "x"` retires both
/// `j` and the arrow `Down` for MoveDown. Conflict detection runs after
/// override application: rebinding `archive` to `e` without freeing the
/// default `e`-bound `draft_edit` is still a structured error.
pub fn resolve_keymap(overrides: &BTreeMap<String, String>) -> Result<Keymap> {
    // Defaults as `(action, key)` pairs — `Vec` (not `BTreeMap`) so a
    // single action can carry multiple keys (e.g. MoveDown ↔ j, Down).
    let mut bindings: Vec<(Action, String)> = DEFAULT_KEYMAP
        .iter()
        .map(|(a, s)| (*a, (*s).to_string()))
        .collect();

    // Apply overrides. Each entry retires every default binding for
    // that action, then claims the user-chosen key. Unknown action
    // names are rejected up-front so typos don't silently no-op.
    for (action_name, key_string) in overrides.iter() {
        let Some(action) = Action::from_name(action_name) else {
            return Err(VulthorError::KeybindingUnknownAction {
                action: action_name.clone(),
            });
        };
        bindings.retain(|(a, _)| *a != action);
        bindings.push((action, key_string.clone()));
    }

    // Materialise to the two lookup tables, raising on either a parse
    // failure or a key-string collision.
    let mut single: HashMap<KeyEvent, Action> = HashMap::new();
    let mut sequences: HashMap<Vec<KeyEvent>, Action> = HashMap::new();

    // Track raw key-strings already claimed for stable conflict
    // diagnostics (so the error names the user's input, not a
    // pretty-printed KeyEvent). Re-binding the same action to the same
    // key string twice is not a conflict — `DEFAULT_KEYMAP` carries one
    // entry per `(action, key)` pair already.
    let mut by_key: HashMap<String, Action> = HashMap::new();

    for (action, key_string) in bindings.iter() {
        let events =
            parse_key_string(key_string).map_err(|reason| VulthorError::KeybindingInvalidKey {
                key: key_string.clone(),
                action: action.name().to_string(),
                reason,
            })?;
        if events.is_empty() {
            return Err(VulthorError::KeybindingInvalidKey {
                key: key_string.clone(),
                action: action.name().to_string(),
                reason: "no key events produced".into(),
            });
        }

        if let Some(prior) = by_key.get(key_string)
            && prior != action
        {
            let mut names = [prior.name().to_string(), action.name().to_string()];
            names.sort();
            return Err(VulthorError::KeybindingConflict {
                key: key_string.clone(),
                action_a: names[0].clone(),
                action_b: names[1].clone(),
            });
        }
        by_key.insert(key_string.clone(), *action);

        if events.len() == 1 {
            if let Some(prior) = single.insert(events[0], *action)
                && prior != *action
            {
                let mut names = [prior.name().to_string(), action.name().to_string()];
                names.sort();
                return Err(VulthorError::KeybindingConflict {
                    key: key_string.clone(),
                    action_a: names[0].clone(),
                    action_b: names[1].clone(),
                });
            }
        } else if let Some(prior) = sequences.insert(events, *action)
            && prior != *action
        {
            let mut names = [prior.name().to_string(), action.name().to_string()];
            names.sort();
            return Err(VulthorError::KeybindingConflict {
                key: key_string.clone(),
                action_a: names[0].clone(),
                action_b: names[1].clone(),
            });
        }
    }

    Ok(Keymap { single, sequences })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn default_keymap_covers_every_action() {
        // VISION.md §Action Keybindings lists every action; the
        // default table must round-trip the closed set, so users can
        // override any one of them and we can render a help screen
        // from a single source of truth.
        let map = resolve_keymap(&BTreeMap::new()).expect("defaults resolve");
        let covered: HashSet<Action> = map.all_actions().collect();
        for action in Action::all() {
            assert!(
                covered.contains(action),
                "default keymap missing action: {:?}",
                action
            );
        }
    }

    #[test]
    fn action_from_name_round_trips() {
        for action in Action::all() {
            assert_eq!(Action::from_name(action.name()), Some(*action));
        }
        assert!(Action::from_name("not_a_real_action").is_none());
    }

    #[test]
    fn parse_key_string_handles_atomic_named_modified_and_sequence() {
        // Atomic char
        let v = parse_key_string("a").unwrap();
        assert_eq!(v, vec![char_event('a')]);

        // Named key
        let v = parse_key_string("Enter").unwrap();
        assert_eq!(v, vec![KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)]);

        // Modified
        let v = parse_key_string("Alt+c").unwrap();
        assert_eq!(
            v,
            vec![KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT)]
        );

        // Sequence
        let v = parse_key_string("gr").unwrap();
        assert_eq!(v, vec![char_event('g'), char_event('r')]);

        // Empty rejected
        assert!(parse_key_string("").is_err());
    }

    #[test]
    fn override_rebinds_archive_to_e_and_evicts_default_e() {
        // The bead's TDD anchor: rebinding `archive` to `e` must put
        // `e -> Archive` in the resolved table, and the prior `e`
        // owner (`draft_edit`) must be either gone or reassigned. We
        // assert the override sticks AND the prior default `a` no
        // longer maps to Archive.
        let mut overrides = BTreeMap::new();
        overrides.insert("archive".to_string(), "e".to_string());
        // Also rebind `draft_edit` so `e` is free for `archive` —
        // otherwise the conflict check fires (see next test).
        overrides.insert("draft_edit".to_string(), "E".to_string());

        let map = resolve_keymap(&overrides).expect("overrides resolve");

        // `e` now triggers Archive.
        assert_eq!(
            map.lookup_single(char_event('e')),
            Some(Action::Archive),
            "override should remap 'e' to Archive"
        );

        // The default `a` no longer triggers Archive (it's unbound).
        assert_eq!(
            map.lookup_single(char_event('a')),
            None,
            "default 'a' should no longer trigger Archive after rebind"
        );

        // `draft_edit` moved to `E`.
        assert_eq!(map.lookup_single(char_event('E')), Some(Action::DraftEdit),);
    }

    #[test]
    fn conflict_detection_rejects_two_actions_on_one_key() {
        // Rebind `archive` to `e` without freeing the default `e`
        // (DraftEdit). resolve_keymap must reject with a structured
        // error naming both action keys.
        let mut overrides = BTreeMap::new();
        overrides.insert("archive".to_string(), "e".to_string());

        let err = resolve_keymap(&overrides).expect_err("conflict expected");
        match err {
            VulthorError::KeybindingConflict {
                key,
                action_a,
                action_b,
            } => {
                assert_eq!(key, "e");
                let mut names = [action_a, action_b];
                names.sort();
                assert_eq!(names, ["archive".to_string(), "draft_edit".to_string()]);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn unknown_action_name_is_rejected() {
        let mut overrides = BTreeMap::new();
        overrides.insert("teleport".to_string(), "t".to_string());
        let err = resolve_keymap(&overrides).expect_err("unknown action");
        match err {
            VulthorError::KeybindingUnknownAction { action } => {
                assert_eq!(action, "teleport");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn lookup_normalizes_shift_on_uppercase_letters() {
        // Some terminals report `R` as Char('R') + SHIFT, others as
        // Char('R') + NONE. The map stores NONE; lookup must hit
        // both ways so DEFAULT_KEYMAP entries like `R -> ReplyLater`
        // work portably.
        let map = resolve_keymap(&BTreeMap::new()).unwrap();
        let bare = KeyEvent::new(KeyCode::Char('R'), KeyModifiers::NONE);
        let with_shift = KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT);
        assert_eq!(map.lookup_single(bare), Some(Action::ReplyLater));
        assert_eq!(map.lookup_single(with_shift), Some(Action::ReplyLater));
    }

    #[test]
    fn sequences_resolve_into_sequence_table() {
        let map = resolve_keymap(&BTreeMap::new()).unwrap();
        let gr = vec![char_event('g'), char_event('r')];
        assert_eq!(map.lookup_sequence(&gr), Some(Action::Reply));
        // `gg` jumps to top.
        let gg = vec![char_event('g'), char_event('g')];
        assert_eq!(map.lookup_sequence(&gg), Some(Action::JumpTop));
    }

    #[test]
    fn default_keymap_binds_arrows_and_page_keys_alongside_hjkl() {
        // The bead's central acceptance: arrow `Down`/`Up` and
        // `PageDown`/`PageUp` must resolve through `DEFAULT_KEYMAP` so
        // `[keybindings]` overrides reach them. Both the j/k vim
        // bindings AND the corresponding arrow keys must hit MoveDown /
        // MoveUp respectively.
        let map = resolve_keymap(&BTreeMap::new()).unwrap();
        let j = char_event('j');
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(map.lookup_single(j), Some(Action::MoveDown));
        assert_eq!(
            map.lookup_single(down),
            Some(Action::MoveDown),
            "arrow Down must resolve to MoveDown (the bead's bypass fix)",
        );

        let k = char_event('k');
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(map.lookup_single(k), Some(Action::MoveUp));
        assert_eq!(map.lookup_single(up), Some(Action::MoveUp));

        let pd = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        let pu = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(map.lookup_single(pd), Some(Action::PageDown));
        assert_eq!(map.lookup_single(pu), Some(Action::PageUp));
    }

    #[test]
    fn override_retires_every_default_key_for_action() {
        // Rebinding `move_down` to `x` must retire BOTH default keys
        // for MoveDown — the vim `j` and the arrow `Down`. Otherwise
        // the user's mental model breaks: they explicitly renamed the
        // action, but ghosts of the old bindings keep firing.
        let mut overrides = BTreeMap::new();
        overrides.insert("move_down".to_string(), "x".to_string());

        let map = resolve_keymap(&overrides).expect("override resolves");

        assert_eq!(
            map.lookup_single(char_event('x')),
            Some(Action::MoveDown),
            "user-chosen 'x' must drive MoveDown",
        );
        assert_eq!(
            map.lookup_single(char_event('j')),
            None,
            "default 'j' must be unbound after move_down is rebound",
        );
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            map.lookup_single(down),
            None,
            "default arrow Down must be unbound after move_down is rebound",
        );
    }

    /// vu-flu: the `o` key defaults to `Action::OpenAttachment` so the
    /// Content / Attachments panes can resolve "open the focused
    /// attachment" through the standard keymap dispatch.
    #[test]
    fn default_keymap_binds_o_to_open_attachment() {
        let map = resolve_keymap(&BTreeMap::new()).unwrap();
        assert_eq!(
            map.lookup_single(char_event('o')),
            Some(Action::OpenAttachment),
            "'o' must default to OpenAttachment",
        );
    }

    #[test]
    fn override_can_steal_pagedown_key_for_another_action() {
        // VISION.md §Configuration Schema promises every action is
        // rebindable from `[keybindings]`. The bead's TDD anchor
        // exercises this for the new PageDown key: a user can rehome
        // `jump_next_unread` to `PageDown` by first freeing the key
        // (`page_down = "Ctrl+d"`) and then claiming it.
        let mut overrides = BTreeMap::new();
        overrides.insert("page_down".to_string(), "Ctrl+d".to_string());
        overrides.insert("jump_next_unread".to_string(), "PageDown".to_string());

        let map = resolve_keymap(&overrides).expect("override resolves");

        let pd = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(
            map.lookup_single(pd),
            Some(Action::JumpNextUnread),
            "PageDown must fire JumpNextUnread after the rebind",
        );
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(
            map.lookup_single(ctrl_d),
            Some(Action::PageDown),
            "page_down moved to Ctrl+d",
        );
    }
}
