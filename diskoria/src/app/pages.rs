//! Central page draw functions, extracted from `app.rs` (Phase 3c).
//!
//! Each submodule adds methods to `crate::app::DiskoriaApp` via an inherent
//! `impl` block. Because these modules are descendants of `app`, they retain
//! access to `DiskoriaApp`'s private fields and helper methods — the split is
//! purely organizational, with no change in behavior or visibility.

mod destructive;
mod sector;
mod speed;
