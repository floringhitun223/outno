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
mod lang;
mod payload;
mod platform;
mod rtf;
mod taskbar;
mod ui;
mod win;

use std::path::PathBuf;

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1).cloned())
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let has = |f: &str| args.iter().any(|a| a == f);

    // 1) Uninstaller:  unins000.exe --uninstall "<install dir>"
    //    It first re-runs itself from %TEMP% so the install folder can be removed completely;
    //    `--elevated-job` is the windowless admin helper for all-users installs.
    if has("--uninstall") {
        let dir = arg_after(&args, "--uninstall").map(PathBuf::from).unwrap_or_default();
        if has("--elevated-job") {
            std::process::exit(installer::uninstall_headless(&dir));
        }
        let from_temp = has("--from-temp");
        if !from_temp && installer::relaunch_from_temp(&dir, &args) {
            return Ok(());
        }
        return installer::run_uninstaller(dir, from_temp);
    }

    // 2) Installer: this exe has a payload appended to it.
    //    `--elevated-install <job>` is the windowless admin helper started by the wizard.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = payload::read_payload(&exe) {
            if let Some(job) = arg_after(&args, "--elevated-install") {
                std::process::exit(installer::run_elevated_job(p, &PathBuf::from(job)));
            }
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
