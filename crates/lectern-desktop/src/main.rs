//! Native desktop entry point for Lectern.

mod app;
mod benchmark;
mod curation;
mod platform;
mod workers;

use std::{io, time::Instant};

use eframe::egui;

use crate::app::LecternApp;

fn main() -> eframe::Result {
    let main_entry = Instant::now();
    let benchmark = benchmark::DesktopBenchmark::from_environment(main_entry).map_err(|error| {
        eframe::Error::AppCreation(Box::new(io::Error::new(io::ErrorKind::InvalidInput, error)))
    })?;
    let benchmark_active = benchmark.is_some();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1_280.0, 820.0])
            .with_min_inner_size([780.0, 560.0])
            .with_active(benchmark_active)
            .with_window_level(if benchmark_active {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }),
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
