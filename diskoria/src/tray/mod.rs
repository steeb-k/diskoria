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

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::TrayManager;
