use crate::payload::{self, Manifest, Payload};
use crate::platform;
use crate::taskbar::{self, Taskbar};
use crate::ui::{self, Nav};
use eframe::egui::{self, Color32, RichText};
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
    let opts = ui::window_options(&title, [620.0, 460.0], false);
    eframe::run_native(
        &title,
        opts,
        Box::new(move |cc| {
            let brand = ui::setup(&cc.egui_ctx);
            Ok(Box::new(Wizard::new(payload, brand)))
        }),
    )
}

struct Wizard {
    payload: Payload,
    m: Arc<Manifest>,
    brand: ui::Brand,
    page: Page,
    accepted: bool,
    dir: String,
    desktop: bool,
    start_menu: bool,
    launch: bool,
    total_size: u64,
    confirm_cancel: bool,
    progress: Arc<Mutex<Progress>>,
    taskbar: Taskbar,
    /// Taskbar button already flashed for the end of the install.
    attention_sent: bool,
}

impl Wizard {
    fn new(payload: Payload, brand: ui::Brand) -> Self {
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
            brand,
            page: Page::Welcome,
            accepted: false,
            total_size,
            confirm_cancel: false,
            progress: Arc::new(Mutex::new(Progress::default())),
            taskbar: Taskbar::default(),
            attention_sent: false,
        }
    }

    fn has_license(&self) -> bool {
        !self.m.license_text.trim().is_empty()
    }

    fn dir_error(&self) -> Option<&'static str> {
        let d = self.dir.trim();
        if d.is_empty() {
            Some("Choose a destination folder.")
        } else if !Path::new(d).is_absolute() {
            Some("Enter a full path, for example C:\\Apps\\MyApp.")
        } else if Path::new(d).is_file() {
            Some("A file with this name already exists.")
        } else {
            None
        }
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
                if self.launch && self.m.launch_after {
                    let dir = PathBuf::from(self.dir.trim());
                    let _ = std::process::Command::new(dir.join(&self.m.main_exe)).current_dir(&dir).spawn();
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

    /// Mirror the install progress on the taskbar button and flash it when done.
    fn update_taskbar(&mut self, ctx: &egui::Context, frame: &eframe::Frame, failed: bool) {
        let state = if self.page == Page::Installing {
            let (done, total) = {
                let p = self.progress.lock().unwrap();
                (p.done, p.total)
            };
            let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
            if failed {
                taskbar::State::Error(frac.max(0.05))
            } else if total == 0 {
                taskbar::State::Indeterminate
            } else {
                taskbar::State::Normal(frac)
            }
        } else {
            taskbar::State::None
        };
        self.taskbar.set(frame, state);

        let ended = self.page == Page::Finished || failed;
        if ended && !self.attention_sent && self.payload_was_installing() {
            self.attention_sent = true;
            if !ctx.input(|i| i.focused) {
                let kind = if failed { egui::UserAttentionType::Critical } else { egui::UserAttentionType::Informational };
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(kind));
            }
        }
    }

    /// True once installation has started (Finished is only reachable after it).
    fn payload_was_installing(&self) -> bool {
        self.progress.lock().unwrap().finished
    }

    fn body(ui: &mut egui::Ui, text: impl Into<String>) {
        ui.label(RichText::new(text.into()).size(14.0));
    }

    fn summary_row(ui: &mut egui::Ui, title: &str, lines: &[String]) {
        ui.label(ui::bold(title, 13.5));
        for l in lines {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(ui::muted(l));
            });
        }
    }
}

impl eframe::App for Wizard {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let m = self.m.clone();
        let mut install_error: Option<String> = None;
        // Dialog opened this frame must not react to the same key press.
        let dialog_open = self.confirm_cancel;

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
        self.update_taskbar(ctx, frame, install_error.is_some());

