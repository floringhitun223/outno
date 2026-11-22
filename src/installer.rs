use crate::payload::{self, Manifest, Payload};
use crate::platform;
use crate::ui::{self, Nav};
use eframe::egui::{self, Color32, Frame, Margin, RichText, Stroke};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const UNINSTALLER: &str = "unins000.exe";
const UNINSTALL_LOG: &str = "unins000.dat";

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Welcome,
    License,
    Dir,
    Tasks,
    Ready,
    Installing,
    Finished,
}

#[derive(Default)]
struct Progress {
    done: usize,
    total: usize,
    current: String,
    finished: bool,
    error: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct UninstallLog {
    pub app_name: String,
    pub dir: String,
    pub files: Vec<String>,
    pub dirs: Vec<String>,
    pub shortcuts: Vec<String>,
    pub reg_key: String,
}

// ───────────────────────────── Installer wizard ─────────────────────────────

pub fn run(payload: Payload) -> eframe::Result<()> {
    let title = format!("Setup - {}", payload.manifest.app_name);
    let opts = ui::window_options(&title, [600.0, 440.0]);
    eframe::run_native(
        &title,
        opts,
        Box::new(move |cc| {
            ui::apply_classic(&cc.egui_ctx);
            Ok(Box::new(Wizard::new(payload)))
        }),
    )
}

struct Wizard {
    payload: Payload,
    m: Arc<Manifest>,
    page: Page,
    accepted: bool,
    dir: String,
    desktop: bool,
    start_menu: bool,
    launch: bool,
    total_size: u64,
    confirm_cancel: bool,
    progress: Arc<Mutex<Progress>>,
}

impl Wizard {
    fn new(payload: Payload) -> Self {
        let m = Arc::new(payload.manifest.clone());
        let total_size = payload::open_zip(&payload)
            .map(|mut z| (0..z.len()).filter_map(|i| z.by_index(i).ok().map(|e| e.size())).sum())
            .unwrap_or(0);
        let dir = platform::default_install_root().join(&m.install_folder);
        Self {
            desktop: m.desktop_icon,
            start_menu: m.start_menu,
            launch: m.launch_after,
            dir: dir.display().to_string(),
            payload,
            m,
            page: Page::Welcome,
            accepted: false,
            total_size,
            confirm_cancel: false,
            progress: Arc::new(Mutex::new(Progress::default())),
        }
    }

    fn has_license(&self) -> bool {
        !self.m.license_text.trim().is_empty()
    }

    fn go_next(&mut self, ctx: &egui::Context) {
        self.page = match self.page {
            Page::Welcome if self.has_license() => Page::License,
            Page::Welcome | Page::License => Page::Dir,
            Page::Dir => Page::Tasks,
            Page::Tasks => Page::Ready,
            Page::Ready => {
                self.start_install(ctx);
                Page::Installing
            }
            Page::Installing => Page::Installing,
            Page::Finished => {
                if self.launch {
                    let dir = PathBuf::from(&self.dir);
                    let _ = std::process::Command::new(dir.join(&self.m.main_exe))
                        .current_dir(&dir)
                        .spawn();
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                Page::Finished
            }
        };
    }

    fn go_back(&mut self) {
        self.page = match self.page {
            Page::Dir if self.has_license() => Page::License,
            Page::Dir => Page::Welcome,
            Page::License => Page::Welcome,
            Page::Tasks => Page::Dir,
            Page::Ready => Page::Tasks,
            p => p,
        };
    }

    fn start_install(&mut self, ctx: &egui::Context) {
        *self.progress.lock().unwrap() = Progress::default();
        let payload = self.payload.clone();
        let dir = PathBuf::from(self.dir.trim());
        let (desktop, start) = (self.desktop, self.start_menu);
        let prog = self.progress.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let res = do_install(&payload, &dir, desktop, start, &prog, &ctx);
            let mut p = prog.lock().unwrap();
            if let Err(e) = res {
                p.error = Some(e);
            }
            p.finished = true;
            ctx.request_repaint();
        });
    }

