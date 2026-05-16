// Shared, read-only context passed to every component each tick.
//
// Components borrow from `Ctx`; they do not mutate it. Mutations to shared
// resources flow as messages to the owner (today: `AppRoot`).
// See DESIGN-COMPONENTS.md § "The Component trait" for the contract.
//
// Phase 0.2.3 (vu-3yj) extends Ctx with `view` and `folder_index` so
// `MessagesComponent` and `ContentComponent` can render context-aware
// (e.g. the messages pane shows the *selected* folder's emails in
// `FolderMessages` view but the *current* folder's emails everywhere
// else — that selection needs `view` + `folder_index`).

use crate::app::View;
use crate::config::Config;
use crate::email::EmailStore;
use crate::theme::VulthorTheme;

pub struct Ctx<'a> {
    pub theme: &'a VulthorTheme,
    pub config: &'a Config,
    pub store: &'a EmailStore,
    /// Current top-level view (which panes are visible / how the
    /// layout is composed). Read by components to switch render mode.
    pub view: View,
    /// Folder selection in the folders pane. Owned by
    /// `FoldersComponent`; published into `Ctx` so other components
    /// (notably Messages) can resolve "which folder to display" in
    /// `FolderMessages` view without reaching across components.
    pub folder_index: usize,
}
