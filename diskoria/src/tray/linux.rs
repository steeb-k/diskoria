//! Linux tray: one aggregated StatusNotifierItem (ksni, D-Bus).
//!
//! Mirrors the Windows `TrayManager` surface the event loop uses —
//! `rebuild_drive_icons`, `update_drive_icon`, `set_drive_alert`,
//! `clear_drive_flash`, `tick_flash` — so the call sites stay shared. There
//! is no flashing on SNI; alerts set the `NeedsAttention` status (DEs
//! highlight the item) until cleared.

use winit::event_loop::EventLoopProxy;

use crate::detected_drive::{BusKind, DetectedDrive};
use crate::UserEvent;

use ksni::blocking::TrayMethods;

struct DriveTemp {
    serial: String,
    model: String,
    temp_c: Option<i32>,
}

struct SniTray {
    proxy: EventLoopProxy<UserEvent>,
    drives: Vec<DriveTemp>,
    /// `Some(is_critical)` while an alert wants attention.
    alert: Option<bool>,
}

impl SniTray {
    fn hottest(&self) -> Option<i32> {
        self.drives.iter().filter_map(|d| d.temp_c).max()
    }
}

impl ksni::Tray for SniTray {
    fn id(&self) -> String {
        "diskoria".into()
    }

    fn title(&self) -> String {
        "Diskoria".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn status(&self) -> ksni::Status {
        if self.alert.is_some() {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        // RGBA → ARGB32 network byte order.
        let rgba = super::render_thermometer_rgba(self.hottest());
        let mut data = Vec::with_capacity(rgba.len());
        for px in rgba.chunks_exact(4) {
            data.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
        }
        vec![ksni::Icon {
            width: super::ICON_SIZE as i32,
            height: super::ICON_SIZE as i32,
            data,
        }]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let description = if self.drives.is_empty() {
            "Drive monitoring".to_string()
        } else {
            self.drives
                .iter()
                .map(|d| match d.temp_c {
                    Some(t) => format!("{} — {t}°C", d.model.trim()),
                    None => format!("{} — n/a", d.model.trim()),
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        ksni::ToolTip {
            title: "Diskoria".into(),
            description,
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        log::debug!(target: "diskoria::tray", "tray activated (left click)");
        let _ = self.proxy.send_event(UserEvent::ShowWindowRequested);
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        log::debug!(target: "diskoria::tray", "tray secondary-activated (middle click)");
        let _ = self.proxy.send_event(UserEvent::OpenNewWindow);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let mut items: Vec<ksni::MenuItem<Self>> = Vec::new();

        // Per-drive status as disabled header items. The same text is in the
        // ToolTip, but whether a panel renders tooltips is up to the shell
        // (Quickshell/waybar configs often don't), while the menu is the one
        // surface every SNI host shows.
        for d in &self.drives {
            items.push(
                StandardItem {
                    label: match d.temp_c {
                        Some(t) => format!("{} — {t}°C", d.model.trim()),
                        None => format!("{} — no reading", d.model.trim()),
                    },
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        }
        if !self.drives.is_empty() {
            items.push(MenuItem::Separator);
        }

        items.extend(vec![
            StandardItem {
                label: "Open Diskoria".into(),
                activate: Box::new(|t: &mut SniTray| {
                    let _ = t.proxy.send_event(UserEvent::ShowWindowRequested);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "New Window".into(),
                activate: Box::new(|t: &mut SniTray| {
                    let _ = t.proxy.send_event(UserEvent::OpenNewWindow);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut SniTray| {
                    let _ = t.proxy.send_event(UserEvent::QuitRequested);
                }),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

pub struct TrayManager {
    handle: ksni::blocking::Handle<SniTray>,
}

impl TrayManager {
    /// Spawn the SNI service. `None` when no StatusNotifierWatcher is on the
    /// bus (no tray on this desktop — e.g. stock GNOME without the extension).
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Option<Self> {
        // Under pkexec the session bus refuses root, so hand ksni's worker
        // thread the invoking user's credentials: threads cloned from this one
        // inherit them, and the worker only ever talks D-Bus. Our own thread
        // takes root back immediately afterwards.
        let dropped = crate::elevation::drop_thread_privileges_to_session_user();
        let spawned = (SniTray {
            proxy,
            drives: Vec::new(),
            alert: None,
        })
        .spawn();
        if dropped {
            crate::elevation::restore_thread_privileges();
        }
        match spawned {
            Ok(handle) => Some(Self { handle }),
            Err(e) => {
                log::warn!(target: "diskoria::tray", "no system tray available: {e}");
                None
            }
        }
    }

    /// Refresh the monitored-drive set (internal drives only, like Windows).
    pub fn rebuild_drive_icons(&mut self, drives: &[DetectedDrive]) {
        let list: Vec<(String, String)> = drives
            .iter()
            .filter(|d| matches!(d.bus, BusKind::Nvme | BusKind::Sata | BusKind::Ufs))
            .map(|d| (d.serial.clone(), d.model.clone()))
            .collect();
        log::debug!(target: "diskoria::tray", "rebuild_drive_icons: {} monitored drive(s)", list.len());
        self.handle.update(|t| {
            // Keep temps for serials that survived the rebuild.
            let old: Vec<DriveTemp> = std::mem::take(&mut t.drives);
            t.drives = list
                .into_iter()
                .map(|(serial, model)| {
                    let temp_c = old
                        .iter()
                        .find(|o| o.serial == serial)
                        .and_then(|o| o.temp_c);
                    DriveTemp { serial, model, temp_c }
                })
                .collect();
        });
    }

    pub fn update_drive_icon(&mut self, serial: &str, temp_c: Option<i32>) {
        let serial = serial.to_string();
        self.handle.update(move |t| {
            match t.drives.iter_mut().find(|d| d.serial == serial) {
                Some(d) => {
                    log::debug!(target: "diskoria::tray", "icon temp {serial} = {temp_c:?}°C");
                    d.temp_c = temp_c;
                }
                // Would leave the thermometer stuck gray — the symptom of the
                // drive list never reaching the tray.
                None => log::warn!(
                    target: "diskoria::tray",
                    "temperature for unknown serial {serial}; tray has {} drive(s)",
                    t.drives.len()
                ),
            }
        });
    }

    pub fn set_drive_alert(&mut self, _serial: &str, is_critical: bool) {
        self.handle.update(move |t| {
            // Escalate but never downgrade an active critical alert.
            t.alert = Some(t.alert.unwrap_or(false) || is_critical);
        });
    }

    pub fn clear_drive_flash(&mut self, _serial: &str) {
        self.handle.update(|t| t.alert = None);
    }

    /// SNI has no flash animation — attention is a status, not a timer.
    pub fn tick_flash(&mut self) -> Option<std::time::Duration> {
        None
    }

    pub fn is_flashing(&self) -> bool {
        false
    }
}
