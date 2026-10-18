//! Embeds the app icon and version information into the executable (best effort: if the resource
//! compiler is not available the build still succeeds).

fn main() {
    println!("cargo:rerun-if-changed=assets/tile-rs.ico");
    slint_build::compile("ui/launcher.slint").expect("compile launcher UI");
    if !std::path::Path::new("assets/tile-rs.ico").exists() {
        return;
    }
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/tile-rs.ico");
    res.set("ProductName", "tile-rs");
    res.set(
        "FileDescription",
        "tile-rs - spiral tiling window manager for Windows",
    );
    res.set("LegalCopyright", "MIT License");
    let _ = res.compile();
}
