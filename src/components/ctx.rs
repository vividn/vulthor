// Shared, read-only context passed to every component each tick.
//
// Components borrow from `Ctx`; they do not mutate it. Mutations to shared
// resources flow as messages to the owner (today: `AppRoot`).
// See DESIGN-COMPONENTS.md § "The Component trait" for the contract.

use crate::config::Config;
use crate::email::EmailStore;
use crate::theme::VulthorTheme;

/// Read-only context handed to every component each dispatch tick.
/// Holds borrowed handles to the shared resources (theme, config,
/// store); components observe these but never mutate them — state
/// changes flow as messages to the owner.
pub struct Ctx<'a> {
    /// Color palette / styling rules.
    pub theme: &'a VulthorTheme,
    /// Loaded user configuration.
    pub config: &'a Config,
    /// Snapshot of the email store taken under AppRoot's lock for
    /// this tick.
    pub store: &'a EmailStore,
}
