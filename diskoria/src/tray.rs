//! Pro-Monitoring: system tray icon management.
//!
//! - One app-level tray icon with a right-click context menu ("Open Diskoria", "Quit").
//! - One per-internal-drive tray icon showing a color-coded thermometer.
//!
//! All tray-icon and menu events are forwarded to the winit event loop via
//! `EventLoopProxy<UserEvent>`.

use std::time::{Duration, Instant};

use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::event_loop::EventLoopProxy;

use crate::detected_drive::DetectedDrive;
use crate::UserEvent;

// ── Alert flash constants ─────────────────────────────────────────────────────

const FLASH_WARN_MS: u64 = 800;
const FLASH_CRIT_MS: u64 = 200;
const FLASH_EXPIRE_SECS: u64 = 300; // auto-stop after 5 minutes

struct DriveFlash {
    is_critical: bool,
    flash_on: bool,
    last_toggle: Instant,
    started_at: Instant,
}

// ── Temperature color bands ───────────────────────────────────────────────────

fn temp_color(temp_c: Option<i32>) -> [u8; 3] {
    match temp_c {
        None => [128, 128, 128],          // gray — no reading
        Some(t) if t < 45 => [39, 174, 96],   // #27AE60 green
        Some(t) if t < 60 => [241, 196, 15],  // #F1C40F yellow
        Some(t) if t < 70 => [230, 126, 34],  // #E67E22 orange
        _ => [231, 76, 60],                    // #E74C3C red
    }
}

// ── Icon rendering ────────────────────────────────────────────────────────────

const ICON_SIZE: u32 = 32;

/// Render a 32×32 RGBA alert icon: solid colored background with a white "!" mark.
pub fn render_alert_icon(is_critical: bool) -> Icon {
    let [r, g, b]: [u8; 3] = if is_critical {
        [231, 76, 60]   // red
    } else {
        [241, 196, 15]  // #F1C40F yellow
    };
    // White "!" is hard to read on bright yellow — use black for warnings.
    let [fr, fg, fb]: [u8; 3] = if is_critical { [255, 255, 255] } else { [0, 0, 0] };

    let mut pixels = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];

    for py in 0..ICON_SIZE as i32 {
        for px in 0..ICON_SIZE as i32 {
            let idx = ((py * ICON_SIZE as i32 + px) * 4) as usize;

            // Solid colored background
            pixels[idx]     = r;
            pixels[idx + 1] = g;
            pixels[idx + 2] = b;
            pixels[idx + 3] = 255;

            // "!" mark — stem: x=[14..17], y=[6..20]; dot: x=[13..18], y=[23..27]
            let in_stem = (14..=17).contains(&px) && (6..=20).contains(&py);
            let in_dot  = (13..=18).contains(&px) && (23..=27).contains(&py);
            if in_stem || in_dot {
                pixels[idx]     = fr;
                pixels[idx + 1] = fg;
                pixels[idx + 2] = fb;
                pixels[idx + 3] = 255;
            }
        }
    }

    Icon::from_rgba(pixels, ICON_SIZE, ICON_SIZE).expect("alert icon RGBA valid")
}

