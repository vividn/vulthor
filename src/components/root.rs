// `AppRoot` — thin wrapper around today's `App`.
//
// Step 1 of the Phase-0.2 refactor (vu-m6s): the runtime entry point is
// declared so subsequent steps (vu-q31 onward) can migrate panes into it
// one at a time. No component owns its slice yet; `main.rs` still drives
// the old `App` directly. This file is dead code on purpose.
//
// See DESIGN-COMPONENTS.md § "Composition" for the target shape.

#![allow(dead_code)]

use std::collections::VecDeque;

use crate::app::App;

use super::Msg;

pub struct AppRoot {
    pub app: App,
    queue: VecDeque<Msg>,
}

impl AppRoot {
    pub fn new(app: App) -> Self {
        Self {
            app,
            queue: VecDeque::new(),
        }
    }

    pub fn enqueue(&mut self, msg: Msg) {
        self.queue.push_back(msg);
    }

    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }
}
