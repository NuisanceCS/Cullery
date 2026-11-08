// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod devices;
mod dragout;
mod export;
mod library;
mod loader;
mod picks;
mod raw;
mod settings;
mod theme;
mod window;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Cullery")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([720.0, 480.0])
            .with_drag_and_drop(true)
            // See-through where nothing is drawn, so the Windows 11 Mica backdrop can
            // show in the top bar. Every other theme paints the whole window.
            .with_transparent(true),
        ..Default::default()
    };
    eframe::run_native("Cullery", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