/// Render a 32×32 RGBA thermometer icon for the given temperature.
pub fn render_thermometer_icon(temp_c: Option<i32>) -> Icon {
    let [r, g, b] = temp_color(temp_c);
    let mut pixels = vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize];

    // Thermometer geometry (pixel coords in 32×32 grid):
    // Bulb: circle at (16, 25) radius 5
    // Stem: rectangle x=[14..18], y=[6..22]
    // Fill level: maps temperature 0–100°C to stem height (from bottom of stem up)

    let bulb_cx = 16i32;
    let bulb_cy = 25i32;
    let bulb_r = 5i32;
    let stem_x1 = 14i32;
    let stem_x2 = 18i32;
    let stem_top = 6i32;
    let stem_bot = 22i32;

    // Fill height in stem: at 0°C → empty, at 100°C → full stem
    let fill_fraction = temp_c.map(|t| (t.clamp(0, 100) as f32) / 100.0).unwrap_or(0.5);
    let stem_h = (stem_bot - stem_top) as f32;
    let fill_top = stem_bot - (fill_fraction * stem_h) as i32;

    let bg = [220u8, 220, 220, 200u8]; // light gray stem outline, visible on dark taskbars

    for py in 0..ICON_SIZE as i32 {
        for px in 0..ICON_SIZE as i32 {
            let idx = ((py * ICON_SIZE as i32 + px) * 4) as usize;

            // Bulb (filled circle)
            let dx = px - bulb_cx;
            let dy = py - bulb_cy;
            let in_bulb = dx * dx + dy * dy <= bulb_r * bulb_r;

            // Stem outline
            let in_stem = px >= stem_x1 && px <= stem_x2 && py >= stem_top && py <= stem_bot;

            // Fill inside stem (above bulb center)
            let in_fill_stem = px > stem_x1 && px < stem_x2
                && py >= fill_top && py <= stem_bot
                && in_stem;

            if in_bulb || in_fill_stem {
                pixels[idx] = r;
                pixels[idx + 1] = g;
                pixels[idx + 2] = b;
                pixels[idx + 3] = 255;
            } else if in_stem {
                // Stem outline (dark frame)
                pixels[idx] = bg[0];
                pixels[idx + 1] = bg[1];
                pixels[idx + 2] = bg[2];
                pixels[idx + 3] = 200;
            } else {
                pixels[idx] = 0;
                pixels[idx + 1] = 0;
                pixels[idx + 2] = 0;
                pixels[idx + 3] = 0; // transparent
            }
        }
    }

    Icon::from_rgba(pixels, ICON_SIZE, ICON_SIZE)
        .expect("thermometer icon RGBA valid")
}

/// Load the bundled trayicon.ico as the app tray icon, falling back to a solid rectangle.
fn app_icon() -> Icon {
    static TRAY_ICO: &[u8] = include_bytes!("../../trayicon.ico");

    if let Ok(img) = image::load_from_memory(TRAY_ICO) {
        let rgba = img.resize(ICON_SIZE, ICON_SIZE, image::imageops::FilterType::Lanczos3)
            .into_rgba8();
        let (w, h) = rgba.dimensions();
        if let Ok(icon) = Icon::from_rgba(rgba.into_raw(), w, h) {
            return icon;
        }
    }

    // Fallback: solid #3D5A80 rectangle.
    let pixels: Vec<u8> = (0..ICON_SIZE * ICON_SIZE)
        .flat_map(|_| [61u8, 90, 128, 255])
        .collect();
    Icon::from_rgba(pixels, ICON_SIZE, ICON_SIZE).expect("app icon valid")
}

// ── TrayManager ───────────────────────────────────────────────────────────────

pub struct TrayManager {
    /// App-level icon (always visible, has context menu). Kept alive to prevent tray removal.
    #[allow(dead_code)]
    app_icon: TrayIcon,
    /// One icon per internal drive (NVMe / SATA), keyed by serial number.
    drive_icons: Vec<(String, TrayIcon)>,
    /// Cached temperature per serial for icon update decisions.
    last_temps: std::collections::HashMap<String, Option<i32>>,
    /// Active alert flash states per serial.
    flash_states: std::collections::HashMap<String, DriveFlash>,
}

