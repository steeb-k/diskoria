//! Theme / accent preferences — persisted under ProgramData (aligned with rust-egui-winui-example).

use std::path::PathBuf;

use egui::Color32;

// Diskoria accent palette (8 swatches)
pub const ACCENT_PALETTE: [Color32; 8] = [
    Color32::from_rgb(142, 68, 173),
    Color32::from_rgb(52, 152, 219),
    Color32::from_rgb(46, 204, 113),
    Color32::from_rgb(26, 188, 156),
    Color32::from_rgb(241, 196, 15),
    Color32::from_rgb(230, 126, 34),
    Color32::from_rgb(231, 76, 60),
    Color32::from_rgb(160, 160, 160),
];

/// Short labels for palette swatch tooltips (dance-themed; colors match [`ACCENT_PALETTE`]).
pub const ACCENT_PALETTE_LABELS: [&str; 8] = [
    "Waltz",
    "Foxtrot",
    "Samba",
    "Bossa Nova",
    "Cha-Cha",
    "Flamenco",
    "Tango",
    "Quickstep",
];

#[derive(Clone, Copy, PartialEq)]
pub enum ThemePref {
    Auto,
    Dark,
    Light,
}

#[derive(Clone, Copy, PartialEq)]
pub enum AccentSourcePref {
    Windows,
    Palette,
}

#[derive(Clone)]
pub struct Settings {
    pub theme: ThemePref,
    pub accent_source: AccentSourcePref,
    pub accent_palette_idx: usize,
    pub accent_use_custom: bool,
    pub accent_custom_hex: String,
    /// Show the full-window PASS / WARN / FAIL result overlay when a sector or
    /// destructive test finds a bad block or finishes.
    pub show_test_result_overlays: bool,
    // Monitoring settings
    pub monitoring_enabled: bool,
    /// Closing the last window hides it to the tray (app keeps running and
    /// monitoring) instead of quitting. When the settings file has no entry —
    /// a fresh profile — the default comes from [`crate::install_mode`]: ON for
    /// installed builds, OFF for the portable exe. See [`load_settings`].
    pub close_to_tray: bool,
    pub poll_interval_mins: u8,
    pub alert_temp_warn: i32,
    pub alert_temp_critical: i32,
    pub alert_wear_threshold: u8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePref::Auto,
            accent_source: AccentSourcePref::Windows,
            accent_palette_idx: 0,
            accent_use_custom: false,
            accent_custom_hex: "#8E44AD".to_string(),
            show_test_result_overlays: true,
            monitoring_enabled: true,
            // Deliberately OS-independent so `Default` stays pure; the real
            // per-install default is applied in `load_settings`.
            close_to_tray: true,
            poll_interval_mins: 3,
            alert_temp_warn: 60,
            alert_temp_critical: 70,
            alert_wear_threshold: 90,
        }
    }
}

fn settings_path() -> PathBuf {
    crate::paths::settings_file()
}

pub fn load_settings() -> Settings {
    // Seed the tray behaviour from how this build was distributed. A `close_to_tray=`
    // line below overrides it, so this only applies until the user first touches the
    // toggle. (The 1.6.0 `minimize_to_tray=` key was written but never read, so it is
    // deliberately ignored here — every profile re-derives the default once.)
    let mut s = Settings {
        close_to_tray: crate::install_mode::default_close_to_tray(),
        ..Settings::default()
    };
    if let Ok(text) = std::fs::read_to_string(settings_path()) {
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("theme=") {
                s.theme = match v.trim() {
                    "dark" => ThemePref::Dark,
                    "light" => ThemePref::Light,
                    _ => ThemePref::Auto,
                };
            } else if let Some(v) = line.strip_prefix("accent_source=") {
                s.accent_source = match v.trim() {
                    "palette" => AccentSourcePref::Palette,
                    _ => AccentSourcePref::Windows,
                };
            } else if let Some(v) = line.strip_prefix("accent_palette_idx=") {
                if let Ok(parsed) = v.trim().parse::<usize>() {
                    s.accent_palette_idx = parsed.min(7);
                }
            } else if let Some(v) = line.strip_prefix("accent_use_custom=") {
                s.accent_use_custom = v.trim() == "true";
            } else if let Some(v) = line.strip_prefix("accent_custom_hex=") {
                s.accent_custom_hex = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("show_test_result_overlays=") {
                s.show_test_result_overlays = v.trim() != "false";
            } else if let Some(v) = line.strip_prefix("monitoring_enabled=") {
                s.monitoring_enabled = v.trim() != "false";
            } else if let Some(v) = line.strip_prefix("close_to_tray=") {
                s.close_to_tray = v.trim() != "false";
            } else if let Some(v) = line.strip_prefix("poll_interval_mins=") {
                if let Ok(n) = v.trim().parse::<u8>() { s.poll_interval_mins = n.clamp(1, 60); }
            } else if let Some(v) = line.strip_prefix("alert_temp_warn=") {
                if let Ok(n) = v.trim().parse::<i32>() { s.alert_temp_warn = n; }
            } else if let Some(v) = line.strip_prefix("alert_temp_critical=") {
                if let Ok(n) = v.trim().parse::<i32>() { s.alert_temp_critical = n; }
            } else if let Some(v) = line.strip_prefix("alert_wear_threshold=") {
                if let Ok(n) = v.trim().parse::<u8>() { s.alert_wear_threshold = n; }
            }
        }
    }
    s
}

