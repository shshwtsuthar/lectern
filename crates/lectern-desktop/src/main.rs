//! Native desktop entry point for Lectern.

mod app;
mod benchmark;
mod workers;

use std::time::Instant;

use eframe::egui;

use crate::app::LecternApp;

fn main() -> eframe::Result {
    let process_started = Instant::now();
    let benchmark = match benchmark::DesktopBenchmark::from_environment(process_started) {
        Ok(benchmark) => benchmark,
        Err(error) => {
            eprintln!("Could not configure desktop benchmark: {error}");
            None
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1_280.0, 820.0])
            .with_min_inner_size([780.0, 560.0]),
        ..eframe::NativeOptions::default()
    };

    eframe::run_native(
        "Lectern",
        options,
        Box::new(move |creation_context| {
            Ok(Box::new(LecternApp::new(creation_context, benchmark)))
        }),
    )
}