impl TrayManager {
    /// Create the app-level tray icon and register event handlers.
    /// Must be called on the same thread as the winit event loop.
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Option<Self> {
        let icon = app_icon();

        let app_icon = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip("Diskoria")
            .with_menu_on_left_click(false)
            .build()
            .map_err(|e| log::error!(target: "diskoria::tray", "Failed to create app tray icon: {e}"))
            .ok()?;

        // Forward tray icon events to the event loop.
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            let _ = proxy.send_event(UserEvent::TrayIconEvent(e));
        }));

        log::info!(target: "diskoria::tray", "App tray icon created");

        Some(TrayManager {
            app_icon,
            drive_icons: Vec::new(),
            last_temps: std::collections::HashMap::new(),
            flash_states: std::collections::HashMap::new(),
        })
    }

    /// Rebuild per-drive tray icons from the current drive list.
    /// Only includes NVMe and SATA drives (no USB/UFS).
    pub fn rebuild_drive_icons(&mut self, drives: &[DetectedDrive]) {
        use crate::detected_drive::BusKind;

        // Drop existing drive icons first (removes them from the tray).
        self.drive_icons.clear();
        self.last_temps.clear();
        self.flash_states.clear();

        for drive in drives {
            if !matches!(drive.bus, BusKind::Nvme | BusKind::Sata) {
                continue;
            }

            let icon = render_thermometer_icon(None);

            match TrayIconBuilder::new()
                .with_icon(icon)
                .with_menu_on_left_click(false)
                .build()
            {
                Ok(tray_icon) => {
                    log::info!(
                        target: "diskoria::tray",
                        "Drive tray icon created for {} ({})",
                        drive.model,
                        drive.serial
                    );
                    self.drive_icons.push((drive.serial.clone(), tray_icon));
                    self.last_temps.insert(drive.serial.clone(), None);
                }
                Err(e) => {
                    log::warn!(
                        target: "diskoria::tray",
                        "Failed to create tray icon for drive {}: {e}",
                        drive.serial
                    );
                }
            }
        }
    }

    /// Update a drive's tray icon color to reflect a new temperature reading.
    pub fn update_drive_icon(&mut self, serial: &str, temp_c: Option<i32>) {
        // Skip re-render if temperature hasn't changed.
        if self.last_temps.get(serial) == Some(&temp_c) {
            return;
        }
        self.last_temps.insert(serial.to_string(), temp_c);

        // Don't clobber the alert flash icon while the drive is flashing.
        if self.flash_states.contains_key(serial) {
            return;
        }

        if let Some((_, tray_icon)) = self.drive_icons.iter().find(|(s, _)| s == serial) {
            let new_icon = render_thermometer_icon(temp_c);
            if let Err(e) = tray_icon.set_icon(Some(new_icon)) {
                log::warn!(target: "diskoria::tray", "set_icon failed for {serial}: {e}");
            }
        }
    }

    /// Start (or reset) alert flashing for a drive.
    /// Warning flashes every 800 ms; Critical every 200 ms.  Auto-stops after 5 minutes.
    pub fn set_drive_alert(&mut self, serial: &str, is_critical: bool) {
        let now = Instant::now();
        self.flash_states.insert(serial.to_string(), DriveFlash {
            is_critical,
            flash_on: true,
            last_toggle: now,
            started_at: now,
        });
        // Show the alert icon immediately.
        if let Some((_, tray_icon)) = self.drive_icons.iter().find(|(s, _)| s == serial) {
            let icon = render_alert_icon(is_critical);
            let _ = tray_icon.set_icon(Some(icon));
        }
    }

    /// Advance all active flash states.  Updates tray icons as needed and returns
    /// the shortest Duration until the next toggle (for `ControlFlow::WaitUntil`),
    /// or `None` when no drives are flashing.
    pub fn tick_flash(&mut self) -> Option<Duration> {
        let now = Instant::now();
        let mut min_wait: Option<Duration> = None;
        let mut to_remove: Vec<String> = Vec::new();
        // (serial, show_alert_icon, is_critical)
        let mut icon_updates: Vec<(String, bool, bool)> = Vec::new();

        for (serial, flash) in &mut self.flash_states {
            if now.duration_since(flash.started_at).as_secs() >= FLASH_EXPIRE_SECS {
                to_remove.push(serial.clone());
                icon_updates.push((serial.clone(), false, flash.is_critical));
                continue;
            }

            let interval = Duration::from_millis(
                if flash.is_critical { FLASH_CRIT_MS } else { FLASH_WARN_MS },
            );
            let elapsed = now.duration_since(flash.last_toggle);

            if elapsed >= interval {
                flash.flash_on = !flash.flash_on;
                flash.last_toggle = now;
                icon_updates.push((serial.clone(), flash.flash_on, flash.is_critical));
                min_wait = Some(min_wait.map_or(interval, |d: Duration| d.min(interval)));
            } else {
                let remaining = interval - elapsed;
                min_wait = Some(min_wait.map_or(remaining, |d: Duration| d.min(remaining)));
            }
        }

        for serial in &to_remove {
            self.flash_states.remove(serial);
        }

        for (serial, show_alert, is_critical) in icon_updates {
            let temp = self.last_temps.get(&serial).copied().flatten();
            if let Some((_, tray_icon)) = self.drive_icons.iter().find(|(s, _)| s == &serial) {
                let icon = if show_alert {
                    render_alert_icon(is_critical)
                } else {
                    render_thermometer_icon(temp)
                };
                let _ = tray_icon.set_icon(Some(icon));
            }
        }

        min_wait
    }

    /// Returns true when any drive icon is currently flashing an alert.
    pub fn is_flashing(&self) -> bool {
        !self.flash_states.is_empty()
    }

    /// Returns the drive serial and icon rect when the cursor enters or moves over a drive icon.
    /// Handles both Enter and Move so the flyout re-opens even when tray-icon's internal
    /// cursor_inside flag is stuck (which happens when WM_MOUSELEAVE never fired).
    pub fn drive_serial_for_hover_event(&self, event: &TrayIconEvent) -> Option<(String, tray_icon::Rect)> {
        let (id, rect) = match event {
            TrayIconEvent::Enter { id, rect, .. } | TrayIconEvent::Move { id, rect, .. } => (id, rect),
            _ => return None,
        };
        self.drive_icons
            .iter()
            .find(|(_, icon)| icon.id() == id)
            .map(|(serial, _)| (serial.clone(), *rect))
    }

    /// Returns true when the cursor leaves any drive icon.
    pub fn is_drive_leave_event(&self, event: &TrayIconEvent) -> bool {
        let id = match event {
            TrayIconEvent::Leave { id, .. } => id,
            _ => return false,
        };
        self.drive_icons.iter().any(|(_, icon)| icon.id() == id)
    }

    /// Returns true when the app icon is left-clicked (not a drive icon).
    pub fn is_app_icon_left_click(&self, event: &TrayIconEvent) -> bool {
        match event {
            TrayIconEvent::Click { id, button, button_state, .. }
                if *button == MouseButton::Left && *button_state == MouseButtonState::Up =>
            {
                self.drive_icons.iter().all(|(_, icon)| icon.id() != id)
            }
            _ => false,
        }
    }

    /// Returns `(serial, cursor_pos)` when a drive icon that currently has an active
    /// alert flash is right-clicked.  Only fires for flashing drives.
    pub fn alert_drive_right_click(
        &self,
        event: &TrayIconEvent,
    ) -> Option<(String, winit::dpi::PhysicalPosition<f64>)> {
        match event {
            TrayIconEvent::Click { id, position, button, button_state, .. }
                if *button == MouseButton::Right && *button_state == MouseButtonState::Up =>
            {
                self.drive_icons
                    .iter()
                    .find(|(serial, icon)| icon.id() == id && self.flash_states.contains_key(serial))
                    .map(|(serial, _)| (serial.clone(), *position))
            }
            _ => None,
        }
    }

    /// Stop flashing a drive and restore its thermometer icon.
    pub fn clear_drive_flash(&mut self, serial: &str) {
        self.flash_states.remove(serial);
        let temp = self.last_temps.get(serial).copied().flatten();
        if let Some((_, tray_icon)) = self.drive_icons.iter().find(|(s, _)| s == serial) {
            let _ = tray_icon.set_icon(Some(render_thermometer_icon(temp)));
        }
    }

    /// Returns the cursor position (physical pixels) when the app icon is right-clicked.
    pub fn app_icon_right_click_pos(
        &self,
        event: &TrayIconEvent,
    ) -> Option<winit::dpi::PhysicalPosition<f64>> {
        match event {
            TrayIconEvent::Click { id, position, button, button_state, .. }
                if *button == MouseButton::Right && *button_state == MouseButtonState::Up =>
            {
                if self.drive_icons.iter().all(|(_, icon)| icon.id() != id) {
                    Some(*position)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

