// Shared, read-only context passed to every component each tick.
//
// Components borrow from `Ctx`; they do not mutate it. Mutations to shared
// resources flow as messages to the owner (today: `AppRoot`).
// See DESIGN-COMPONENTS.md § "The Component trait" for the contract.

use crate::config::Config;
use crate::email::EmailStore;
use crate::theme::VulthorTheme;

pub struct Ctx<'a> {
    pub theme: &'a VulthorTheme,
    pub config: &'a Config,
    pub store: &'a EmailStore,
}
