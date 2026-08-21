//! The pages the shell draws.
//!
//! Each is a module of `impl AppShell` methods rather than a type of its own:
//! a page reads most of the shell's state and writes some of it, so a
//! separate view would spend its whole existence proxying.

pub(super) mod calibrate;
pub(super) mod convert;
pub(super) mod history;
pub mod opportunities;
pub(super) mod settings;
pub(super) mod watchlist;