        // Window close button: ask first, and never interrupt a running install.
        if ctx.input(|i| i.viewport().close_requested()) {
            let busy = self.page == Page::Installing && install_error.is_none();
            let done = self.page == Page::Finished || install_error.is_some();
            if busy {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else if !done && !self.confirm_cancel {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm_cancel = true;
            }
        }

        let nav = match self.page {
            Page::Welcome => {
                let nav = ui::footer(ctx, None, "Next >", true, Some(true));
                ui::side_banner(ctx, &self.brand, &m.app_name, &m.version);
                egui::CentralPanel::default()
                    .frame(egui::Frame::none().fill(Color32::WHITE).inner_margin(egui::Margin::symmetric(26.0, 20.0)))
                    .show(ctx, |ui| {
                        ui.add_space(18.0);
                        ui.label(ui::bold(format!("Welcome to the {} Setup Wizard", m.app_name), 20.0));
                        ui.add_space(16.0);
                        Self::body(ui, format!("This will install {} version {} on your computer.", m.app_name, m.version));
                        ui.add_space(6.0);
                        Self::body(ui, "It is recommended that you close all other applications before continuing.");
                        ui.add_space(6.0);
                        Self::body(ui, "Click Next to continue, or Cancel to exit Setup.");
                        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                            if !m.url.is_empty() {
                                ui.hyperlink_to(RichText::new(&m.url).color(ui::ACCENT), &m.url);
                            }
                            if !m.publisher.is_empty() {
                                ui.label(ui::muted(format!("Published by {}", m.publisher)));
                            }
                        });
                    });
                nav
            }

