//! Pro-Monitoring: system tray.
//!
//! Windows (`windows.rs`): the tray-icon crate — one app icon plus one
//! color-coded thermometer icon per internal drive, with flyout/context-menu
//! events routed through the winit proxy.
//!
//! Linux (`linux.rs`): a single aggregated StatusNotifierItem via ksni (pure
//! D-Bus, own service thread — no GTK loop to reconcile with winit). The item
//! shows the hottest drive's thermometer, lists every drive's temperature in
//! the tooltip, carries Open/New Window/Quit in its D-Bus menu, and flags
//! alerts through the SNI NeedsAttention status. Per-drive icons and the
//! hover flyout stay Windows-only — most desktops collapse or hide extra
//! items, and Wayland offers no icon geometry to anchor a flyout to.

pub(crate) const ICON_SIZE: u32 = 32;

pub(crate) fn temp_color(temp_c: Option<i32>) -> [u8; 3] {
    match temp_c {
        None => [128, 128, 128],          // gray — no reading
        Some(t) if t < 45 => [39, 174, 96],   // #27AE60 green
        Some(t) if t < 60 => [241, 196, 15],  // #F1C40F yellow
        Some(t) if t < 70 => [230, 126, 34],  // #E67E22 orange
        _ => [231, 76, 60],                    // #E74C3C red
    }
}

/// Render the 32×32 RGBA thermometer for the given temperature. Shared pixel
/// source for both platforms' icons.
pub(crate) fn render_thermometer_rgba(temp_c: Option<i32>) -> Vec<u8> {
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
    pixels
}

/// The app-level tray icon as 32×32 RGBA.
///
/// `appicon2.ico`, not `trayicon.ico`: every frame of the latter is a
/// green/yellow placeholder (KI-41), which is what the Windows tray icon has
/// been drawing. Falls back to a solid accent square if decoding fails.
pub(crate) fn app_icon_rgba() -> Vec<u8> {
    static APP_ICO: &[u8] = include_bytes!("../../../assets/appicon2.ico");

    if let Ok(img) = image::load_from_memory(APP_ICO) {
        let rgba = img
            .resize(ICON_SIZE, ICON_SIZE, image::imageops::FilterType::Lanczos3)
            .into_rgba8();
        if rgba.dimensions() == (ICON_SIZE, ICON_SIZE) {
            return rgba.into_raw();
        }
    }
    (0..ICON_SIZE * ICON_SIZE)
        .flat_map(|_| [61u8, 90, 128, 255])
        .collect()
}

/// What the single Linux tray item should show: a drive thermometer once
/// something reports a temperature, otherwise the app icon.
///
/// Windows carries a separate app icon alongside the per-drive thermometers;
/// with one aggregated item there is nothing to show when no drive reports a
/// temperature — a gray thermometer just looked broken.
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn tray_icon_rgba(hottest: Option<i32>) -> Vec<u8> {
    match hottest {
        Some(t) => render_thermometer_rgba(Some(t)),
        None => app_icon_rgba(),
    }
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::TrayManager;

#[cfg(test)]
mod tests {
    use super::*;

    const PIXELS: usize = (ICON_SIZE * ICON_SIZE * 4) as usize;

    #[test]
    fn app_icon_decodes_to_a_full_rgba_buffer() {
        let icon = app_icon_rgba();
        assert_eq!(icon.len(), PIXELS);
        assert!(
            icon.chunks_exact(4).any(|px| px[3] > 0),
            "app icon must not be fully transparent"
        );
    }

    /// With nothing reporting a temperature the item shows the app icon
    /// rather than an empty (gray) thermometer.
    #[test]
    fn no_temperature_falls_back_to_the_app_icon() {
        assert_eq!(tray_icon_rgba(None), app_icon_rgba());
        assert_ne!(tray_icon_rgba(Some(48)), app_icon_rgba());
        assert_eq!(tray_icon_rgba(Some(48)), render_thermometer_rgba(Some(48)));
    }

    #[test]
    fn thermometer_colour_tracks_the_temperature() {
        assert_eq!(temp_color(Some(30)), [39, 174, 96]);
        assert_eq!(temp_color(Some(65)), [230, 126, 34]);
        assert_eq!(temp_color(Some(80)), [231, 76, 60]);
        assert_eq!(temp_color(None), [128, 128, 128]);
    }
}

#[cfg(test)]
mod icon_dump {
    /// Diagnostic: writes the tray artwork to disk so it can be eyeballed.
    /// `DISKORIA_ICON_DUMP_DIR` chooses where (default `/tmp`).
    #[test]
    #[ignore = "writes PNGs for visual inspection"]
    fn dump_tray_icons() {
        let dir = std::env::var("DISKORIA_ICON_DUMP_DIR").unwrap_or_else(|_| "/tmp".into());
        for (name, rgba) in [
            ("tray-app", super::app_icon_rgba()),
            ("tray-cool", super::render_thermometer_rgba(Some(30))),
            ("tray-warm", super::render_thermometer_rgba(Some(65))),
        ] {
            let img: image::RgbaImage =
                image::ImageBuffer::from_raw(super::ICON_SIZE, super::ICON_SIZE, rgba)
                    .expect("icon buffer");
            let path = format!("{dir}/{name}.png");
            img.save(&path).expect("write png");
            println!("wrote {path}");
        }
    }
}
