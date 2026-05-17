//! Vulthor library surface.
//!
//! The runtime binary still lives in `main.rs`; this `lib.rs` re-declares
//! the module tree publicly so out-of-tree consumers — currently the
//! `benches/` criterion suite — can construct the same scanner / store /
//! sanitizer types the bin uses. Adding this lib target was driven by
//! vu-dcg: criterion benches are compiled as separate integration crates
//! and have no other path to internal types.

pub mod classifier;
pub mod components;
pub mod compose;
pub mod config;
pub mod crash;
pub mod doctor;
pub mod email;
pub mod error;
pub mod keymap;
pub mod layout;
pub mod log;
pub mod maildir;
pub mod sanitizer;
pub mod stats;
pub mod theme;
pub mod ui;
pub mod undo;
pub mod web;

#[cfg(test)]
mod test_fixtures;

#[cfg(test)]
mod integration_tests;

#[cfg(test)]
mod phase3_integration_tests;

#[cfg(test)]
mod phase4_integration_tests;