            Page::License => {
                let nav = ui::footer(ctx, Some(true), "Next >", self.accepted, Some(true));
                ui::header(ctx, &self.brand, "License Agreement", "Please read the following important information before continuing.");
                egui::CentralPanel::default()
                    .frame(egui::Frame::none().fill(ui::BG).inner_margin(egui::Margin::symmetric(22.0, 14.0)))
                    .show(ctx, |ui| {
                        Self::body(ui, "You must accept the terms of this agreement before continuing with the installation.");
                        ui.add_space(4.0);
                        ui::inset_frame().show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(176.0).auto_shrink([false, false]).show(ui, |ui| {
                                ui.add(egui::Label::new(RichText::new(&m.license_text).monospace()).wrap());
                            });
                        });
                        ui.add_space(6.0);
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
                let err = self.dir_error();
                let nav = ui::footer(ctx, Some(true), "Next >", err.is_none(), Some(true));
                ui::header(ctx, &self.brand, "Select Destination Location", &format!("Where should {} be installed?", m.app_name));
                egui::CentralPanel::default()
                    .frame(egui::Frame::none().fill(ui::BG).inner_margin(egui::Margin::symmetric(22.0, 18.0)))
                    .show(ctx, |ui| {
                        Self::body(ui, format!("Setup will install {} into the following folder.", m.app_name));
                        ui.add_space(2.0);
                        ui.label(ui::muted("To continue, click Next. To choose a different folder, click Browse."));
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(ui::button("Browse...")).clicked() {
                                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                        self.dir = p.join(&m.install_folder).display().to_string();
                                    }
                                }
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.dir)
                                        .margin(egui::vec2(8.0, 6.0))
                                        .desired_width(ui.available_width()),
                                );
                            });
                        });
                        if let Some(e) = err {
                            ui.label(RichText::new(e).color(ui::ERROR).size(12.5));
                        }
                        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                            ui.label(ui::muted(format!("At least {} of free disk space is required.", ui::fmt_size(self.total_size))));
                        });
                    });
                nav
            }

            Page::Tasks => {
                let nav = ui::footer(ctx, Some(true), "Next >", true, Some(true));
                ui::header(ctx, &self.brand, "Select Additional Tasks", "Which additional tasks should be performed?");
                egui::CentralPanel::default()
                    .frame(egui::Frame::none().fill(ui::BG).inner_margin(egui::Margin::symmetric(22.0, 18.0)))
                    .show(ctx, |ui| {
                        Self::body(ui, format!("Select the additional tasks you would like Setup to perform while installing {}, then click Next.", m.app_name));
                        ui.add_space(12.0);
                        ui::inset_frame().inner_margin(egui::Margin::symmetric(14.0, 12.0)).show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.label(ui::bold("Additional shortcuts", 13.5));
                            ui.add_space(2.0);
                            ui.checkbox(&mut self.desktop, "Create a desktop shortcut");
                            ui.checkbox(&mut self.start_menu, "Create a Start Menu shortcut");
                        });
                    });
                nav
            }

            Page::Ready => {
                let nav = ui::footer(ctx, Some(true), "Install", true, Some(true));
                ui::header(ctx, &self.brand, "Ready to Install", &format!("Setup is now ready to begin installing {} on your computer.", m.app_name));
                egui::CentralPanel::default()
                    .frame(egui::Frame::none().fill(ui::BG).inner_margin(egui::Margin::symmetric(22.0, 16.0)))
                    .show(ctx, |ui| {
                        Self::body(ui, "Click Install to continue, or click Back to review or change any settings.");
                        ui.add_space(8.0);
                        ui::inset_frame().inner_margin(egui::Margin::symmetric(14.0, 12.0)).show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            Self::summary_row(ui, "Destination location", &[self.dir.trim().to_string()]);
                            ui.add_space(6.0);
                            Self::summary_row(ui, "Disk space required", &[ui::fmt_size(self.total_size)]);
                            let mut tasks = Vec::new();
                            if self.desktop {
                                tasks.push("Create a desktop shortcut".to_string());
                            }
                            if self.start_menu {
                                tasks.push("Create a Start Menu shortcut".to_string());
                            }
                            if !tasks.is_empty() {
                                ui.add_space(6.0);
                                Self::summary_row(ui, "Additional tasks", &tasks);
                            }
                        });
                    });
                nav
            }

            Page::Installing => {
                let failed = install_error.is_some();
                let nav = ui::footer(ctx, None, if failed { "Close" } else { "Next >" }, failed, Some(false));
                ui::header(ctx, &self.brand, "Installing", &format!("Please wait while Setup installs {} on your computer.", m.app_name));
                let (done, total, current) = {
                    let p = self.progress.lock().unwrap();
                    (p.done, p.total, p.current.clone())
                };
                egui::CentralPanel::default()
                    .frame(egui::Frame::none().fill(ui::BG).inner_margin(egui::Margin::symmetric(22.0, 22.0)))
                    .show(ctx, |ui| {
                        if let Some(e) = &install_error {
                            ui.label(ui::bold("Setup could not finish", 15.0).color(ui::ERROR));
                            ui.add_space(6.0);
                            ui.label(e);
                            ui.add_space(6.0);
                            ui.label(ui::muted("Click Close to exit Setup. Files copied so far were left in place."));
                        } else {
                            ui.label(ui::bold("Extracting files...", 14.0));
                            ui.add_space(6.0);
                            let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
                            ui.add(egui::ProgressBar::new(frac).desired_width(ui.available_width()).desired_height(20.0).show_percentage());
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.add(egui::Label::new(ui::muted(current).size(12.5)).truncate());
                            });
                            ui.label(ui::muted(format!("{done} of {total} items")).size(12.5));
                        }
                    });
                if failed && nav == Nav::Next {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Nav::None
            }

            Page::Finished => {
                let nav = ui::footer(ctx, None, "Finish", true, None);
                ui::side_banner(ctx, &self.brand, &m.app_name, &m.version);
                egui::CentralPanel::default()
                    .frame(egui::Frame::none().fill(Color32::WHITE).inner_margin(egui::Margin::symmetric(26.0, 20.0)))
                    .show(ctx, |ui| {
                        ui.add_space(18.0);
                        ui.label(ui::bold(format!("Completing the {} Setup Wizard", m.app_name), 20.0));
                        ui.add_space(16.0);
                        Self::body(ui, format!("Setup has finished installing {} on your computer.", m.app_name));
                        ui.add_space(6.0);
                        Self::body(ui, "Click Finish to exit Setup.");
                        if m.launch_after {
                            ui.add_space(18.0);
                            ui.checkbox(&mut self.launch, format!("Launch {}", m.app_name));
                        }
                    });
                nav
            }
        };

        if dialog_open {
            let lines = ["Setup is not complete. If you exit now, the program will not be installed.", "Exit Setup?"];
            if let Some(yes) = ui::confirm_dialog(ctx, "Exit Setup", &lines, "Exit Setup", "Continue") {
                self.confirm_cancel = false;
                if yes {
                    // Mark as finished so the close request isn't intercepted again.
                    self.page = Page::Finished;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        } else {
            match nav {
                Nav::Next => self.go_next(ctx),
                Nav::Back => self.go_back(),
                Nav::Cancel => self.confirm_cancel = true,
                Nav::None => {}
            }
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
    let log: Option<UninstallLog> = fs::read(dir.join(UNINSTALL_LOG)).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let name = log.as_ref().map(|l| l.app_name.clone()).unwrap_or_else(|| "application".into());
    let title = format!("Uninstall - {name}");
    let opts = ui::window_options(&title, [520.0, 330.0], false);
    eframe::run_native(
        &title,
        opts,
        Box::new(move |cc| {
            let brand = ui::setup(&cc.egui_ctx);
            Ok(Box::new(Uninstall { dir, log, brand, done: false }))
        }),
    )
}

struct Uninstall {
    dir: PathBuf,
    log: Option<UninstallLog>,
    brand: ui::Brand,
    done: bool,
}

impl Uninstall {
    fn remove_all(&self, log: &UninstallLog) {
        for f in log.files.iter().chain(&log.shortcuts) {
            let _ = fs::remove_file(f);
        }
        let _ = fs::remove_file(self.dir.join(UNINSTALL_LOG));
        let mut dirs = log.dirs.clone();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.len())); // children first
        for d in dirs {
            let _ = fs::remove_dir(d);
        }
        platform::unregister_uninstall(&log.reg_key);
        if let Ok(exe) = std::env::current_exe() {
            platform::schedule_self_delete(&exe, &self.dir);
        }
    }
}