pub fn save_settings(s: &Settings) {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let theme_s = match s.theme {
        ThemePref::Auto => "auto",
        ThemePref::Dark => "dark",
        ThemePref::Light => "light",
    };
    let accent_src = match s.accent_source {
        AccentSourcePref::Windows => "windows",
        AccentSourcePref::Palette => "palette",
    };
    let text = format!(
        "theme={}\naccent_source={}\naccent_palette_idx={}\naccent_use_custom={}\naccent_custom_hex={}\nshow_test_result_overlays={}\nmonitoring_enabled={}\nclose_to_tray={}\npoll_interval_mins={}\nalert_temp_warn={}\nalert_temp_critical={}\nalert_wear_threshold={}\n",
        theme_s,
        accent_src,
        s.accent_palette_idx,
        s.accent_use_custom,
        s.accent_custom_hex,
        s.show_test_result_overlays,
        s.monitoring_enabled,
        s.close_to_tray,
        s.poll_interval_mins,
        s.alert_temp_warn,
        s.alert_temp_critical,
        s.alert_wear_threshold,
    );
    let _ = std::fs::write(path, text);
}

pub fn parse_hex_color_6(hex: &str) -> Option<Color32> {
    let t = hex.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    if t.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&t[0..2], 16).ok()?;
    let g = u8::from_str_radix(&t[2..4], 16).ok()?;
    let b = u8::from_str_radix(&t[4..6], 16).ok()?;
    Some(Color32::from_rgb(r, g, b))
}

pub fn color_to_hex_6(c: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

pub fn accent_from_palette(palette_idx: usize, use_custom: bool, custom_hex: &str) -> Color32 {
    if use_custom {
        if let Some(c) = parse_hex_color_6(custom_hex) {
            return c;
        }
    }
    ACCENT_PALETTE[palette_idx.min(7)]
}

pub fn initial_accent_color(s: &Settings) -> Color32 {
    match s.accent_source {
        AccentSourcePref::Palette => accent_from_palette(
            s.accent_palette_idx,
            s.accent_use_custom,
            &s.accent_custom_hex,
        ),
        AccentSourcePref::Windows => crate::theme::windows_accent_color().unwrap_or(ACCENT_PALETTE[0]),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Color32;

    #[test]
    fn hex_parse_and_format() {
        assert_eq!(parse_hex_color_6("#FF8800"), Some(Color32::from_rgb(0xFF, 0x88, 0x00)));
        assert_eq!(parse_hex_color_6("00FF00"), Some(Color32::from_rgb(0, 0xFF, 0)));
        assert_eq!(parse_hex_color_6("xyz"), None);
        assert_eq!(parse_hex_color_6("#FFF"), None); // wrong length
        assert_eq!(color_to_hex_6(Color32::from_rgb(0x12, 0x34, 0x56)), "#123456");
    }

    #[test]
    fn accent_selection_rules() {
        // A valid custom hex wins over the palette index.
        assert_eq!(accent_from_palette(0, true, "#010203"), Color32::from_rgb(1, 2, 3));
        // An invalid custom hex falls back to the palette index.
        assert_eq!(accent_from_palette(2, true, "nope"), ACCENT_PALETTE[2]);
        // Not custom → palette index, clamped to the last swatch.
        assert_eq!(accent_from_palette(99, false, ""), ACCENT_PALETTE[7]);
    }

    #[cfg(windows)]
    #[test]
    fn settings_save_load_roundtrip() {
        // Point PROGRAMDATA at a temp dir so `settings_path()` stays hermetic.
        // No other unit test reads PROGRAMDATA, so the process-global env write
        // is safe here.
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("PROGRAMDATA", dir.path());

        let s = Settings {
            theme: ThemePref::Light,
            accent_source: AccentSourcePref::Palette,
            accent_palette_idx: 5,
            poll_interval_mins: 7,
            alert_temp_warn: 55,
            monitoring_enabled: false,
            show_test_result_overlays: false,
            close_to_tray: false,
            ..Settings::default()
        };
        save_settings(&s);

        let loaded = load_settings();
        assert!(loaded.theme == ThemePref::Light);
        assert!(matches!(loaded.accent_source, AccentSourcePref::Palette));
        assert_eq!(loaded.accent_palette_idx, 5);
        assert_eq!(loaded.poll_interval_mins, 7);
        assert_eq!(loaded.alert_temp_warn, 55);
        assert!(!loaded.monitoring_enabled);
        assert!(!loaded.show_test_result_overlays);
        assert!(!loaded.close_to_tray);
        // Default must round-trip as enabled.
        save_settings(&Settings::default());
        let loaded = load_settings();
        assert!(loaded.show_test_result_overlays);
        assert!(loaded.close_to_tray);

        // A settings file predating `close_to_tray` (or hand-edited to drop it)
        // must re-derive the default from the install mode rather than inheriting
        // `Settings::default()`. Asserted here rather than in its own #[test] so
        // the process-global PROGRAMDATA write above isn't raced by a parallel test.
        let path = settings_path();
        // 1.6.0-shaped file: retired `minimize_to_tray` key, no `close_to_tray`.
        std::fs::write(&path, "theme=dark\nminimize_to_tray=true\n").unwrap();
        assert_eq!(
            load_settings().close_to_tray,
            crate::install_mode::default_close_to_tray(),
        );
    }
}
