#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> uniwow_api::eframe::Result {
    uniwow_kernel::run()
}
