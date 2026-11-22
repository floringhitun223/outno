// Hide the console window on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

//! OUTNO — lightweight setup creator.
//!
//! One executable, three roles:
//!   * compiler     — no payload attached: the OUTNO window (or `--build` from the command line)
//!   * installer    — a payload is appended: the setup wizard
//!   * uninstaller  — started as `unins000.exe --uninstall "<dir>"`

mod builder;
mod installer;
mod payload;
mod platform;
mod taskbar;
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

    // 3) Headless build:  outno --build project.outno
    if let Some(i) = args.iter().position(|a| a == "--build") {
        platform::attach_parent_console();
        let Some(project) = args.get(i + 1) else {
            eprintln!("usage: outno --build <project.{}>", builder::PROJECT_EXT);
            std::process::exit(2);
        };
        match builder::build_cli(&PathBuf::from(project)) {
            Ok(p) => {
                println!("Setup written to {}", p.display());
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("Build failed:\n{e}");
                std::process::exit(1);
            }
        }
    }

    // 4) Compiler window, optionally opening a project passed as the first argument.
    let project = args
        .iter()
        .skip(1)
        .find(|a| a.to_lowercase().ends_with(&format!(".{}", builder::PROJECT_EXT)))
        .map(PathBuf::from);
    builder::run(project)
}