    fn handle_nav(&mut self, nav: Nav, ctx: &egui::Context) {
        match nav {
            Nav::Next => self.go_next(ctx),
            Nav::Back => self.go_back(),
            Nav::Cancel => self.confirm_cancel = true,
            Nav::None => {}
        }
    }

    fn body_label(ui: &mut egui::Ui, text: impl Into<String>) {
        ui.label(text.into());
    }
}

impl eframe::App for Wizard {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        let m = self.m.clone();
        let mut install_error: Option<String> = None;

        if self.page == Page::Installing {
            let (fin, err) = {
                let p = self.progress.lock().unwrap();
                (p.finished, p.error.clone())
            };
            if fin && err.is_none() {
                self.page = Page::Finished;
            }
            install_error = err;
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        let nav = match self.page {
            Page::Welcome => {
                let nav = ui::footer(ctx, None, "Next >", true, Some(true));
                ui::side_banner(ctx, &m.app_name);
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add_space(24.0);
                    ui.label(RichText::new(format!("Welcome to the {} Setup Wizard", m.app_name)).strong().size(18.0));
                    ui.add_space(14.0);
                    Self::body_label(ui, format!("This will install {} version {} on your computer.", m.app_name, m.version));
                    ui.add_space(8.0);
                    Self::body_label(ui, "It is recommended that you close all other applications before continuing.");
                    ui.add_space(8.0);
                    Self::body_label(ui, "Click Next to continue, or Cancel to exit Setup.");
                    if !m.publisher.is_empty() {
                        ui.add_space(40.0);
                        ui.label(RichText::new(&m.publisher).weak());
                    }
                });
                nav
            }

