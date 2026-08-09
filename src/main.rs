#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod catalog;
mod console;
mod dfu;
mod esp;
mod net;
mod ports;
mod worker;

/// Window icon as straight RGBA pixels, 64x64. Raw pixels keep the binary
/// free of an image decoder.
const ICON_RGBA: &[u8] = include_bytes!("../assets/icon-64.rgba");
const ICON_SIZE: u32 = 64;

fn window_icon() -> eframe::egui::IconData {
    debug_assert_eq!(ICON_RGBA.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
    eframe::egui::IconData {
        rgba: ICON_RGBA.to_vec(),
        width: ICON_SIZE,
        height: ICON_SIZE,
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([900.0, 640.0])
            .with_min_inner_size([700.0, 480.0])
            .with_title("MeshFlash")
            .with_icon(window_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "MeshFlash",
        options,
        Box::new(|cc| Ok(Box::new(app::MeshFlashApp::new(cc)))),
    )
}
