//! Native desktop entry point for Lectern.

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions::default();

    eframe::run_native(
        "Lectern",
        options,
        Box::new(|_creation_context| Ok(Box::<LecternApp>::default())),
    )
}

#[derive(Default)]
struct LecternApp;

impl eframe::App for LecternApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Lectern");
            ui.label("Your ebook library, without the waiting.");
        });
    }
}
