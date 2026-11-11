#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[path = "gui/app.rs"]
mod app;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 720.0])
            .with_min_inner_size([460.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "MetaSafe",
        options,
        Box::new(|context| Ok(Box::new(app::MetaSafe::new(context)))),
    )
}
