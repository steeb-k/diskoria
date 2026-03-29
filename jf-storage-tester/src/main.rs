#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    // Dev: `run-dev.ps1` sets RUST_LOG (default jf_surface=debug). Release .exe has no console unless
    // launched from a terminal; use `set RUST_LOG=jf_surface=debug` before starting if needed.
    let _ = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("jf_surface=debug,warn"),
    )
    .format_timestamp_millis()
    .try_init();

    log::info!(
        target: "jf_surface",
        "JF Storage Tester {} starting (logging target 'jf_surface')",
        env!("CARGO_PKG_VERSION")
    );

    let icon = jf_storage_tester::window_icon_from_image_bytes(include_bytes!("../../appicon.ico"));

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("JF Storage Tester")
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([780.0, 580.0])
            .with_decorations(false)
            .with_icon(std::sync::Arc::new(icon)),
        ..Default::default()
    };

    eframe::run_native(
        "JF Storage Tester",
        native_options,
        Box::new(|cc| Ok(Box::new(jf_storage_tester::JfStorageTesterApp::new(cc)))),
    )
}
