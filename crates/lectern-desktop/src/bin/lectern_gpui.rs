//! Native GPUI entry point for Lectern.

use std::time::Instant;

fn main() {
    lectern_desktop::gpui_app::run(Instant::now());
}
