fn main() {
    // Bootstrap Icons WOFF2 → TTF for egui (MIT, https://icons.getbootstrap.com/).
    {
        use std::io::Cursor;
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let woff2_path = std::path::Path::new(&manifest_dir)
            .join("fonts")
            .join("bootstrap-icons.woff2");
        let out_dir = std::env::var("OUT_DIR").unwrap();
        let out_path = std::path::Path::new(&out_dir).join("bootstrap-icons.ttf");
        let bytes = std::fs::read(&woff2_path).unwrap_or_else(|e| {
            panic!("failed to read {}: {e}", woff2_path.display());
        });
        let ttf = woff2_patched::decode::convert_woff2_to_ttf(&mut Cursor::new(bytes))
            .unwrap_or_else(|e| panic!("woff2 decode failed: {e}"));
        std::fs::write(&out_path, ttf).expect("write bootstrap-icons.ttf");
        println!("cargo:rerun-if-changed={}", woff2_path.display());
    }

    #[cfg(windows)]
    {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let icon_path = std::path::Path::new(&manifest_dir).join("appicon.ico");
        let app_manifest = std::path::Path::new(&manifest_dir).join("app.manifest");
        let mut res = winresource::WindowsResource::new();
        res.set_icon(icon_path.to_string_lossy().as_ref());
        res.set_manifest_file(app_manifest.to_string_lossy().as_ref());
        res.compile().expect("failed to compile Windows resources");
        println!("cargo:rerun-if-changed={}", icon_path.display());
        println!("cargo:rerun-if-changed={}", app_manifest.display());
    }
}
