//! Native desktop entry point for Lectern.

mod app;
mod workers;

use eframe::egui;

use crate::app::LecternApp;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1_280.0, 820.0])
            .with_min_inner_size([780.0, 560.0]),
        ..eframe::NativeOptions::default()
    };

    eframe::run_native(
        "Lectern",
        options,
        Box::new(|creation_context| Ok(Box::new(LecternApp::new(creation_context)))),
    )
}