            Page::License => {
                let nav = ui::footer(ctx, Some(true), "Next >", self.accepted, Some(true));
                ui::header(ctx, "License Agreement", "Please read the following important information before continuing.");
                egui::CentralPanel::default().show(ctx, |ui| {
                    Self::body_label(ui, "Please read the following License Agreement. You must accept the terms of this agreement before continuing with the installation.");
                    Frame::none()
                        .fill(Color32::WHITE)
                        .stroke(Stroke::new(1.0, Color32::from_rgb(130, 135, 144)))
                        .inner_margin(Margin::same(6.0))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(200.0).auto_shrink([false, false]).show(ui, |ui| {
                                ui.add(egui::Label::new(RichText::new(&m.license_text).monospace().size(12.0)).wrap());
                            });
                        });
                    ui.add_space(4.0);
                    if ui.radio(self.accepted, "I accept the agreement").clicked() {
                        self.accepted = true;
                    }
                    if ui.radio(!self.accepted, "I do not accept the agreement").clicked() {
                        self.accepted = false;
                    }
                });
                nav
            }

            Page::Dir => {
                let ok = !self.dir.trim().is_empty();
                let nav = ui::footer(ctx, Some(true), "Next >", ok, Some(true));
                ui::header(ctx, "Select Destination Location", &format!("Where should {} be installed?", m.app_name));
                egui::CentralPanel::default().show(ctx, |ui| {
                    Self::body_label(ui, format!("Setup will install {} into the following folder.", m.app_name));
                    ui.add_space(4.0);
                    Self::body_label(ui, "To continue, click Next. If you would like to select a different folder, click Browse.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.dir).desired_width(400.0));
                        if ui.button("Browse...").clicked() {
                            if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                self.dir = p.join(&m.install_folder).display().to_string();
                            }
                        }
                    });
                    ui.add_space(30.0);
                    Self::body_label(ui, format!("At least {} of free disk space is required.", ui::fmt_size(self.total_size)));
                });
                nav
            }

            Page::Tasks => {
                let nav = ui::footer(ctx, Some(true), "Next >", true, Some(true));
                ui::header(ctx, "Select Additional Tasks", "Which additional tasks should be performed?");
                egui::CentralPanel::default().show(ctx, |ui| {
                    Self::body_label(ui, format!("Select the additional tasks you would like Setup to perform while installing {}, then click Next.", m.app_name));
                    ui.add_space(10.0);
                    ui.label(RichText::new("Additional shortcuts:").strong());
                    ui.checkbox(&mut self.desktop, "Create a desktop shortcut");
                    ui.checkbox(&mut self.start_menu, "Create a Start Menu shortcut");
                });
                nav
            }

            Page::Ready => {
                let nav = ui::footer(ctx, Some(true), "Install", true, Some(true));
                ui::header(ctx, "Ready to Install", &format!("Setup is now ready to begin installing {} on your computer.", m.app_name));
                egui::CentralPanel::default().show(ctx, |ui| {
                    Self::body_label(ui, "Click Install to continue with the installation, or click Back if you want to review or change any settings.");
                    ui.add_space(8.0);
                    Frame::none()
                        .fill(Color32::WHITE)
                        .stroke(Stroke::new(1.0, Color32::from_rgb(130, 135, 144)))
                        .inner_margin(Margin::same(10.0))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(RichText::new("Destination location:").strong());
                            ui.label(format!("      {}", self.dir));
                            if self.desktop || self.start_menu {
                                ui.add_space(8.0);
                                ui.label(RichText::new("Additional tasks:").strong());
                                if self.desktop { ui.label("      Create a desktop shortcut"); }
                                if self.start_menu { ui.label("      Create a Start Menu shortcut"); }
                            }
                        });
                });
                nav
            }

            Page::Installing => {
                let failed = install_error.is_some();
                let nav = ui::footer(ctx, None, if failed { "Close" } else { "Next >" }, failed, Some(false));
                ui::header(ctx, "Installing", &format!("Please wait while Setup installs {} on your computer.", m.app_name));
                let (done, total, current) = {
                    let p = self.progress.lock().unwrap();
                    (p.done, p.total, p.current.clone())
                };
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add_space(20.0);
                    if let Some(e) = &install_error {
                        ui.colored_label(Color32::from_rgb(180, 0, 0), format!("Setup failed: {e}"));
                    } else {
                        ui.label("Extracting files...");
                        ui.label(RichText::new(current).weak());
                        ui.add_space(6.0);
                        let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
                        ui.add(egui::ProgressBar::new(frac).desired_width(ui.available_width() - 10.0).show_percentage());
                    }
                });
                if failed && nav == Nav::Next {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Nav::None
            }

            Page::Finished => {
                let nav = ui::footer(ctx, None, "Finish", true, None);
                ui::side_banner(ctx, &m.app_name);
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add_space(24.0);
                    ui.label(RichText::new(format!("Completing the {} Setup Wizard", m.app_name)).strong().size(18.0));
                    ui.add_space(14.0);
                    Self::body_label(ui, format!("Setup has finished installing {} on your computer.", m.app_name));
                    ui.add_space(6.0);
                    Self::body_label(ui, "Click Finish to exit Setup.");
                    ui.add_space(20.0);
                    ui.checkbox(&mut self.launch, format!("Launch {}", m.app_name));
                });
                nav
            }
        };

        self.handle_nav(nav, ctx);

        if self.confirm_cancel {
            egui::Window::new("Exit Setup")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("Setup is not complete. If you exit now, the program will not be installed.");
                    ui.label("Exit Setup?");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("  Yes  ").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if ui.button("   No   ").clicked() {
                            self.confirm_cancel = false;
                        }
                    });
                });
        }
    }
}