impl eframe::App for Uninstall {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        let panel = egui::Frame::none().fill(ui::BG).inner_margin(egui::Margin::symmetric(22.0, 22.0));

        let Some(log) = &self.log else {
            let nav = ui::footer(ctx, None, "Close", true, None);
            ui::header(ctx, &self.brand, "Uninstall", "Uninstall information is missing.");
            egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
                ui.label(ui::bold("Can't uninstall", 15.0).color(ui::ERROR));
                ui.add_space(6.0);
                ui.label(format!("The file {UNINSTALL_LOG} was not found in:"));
                ui.label(ui::muted(self.dir.display().to_string()));
                ui.add_space(6.0);
                ui.label("The application may already have been removed.");
            });
            if nav == Nav::Next {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        };
        let name = log.app_name.clone();

        if self.done {
            let nav = ui::footer(ctx, None, "Close", true, None);
            ui::header(ctx, &self.brand, "Uninstall Complete", &format!("{name} was removed."));
            egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
                ui.label(ui::bold(format!("{name} was successfully removed from your computer."), 15.0).color(ui::SUCCESS));
                ui.add_space(6.0);
                ui.label(ui::muted("Files you created after installing may still be in the install folder."));
            });
            if nav == Nav::Next {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        } else {
            let nav = ui::footer(ctx, None, "Uninstall", true, Some(true));
            ui::header(ctx, &self.brand, &format!("Uninstall {name}"), "Remove the application from your computer.");
            egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
                ui.label(format!("Are you sure you want to completely remove {name} and all of its components?"));
                ui.add_space(10.0);
                ui::inset_frame().show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(ui::bold("Install location", 13.5));
                    ui.label(ui::muted(&log.dir));
                    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
                    ui.label(ui::muted(format!("{}, {}", plural(log.files.len(), "file"), plural(log.shortcuts.len(), "shortcut"))).size(12.5));
                });
            });
            match nav {
                Nav::Next => {
                    let log = self.log.take().unwrap_or_default();
                    self.remove_all(&log);
                    self.log = Some(log);
                    self.done = true;
                }
                Nav::Cancel => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                _ => {}
            }
        }
    }
}
