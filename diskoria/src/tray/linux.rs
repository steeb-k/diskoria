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

use std::sync::atomic::{AtomicBool, Ordering};

/// Is a StatusNotifierWatcher actually on the bus right now?
///
/// `assume_sni_available` keeps our service alive when no watcher exists yet
/// (KI-45), which means `TrayManager` being `Some` no longer implies a visible
/// icon. Hiding the last window to a tray that is not there would strand the
/// app with no way back — on Wayland the window is destroyed, not hidden — so
/// the close-to-tray decision asks this instead (KI-47).
///
/// Optimistic until told otherwise: ksni calls `watcher_offline` when
/// registration fails, and `watcher_online` when one appears.
static WATCHER_ONLINE: AtomicBool = AtomicBool::new(true);

/// Whether an SNI host is present to show our icon.
pub fn host_present() -> bool {
    WATCHER_ONLINE.load(Ordering::Relaxed)
}

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
        // Panels differ in what they surface on hover — some show ToolTip,
        // some only Title — so the hottest reading goes in both.
        match self.hottest() {
            Some(t) => format!("Diskoria — {t}°C"),
            None => "Diskoria".into(),
        }
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
        let rgba = super::tray_icon_rgba(self.hottest());
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

    /// The panel's StatusNotifierWatcher appeared (or came back). ksni
    /// re-registers the item for us; the host then re-reads every property, so
    /// the icon repaints itself with no work here.
    fn watcher_online(&self) {
        WATCHER_ONLINE.store(true, Ordering::Relaxed);
        log::info!(target: "diskoria::tray", "StatusNotifierWatcher online — tray item (re)registered");
    }

    /// The watcher is not on the bus. Returning `true` keeps the service loop
    /// alive so it can register once the watcher shows up — which is the whole
    /// point at login, when we start before the panel does (KI-45).
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        WATCHER_ONLINE.store(false, Ordering::Relaxed);
        log::info!(
            target: "diskoria::tray",
            "no StatusNotifierWatcher yet ({reason:?}) — waiting for the desktop to provide one"
        );
        true
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
            // Two lines per drive: model names are long and panels truncate a
            // single row, which would cut the temperature off the end.
            // Enabled on purpose — some panels dim or skip disabled entries,
            // and this is the one surface guaranteed to show the reading when
            // a bar renders no tooltip. Either line opens the app.
            for label in [
                d.model.trim().to_string(),
                match d.temp_c {
                    Some(t) => format!("    {t}°C"),
                    None => "    no reading".to_string(),
                },
            ] {
                items.push(
                    StandardItem {
                        label,
                        activate: Box::new(|t: &mut SniTray| {
                            let _ = t.proxy.send_event(UserEvent::ShowWindowRequested);
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }
        if !self.drives.is_empty() {
            items.push(MenuItem::Separator);
        }

        // Background collection: state only. Stopping lives on Quit, which
        // stops the collector as well as the app, and the durable off switch
        // is Settings — a second way to stop it from here would just be a
        // third place to look.
        if let Some(svc) = crate::service_control::status().filter(|s| s.installed) {
            items.push(
                StandardItem {
                    label: format!("Background service: {}", svc.summary()),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
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

/// Work for the tray thread. Every mutation of the SNI item goes through this
/// channel rather than being applied inline — see [`TrayManager`].
enum TrayCmd {
    Rebuild(Vec<(String, String)>),
    Temp(String, Option<i32>),
    Alert(bool),
    ClearAlert,
}

/// Handle to the tray, usable from the event-loop thread.
///
/// `ksni::blocking::Handle::update` is not a fire-and-forget send: it is a
/// `block_on` of an async-mutex acquire *plus* a oneshot round trip with the
/// D-Bus service loop, so the caller is parked until the tray service and the
/// panel on the other end are both done. Calling it from a winit callback put
/// unbounded D-Bus I/O on the UI thread, where it froze every window at once
/// for as long as the round trip took — once per drive per monitor tick
/// (KI-43). Commands are queued to a dedicated thread instead; the event loop
/// only ever does a non-blocking channel send.
pub struct TrayManager {
    tx: std::sync::mpsc::Sender<TrayCmd>,
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
        // `assume_sni_available` is what makes a login-time start reliable.
        // Without it, a missing watcher makes `spawn` fail outright, we store
        // `None`, and there is no service loop left to ever recover — so
        // starting one moment before the panel means no tray for the entire
        // session. With it, that case is routed to `watcher_offline`, the loop
        // runs, and ksni re-registers on the watcher's `NameOwnerChanged`
        // (KI-45).
        let spawned = (SniTray {
            proxy,
            drives: Vec::new(),
            alert: None,
        })
        .assume_sni_available(true)
        .spawn();
        if dropped {
            crate::elevation::restore_thread_privileges();
        }
        let handle = match spawned {
            Ok(handle) => handle,
            Err(e) => {
                log::warn!(target: "diskoria::tray", "no system tray available: {e}");
                return None;
            }
        };

        let (tx, rx) = std::sync::mpsc::channel::<TrayCmd>();
        // Owns the handle: when `TrayManager` drops, the channel closes, this
        // loop ends and the handle drops with it, removing the SNI item —
        // the same teardown as when the manager held the handle directly.
        std::thread::Builder::new()
            .name("diskoria-tray".into())
            .spawn(move || {
                for cmd in rx {
                    apply(&handle, cmd);
                }
                log::debug!(target: "diskoria::tray", "tray thread exiting");
            })
            .ok()?;

        Some(Self { tx })
    }

    /// Queue a tray mutation. A closed channel means the tray thread is gone
    /// (shutdown), which is not worth reporting per call.
    fn send(&self, cmd: TrayCmd) {
        let _ = self.tx.send(cmd);
    }

    /// Refresh the monitored-drive set (internal drives only, like Windows).
    pub fn rebuild_drive_icons(&mut self, drives: &[DetectedDrive]) {
        let list: Vec<(String, String)> = drives
            .iter()
            .filter(|d| matches!(d.bus, BusKind::Nvme | BusKind::Sata | BusKind::Ufs))
            .map(|d| (d.serial.clone(), d.model.clone()))
            .collect();
        log::debug!(target: "diskoria::tray", "rebuild_drive_icons: {} monitored drive(s)", list.len());
        self.send(TrayCmd::Rebuild(list));
    }

    pub fn update_drive_icon(&mut self, serial: &str, temp_c: Option<i32>) {
        self.send(TrayCmd::Temp(serial.to_string(), temp_c));
    }

    pub fn set_drive_alert(&mut self, _serial: &str, is_critical: bool) {
        self.send(TrayCmd::Alert(is_critical));
    }

    pub fn clear_drive_flash(&mut self, _serial: &str) {
        self.send(TrayCmd::ClearAlert);
    }

    /// SNI has no flash animation — attention is a status, not a timer.
    pub fn tick_flash(&mut self) -> Option<std::time::Duration> {
        None
    }

    pub fn is_flashing(&self) -> bool {
        false
    }
}

/// Apply one command to the SNI item. Runs on the tray thread only — every
/// `handle.update` in here blocks until the D-Bus service loop acknowledges.
fn apply(handle: &ksni::blocking::Handle<SniTray>, cmd: TrayCmd) {
    match cmd {
        TrayCmd::Rebuild(list) => {
            handle.update(|t| {
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
        TrayCmd::Temp(serial, temp_c) => {
            handle.update(move |t| {
                match t.drives.iter_mut().find(|d| d.serial == serial) {
                    Some(d) => {
                        log::debug!(target: "diskoria::tray", "icon temp {serial} = {temp_c:?}°C");
                        d.temp_c = temp_c;
                    }
                    // Would leave the thermometer stuck gray — the symptom of
                    // the drive list never reaching the tray.
                    None => log::warn!(
                        target: "diskoria::tray",
                        "temperature for unknown serial {serial}; tray has {} drive(s)",
                        t.drives.len()
                    ),
                }
            });
        }
        TrayCmd::Alert(is_critical) => {
            handle.update(move |t| {
                // Escalate but never downgrade an active critical alert.
                t.alert = Some(t.alert.unwrap_or(false) || is_critical);
            });
        }
        TrayCmd::ClearAlert => {
            handle.update(|t| t.alert = None);
        }
    }
}