fn do_install(
    payload: &Payload,
    dir: &Path,
    desktop: bool,
    start_menu: bool,
    prog: &Arc<Mutex<Progress>>,
    ctx: &egui::Context,
) -> Result<(), String> {
    let e = |x: &dyn std::fmt::Display| x.to_string();
    let m = &payload.manifest;
    let mut zip = payload::open_zip(payload).map_err(|x| e(&x))?;
    let total = zip.len();
    prog.lock().unwrap().total = total;

    fs::create_dir_all(dir).map_err(|x| e(&x))?;
    let mut log = UninstallLog {
        app_name: m.app_name.clone(),
        dir: dir.display().to_string(),
        reg_key: platform::registry_key_name(m),
        ..Default::default()
    };

    for i in 0..total {
        let mut entry = zip.by_index(i).map_err(|x| e(&x))?;
        let Some(rel) = entry.enclosed_name() else { continue }; // blocks ../ path tricks
        let target = dir.join(&rel);
        prog.lock().unwrap().current = rel.display().to_string();
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|x| e(&x))?;
            log.dirs.push(target.display().to_string());
        } else {
            if let Some(p) = target.parent() {
                fs::create_dir_all(p).map_err(|x| e(&x))?;
            }
            let mut out = File::create(&target).map_err(|x| format!("{}: {}", target.display(), x))?;
            io::copy(&mut entry, &mut out).map_err(|x| e(&x))?;
            log.files.push(target.display().to_string());
        }
        prog.lock().unwrap().done = i + 1;
        ctx.request_repaint();
    }

    // Uninstaller = the stub part of this exe (everything before the payload).
    let unins = dir.join(UNINSTALLER);
    {
        let mut f = File::open(&payload.exe_path).map_err(|x| e(&x))?;
        let mut stub = vec![0u8; payload.zip_start as usize];
        f.read_exact(&mut stub).map_err(|x| e(&x))?;
        fs::write(&unins, stub).map_err(|x| e(&x))?;
    }
    log.files.push(unins.display().to_string());

    // Shortcuts
    let target = dir.join(&m.main_exe);
    if desktop {
        if let Some(d) = platform::desktop_dir() {
            let lnk = d.join(format!("{}.lnk", m.app_name));
            platform::create_shortcut(&target, &lnk, dir)?;
            log.shortcuts.push(lnk.display().to_string());
        }
    }
    if start_menu {
        if let Some(d) = platform::start_menu_dir(&m.app_name) {
            let lnk = d.join(format!("{}.lnk", m.app_name));
            platform::create_shortcut(&target, &lnk, dir)?;
            log.shortcuts.push(lnk.display().to_string());
            log.dirs.push(d.display().to_string());
        }
    }

    log.dirs.push(dir.display().to_string());
    fs::write(
        dir.join(UNINSTALL_LOG),
        serde_json::to_vec_pretty(&log).map_err(|x| e(&x))?,
    )
    .map_err(|x| e(&x))?;
    platform::register_uninstall(m, dir, &unins)?;
    Ok(())
}

// ───────────────────────────── Uninstaller ─────────────────────────────

pub fn run_uninstaller(dir: PathBuf) -> eframe::Result<()> {
    let log: UninstallLog = fs::read(dir.join(UNINSTALL_LOG))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let title = format!("Uninstall - {}", log.app_name);
    let opts = ui::window_options(&title, [480.0, 300.0]);
    eframe::run_native(
        &title,
        opts,
        Box::new(move |cc| {
            ui::apply_classic(&cc.egui_ctx);
            Ok(Box::new(Uninstall { dir, log, done: false }))
        }),
    )
}

struct Uninstall {
    dir: PathBuf,
    log: UninstallLog,
    done: bool,
}

impl Uninstall {
    fn remove_all(&self) {
        for f in self.log.files.iter().chain(&self.log.shortcuts) {
            let _ = fs::remove_file(f);
        }
        let _ = fs::remove_file(self.dir.join(UNINSTALL_LOG));
        let mut dirs = self.log.dirs.clone();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.len())); // children first
        for d in dirs {
            let _ = fs::remove_dir(d);
        }
        platform::unregister_uninstall(&self.log.reg_key);
        if let Ok(exe) = std::env::current_exe() {
            platform::schedule_self_delete(&exe, &self.dir);
        }
    }
}

impl eframe::App for Uninstall {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        let name = self.log.app_name.clone();
        if self.done {
            let nav = ui::footer(ctx, None, "Close", true, None);
            ui::header(ctx, "Uninstall Complete", &format!("{name} was successfully removed."));
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add_space(20.0);
                ui.label(format!("{name} was successfully removed from your computer."));
            });
            if nav == Nav::Next {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        } else {
            let nav = ui::footer(ctx, None, "Uninstall", true, Some(true));
            ui::header(ctx, "Uninstall", &format!("Remove {name} from your computer."));
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.add_space(20.0);
                ui.label(format!("Are you sure you want to completely remove {name} and all of its components?"));
            });
            match nav {
                Nav::Next => {
                    self.remove_all();
                    self.done = true;
                }
                Nav::Cancel => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                _ => {}
            }
        }
    }
}
