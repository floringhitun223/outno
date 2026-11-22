// Hide the console window on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod builder;
mod installer;
mod payload;
mod platform;
mod ui;

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // 1) Uninstaller mode:  unins000.exe --uninstall "<install dir>"
    if let Some(i) = args.iter().position(|a| a == "--uninstall") {
        let dir = args.get(i + 1).map(PathBuf::from).unwrap_or_default();
        return installer::run_uninstaller(dir);
    }

    // 2) Installer mode: this exe has a payload appended to it.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = payload::read_payload(&exe) {
            return installer::run(p);
        }
    }

    // 3) Otherwise we are the compiler (the "Inno Setup Compiler" part).
    builder::run()
}
