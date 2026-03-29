//! JF Storage Tester — egui shell (layout phase).
//!
//! Style tokens and window chrome follow `rust-egui-winui-example` / `WINDOWS11_EGUI_STYLE_GUIDE.md`.

mod about;
mod app;
mod app_settings;
mod chrome;
mod focus;
mod modal_confirm;
mod partition_info;
mod detected_drive;
mod drive_enumeration;
pub mod surface_test;
pub mod speed_test;
pub mod destructive_test;
mod shortcuts;
mod smart_health;
mod theme;
mod update;
mod widgets;

pub use app::JfStorageTesterApp;
pub use chrome::window_icon_from_image_bytes;
