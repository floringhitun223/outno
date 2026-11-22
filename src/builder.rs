//! The compiler: pick an app folder + options, produce a self-contained setup.exe.

use crate::payload::{self, Manifest, MAGIC};
use crate::taskbar::{self, Taskbar};
use crate::{platform, ui};
use eframe::egui::{self, Color32, Key, Modifiers, RichText, Stroke};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use walkdir::WalkDir;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

pub const PROJECT_EXT: &str = "outno";
const INVALID_NAME_CHARS: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

// ───────────────────────────── Project / form ─────────────────────────────

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Form {
    pub app_name: String,
    pub version: String,
    pub publisher: String,
    pub url: String,
    pub source_dir: String,
    pub main_exe: String,
    pub license_file: String,
    pub install_folder: String,
    pub out_dir: String,
    pub out_name: String,
    pub desktop: bool,
    pub start_menu: bool,
    pub launch: bool,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            app_name: String::new(),
            version: "1.0.0".into(),
            publisher: String::new(),
            url: String::new(),
            source_dir: String::new(),
            main_exe: String::new(),
            license_file: String::new(),
            install_folder: String::new(),
            out_dir: std::env::current_dir().unwrap_or_default().join("Output").display().to_string(),
            out_name: String::new(),
            desktop: true,
            start_menu: true,
            launch: true,
        }
    }
}

impl Form {
    fn default_out_name(&self) -> String {
        let app = self.app_name.trim();
        let app = if app.is_empty() { "App" } else { app };
        let ver = self.version.trim();
        let ver = if ver.is_empty() { "1.0.0" } else { ver };
        format!("{}-setup-{}.exe", app.replace(' ', "_"), ver)
    }

    fn out_file_name(&self) -> String {
        let n = self.out_name.trim();
        if n.is_empty() {
            self.default_out_name()
        } else if n.to_lowercase().ends_with(".exe") {
            n.to_string()
        } else {
            format!("{n}.exe")
        }
    }

    /// Load a `.outno` project. Relative paths are resolved against the project's folder.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|e| format!("Can't read {}: {e}", path.display()))?;
        let mut f: Form = serde_json::from_slice(&bytes).map_err(|e| format!("Invalid project file: {e}"))?;
        let base = path.parent().unwrap_or(Path::new("."));
        for p in [&mut f.source_dir, &mut f.license_file, &mut f.out_dir] {
            if !p.trim().is_empty() && Path::new(p.trim()).is_relative() {
                *p = base.join(p.trim()).display().to_string();
            }
        }
        Ok(f)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, json).map_err(|e| format!("Can't save {}: {e}", path.display()))
    }
}

/// Per-field validation messages (None = field is fine).
#[derive(Default)]
struct Errors {
    app_name: Option<String>,
    version: Option<String>,
    source_dir: Option<String>,
    main_exe: Option<String>,
    license_file: Option<String>,
    install_folder: Option<String>,
    out_dir: Option<String>,
    out_name: Option<String>,
}

impl Errors {
    fn all(&self) -> Vec<String> {
        [
            ("Application name", &self.app_name),
            ("Version", &self.version),
            ("Application folder", &self.source_dir),
            ("Main executable", &self.main_exe),
            ("License file", &self.license_file),
            ("Install folder", &self.install_folder),
            ("Output folder", &self.out_dir),
            ("Setup file name", &self.out_name),
        ]
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}: {v}")))
        .collect()
    }
}

fn bad_name(s: &str) -> Option<String> {
    if s.contains(INVALID_NAME_CHARS) {
        Some("Can't contain \\ / : * ? \" < > |".into())
    } else if s.trim() == "." || s.trim() == ".." {
        Some("Not a valid name".into())
    } else {
        None
    }
}

fn same_or_inside(child: &Path, parent: &Path) -> bool {
    let c = fs::canonicalize(child).unwrap_or_else(|_| child.to_path_buf());
    let p = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    c.starts_with(p)
}

fn validate(f: &Form) -> Errors {
    let mut e = Errors::default();
    let req = |s: &str| s.trim().is_empty().then(|| "Required".to_string());

    e.app_name = req(&f.app_name).or_else(|| bad_name(&f.app_name));
    e.version = req(&f.version);

    let src = PathBuf::from(f.source_dir.trim());
    e.source_dir = if f.source_dir.trim().is_empty() {
        Some("Choose the folder that contains your app".into())
    } else if !src.is_dir() {
        Some("Folder not found".into())
    } else {
        None
    };

    let exe = f.main_exe.trim();
    e.main_exe = if exe.is_empty() {
        Some("Choose the program to launch".into())
    } else if !exe.to_lowercase().ends_with(".exe") {
        Some("Must be an .exe file".into())
    } else if Path::new(exe).is_absolute() || exe.split(['/', '\\']).any(|c| c == "..") {
        Some("Must be a path inside the application folder".into())
    } else if e.source_dir.is_none() && !src.join(exe).is_file() {
        Some("Not found inside the application folder".into())
    } else {
        None
    };

    let lic = f.license_file.trim();
    if !lic.is_empty() && !Path::new(lic).is_file() {
        e.license_file = Some("File not found".into());
    }

    e.install_folder = bad_name(&f.install_folder);

    let out = PathBuf::from(f.out_dir.trim());
    e.out_dir = if f.out_dir.trim().is_empty() {
        Some("Required".into())
    } else if e.source_dir.is_none() && same_or_inside(&src, &out) {
        Some("Can't be the application folder or one of its parents".into())
    } else {
        None
    };

    e.out_name = bad_name(&f.out_name);
    e
}

// ───────────────────────────── Folder scan ─────────────────────────────

#[derive(Default, Clone)]
struct Scan {
    dir: String,
    busy: bool,
    done: bool,
    files: usize,
    bytes: u64,
    exes: Vec<String>,
}

fn scan_dir(dir: &Path) -> (usize, u64, Vec<String>) {
    let (mut files, mut bytes, mut exes) = (0usize, 0u64, Vec::new());
    for e in WalkDir::new(dir).into_iter().flatten() {
        if !e.file_type().is_file() {
            continue;
        }
        files += 1;
        bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
        let is_exe = e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("exe"));
        if is_exe && e.depth() <= 3 {
            if let Ok(rel) = e.path().strip_prefix(dir) {
                exes.push(rel.display().to_string());
            }
        }
    }
    exes.sort_by_key(|p| (p.matches(['/', '\\']).count(), p.to_lowercase()));
    (files, bytes, exes)
}

/// Best guess for the main exe: named after the folder, else top-level, skipping helpers.
fn pick_main_exe(dir: &str, exes: &[String]) -> Option<String> {
    let folder = Path::new(dir).file_name()?.to_string_lossy().to_lowercase();
    let helper = |p: &str| {
        let n = p.to_lowercase();
        let file = n.rsplit(['/', '\\']).next().unwrap_or(&n).to_string();
        file.starts_with("unins") || file.contains("crash") || file.contains("redist") || file.contains("setup") || file.contains("update")
    };
    let candidates: Vec<&String> = exes.iter().filter(|e| !helper(e)).collect();
    let stem = |p: &str| Path::new(p).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    candidates
        .iter()
        .find(|e| stem(e) == folder || stem(e).replace([' ', '-', '_'], "") == folder.replace([' ', '-', '_'], ""))
        .or_else(|| candidates.first())
        .map(|s| s.to_string())
        .or_else(|| exes.first().cloned())
}

// ───────────────────────────── Build state ─────────────────────────────

#[derive(Default)]
struct BuildState {
    running: bool,
    log: Vec<String>,
    done: usize,
    total: usize,
    result: Option<Result<(PathBuf, u64), String>>,
}

enum Msg {
    Log(String),
    Total(usize),
    Progress(usize),
}

// ───────────────────────────── App ─────────────────────────────

pub fn run(project: Option<PathBuf>) -> eframe::Result<()> {
    let opts = ui::window_options("OUTNO", [860.0, 760.0], true);
    eframe::run_native(
        "OUTNO",
        opts,
        Box::new(move |cc| {
            let brand = ui::setup(&cc.egui_ctx);
            let mut app = Builder {
                f: Form::default(),
                saved: Form::default(),
                brand,
                state: Arc::default(),
                scan: Arc::default(),
                project: None,
                tried: false,
                notice: None,
                title: String::new(),
                confirm_new: false,
                taskbar: Taskbar::default(),
                was_running: false,
            };
            if let Some(p) = project {
                app.open_project(&p);
            }
            Ok(Box::new(app))
        }),
    )
}

struct Builder {
    f: Form,
    /// Last saved/loaded state, to detect unsaved changes.
    saved: Form,
    brand: ui::Brand,
    state: Arc<Mutex<BuildState>>,
    scan: Arc<Mutex<Scan>>,
    project: Option<PathBuf>,
    /// Show "required" errors only after the first build attempt.
    tried: bool,
    notice: Option<(String, bool)>,
    title: String,
    confirm_new: bool,
    taskbar: Taskbar,
    was_running: bool,
}

/// Label column + full-width input + optional trailing widgets, with an inline error line.
fn field(
    ui: &mut egui::Ui,
    label: &str,
    hint: &str,
    value: &mut String,
    err: Option<&String>,
    trailing: impl FnOnce(&mut egui::Ui),
) {
    ui.label(label);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        trailing(ui);
        ui.scope(|ui| {
            if err.is_some() {
                let s = Stroke::new(1.0_f32, ui::ERROR);
                ui.visuals_mut().widgets.inactive.bg_stroke = s;
                ui.visuals_mut().widgets.hovered.bg_stroke = s;
            }
            ui.add(
                egui::TextEdit::singleline(value)
                    .hint_text(RichText::new(hint).color(Color32::from_rgb(160, 165, 174)))
                    .margin(egui::vec2(8.0, 5.0))
                    .desired_width(ui.available_width()),
            );
        });
    });
    ui.end_row();
    if let Some(e) = err {
        ui.label("");
        ui.label(RichText::new(e).color(ui::ERROR).size(12.0));
        ui.end_row();
    }
}

fn grid(id: &str) -> egui::Grid {
    egui::Grid::new(id).num_columns(2).min_col_width(150.0).spacing([12.0, 8.0])
}

impl eframe::App for Builder {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let (running, log, result, done, total) = {
            let s = self.state.lock().unwrap();
            (s.running, s.log.clone(), s.result.clone(), s.done, s.total)
        };
        if running {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }

        // Build progress on the taskbar button; flash it when a background build ends.
        let focused = ctx.input(|i| i.focused);
        let failed = matches!(result, Some(Err(_)));
        let tb = if running {
            if total == 0 { taskbar::State::Indeterminate } else { taskbar::State::Normal(done as f32 / total as f32) }
        } else if failed && !focused {
            taskbar::State::Error(1.0)
        } else {
            taskbar::State::None
        };
        self.taskbar.set(frame, tb);
        if self.was_running && !running && !focused {
            let kind = if failed { egui::UserAttentionType::Critical } else { egui::UserAttentionType::Informational };
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(kind));
        }
        self.was_running = running;

        self.poll_scan(ctx);
        self.handle_drops(ctx);
        self.handle_shortcuts(ctx, running);
        self.update_title(ctx);

        let shown = validate(&self.f);
        let errors = shown.all();
        // Hide "Required"-style messages until the user tries to build.
        let tried = self.tried;
        let show = move |e: &Option<String>, v: &str| -> Option<String> {
            if tried || !v.trim().is_empty() { e.clone() } else { None }
        };

        // ── Top brand bar ──
        egui::TopBottomPanel::top("brand")
            .exact_height(68.0)
            .frame(
                egui::Frame::none()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(20.0, 10.0))
                    .stroke(Stroke::new(1.0_f32, ui::BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add(self.brand.image(44.0));
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.add_space(4.0);
                        ui.label(ui::bold(ui::BRAND, 20.0).color(ui::ACCENT));
                        ui.label(ui::muted(ui::TAGLINE).size(12.5));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(ui::button("Save")).on_hover_text("Save project (Ctrl+S)").clicked() {
                            self.save_project(false);
                        }
                        if ui.add(ui::button("Open...")).on_hover_text("Open a .outno project (Ctrl+O)").clicked() {
                            self.open_project_dialog();
                        }
                        if ui.add(ui::button("New")).on_hover_text("Start a new project (Ctrl+N)").clicked() {
                            self.new_project();
                        }
                        if let Some(p) = &self.project {
                            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                            let dirty = if self.f != self.saved { " •" } else { "" };
                            ui.label(ui::muted(format!("{name}{dirty}"))).on_hover_text(p.display().to_string());
                        }
                    });
                });
            });

        // ── Bottom action bar ──
        egui::TopBottomPanel::bottom("actions")
            .frame(
                egui::Frame::none()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(20.0, 12.0))
                    .stroke(Stroke::new(1.0_f32, ui::BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can_build = !running && errors.is_empty();
                        let label = if running { "Building..." } else { "Build setup" };
                        let mut resp = ui.add_enabled(!running, ui::primary_button(label).min_size(egui::vec2(140.0, 36.0)));
                        resp = resp.on_hover_text("Build the setup executable (F9 / Ctrl+B)");
                        if resp.clicked() {
                            self.tried = true;
                            if can_build {
                                self.start_build();
                            }
                        }
                        if let Some(Ok((p, _))) = &result {
                            if !running && ui.add(ui::button("Show in folder")).clicked() {
                                platform::reveal_file(p);
                            }
                        }
                        ui.add_space(8.0);
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            if running {
                                let frac = if total == 0 { 0.0 } else { done as f32 / total as f32 };
                                ui.add(
                                    egui::ProgressBar::new(frac)
                                        .desired_width(ui.available_width())
                                        .text(format!("Packing {done} / {total} files")),
                                );
                            } else if self.tried && !errors.is_empty() {
                                let n = errors.len();
                                ui.label(
                                    RichText::new(format!("Fix {n} field{} to build", if n == 1 { "" } else { "s" }))
                                        .color(ui::ERROR),
                                )
                                .on_hover_text(errors.join("\n"));
                            } else if let Some(r) = &result {
                                match r {
                                    Ok((p, size)) => {
                                        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                                        ui.label(RichText::new(format!("Built {name}  ({})", ui::fmt_size(*size))).color(ui::SUCCESS))
                                            .on_hover_text(p.display().to_string());
                                    }
                                    Err(e) => {
                                        ui.label(RichText::new(format!("Build failed: {e}")).color(ui::ERROR));
                                    }
                                }
                            } else if let Some((msg, ok)) = &self.notice {
                                ui.label(RichText::new(msg).color(if *ok { ui::MUTED } else { ui::ERROR }));
                            } else {
                                ui.label(ui::muted(format!("Output: {}", self.f.out_file_name())));
                            }
                        });
                    });
                });
            });

        // ── Form ──
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(ui::BG).inner_margin(egui::Margin::symmetric(20.0, 16.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_enabled_ui(!running, |ui| self.form_ui(ui, &shown, &show));
                    ui.add_space(12.0);
                    ui::card(ui, "Build log", "", |ui| {
                        ui::inset_frame().fill(Color32::from_rgb(250, 250, 251)).show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            egui::ScrollArea::vertical()
                                .id_salt("log")
                                .max_height(130.0)
                                .min_scrolled_height(130.0)
                                .stick_to_bottom(true)
                                .auto_shrink([false, true])
                                .show(ui, |ui| {
                                    if log.is_empty() {
                                        ui.label(ui::muted("Compiler output appears here."));
                                    }
                                    for l in &log {
                                        ui.label(RichText::new(l).monospace().color(ui::MUTED));
                                    }
                                });
                        });
                    });
                });
            });

        // Drag & drop overlay.
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            egui::Area::new(egui::Id::new("drop-overlay"))
                .order(egui::Order::Foreground)
                .fixed_pos(egui::Pos2::ZERO)
                .show(ctx, |ui| {
                    let r = ctx.screen_rect();
                    ui.painter().rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(10, 100, 214, 40));
                    ui.painter().rect_stroke(r.shrink(10.0), 12.0, Stroke::new(2.0_f32, ui::ACCENT));
                    ui.painter().text(
                        r.center(),
                        egui::Align2::CENTER_CENTER,
                        "Drop an app folder, a .outno project or a license file",
                        egui::FontId::proportional(18.0),
                        ui::ACCENT_PRESS,
                    );
                });
        }

        if self.confirm_new {
            let lines = ["Your changes to the current project are not saved."];
            if let Some(yes) = ui::confirm_dialog(ctx, "Discard changes?", &lines, "Discard", "Keep editing") {
                self.confirm_new = false;
                if yes {
                    self.reset();
                }
            }
        }
    }
}

impl Builder {
    fn form_ui(&mut self, ui: &mut egui::Ui, e: &Errors, show: &dyn Fn(&Option<String>, &str) -> Option<String>) {
        let f = &mut self.f;

        ui::card(ui, "Application", "Shown in the setup wizard and in Installed apps.", |ui| {
            grid("app").show(ui, |ui| {
                let err = show(&e.app_name, &f.app_name);
                field(ui, "Application name", "My App", &mut f.app_name, err.as_ref(), |_| {});
                let err = show(&e.version, &f.version);
                field(ui, "Version", "1.0.0", &mut f.version, err.as_ref(), |_| {});
                field(ui, "Publisher", "Your name or company (optional)", &mut f.publisher, None, |_| {});
                field(ui, "Website", "https://example.com (optional)", &mut f.url, None, |_| {});
            });
        });
        ui.add_space(12.0);

        let mut picked_src: Option<String> = None;
        let (mut clear_lic, mut set_lic, mut set_out) = (false, None::<String>, None::<String>);
        let scan = self.scan.lock().unwrap().clone();
        ui::card(ui, "Files", "Everything inside the application folder is packed into the setup.", |ui| {
            grid("files").show(ui, |ui| {
                let err = show(&e.source_dir, &f.source_dir);
                field(ui, "Application folder", "Browse or drop a folder here", &mut f.source_dir, err.as_ref(), |ui| {
                    if ui.button("Browse...").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            picked_src = Some(p.display().to_string());
                        }
                    }
                });

                if !f.source_dir.trim().is_empty() && e.source_dir.is_none() {
                    ui.label("");
                    let txt = if scan.busy || scan.dir != f.source_dir.trim() {
                        "Scanning folder...".to_string()
                    } else {
                        format!(
                            "{} files · {} · {} executable{}",
                            scan.files,
                            ui::fmt_size(scan.bytes),
                            scan.exes.len(),
                            if scan.exes.len() == 1 { "" } else { "s" }
                        )
                    };
                    ui.label(ui::muted(txt).size(12.5));
                    ui.end_row();
                }

                let src = PathBuf::from(f.source_dir.trim());
                let err = show(&e.main_exe, &f.main_exe);
                let exes = scan.exes.clone();
                let mut chosen: Option<String> = None;
                field(ui, "Main executable", "app.exe — used for shortcuts and Launch", &mut f.main_exe, err.as_ref(), |ui| {
                    if ui.button("Browse...").clicked() {
                        let mut dlg = rfd::FileDialog::new().add_filter("Program", &["exe"]);
                        if src.is_dir() {
                            dlg = dlg.set_directory(&src);
                        }
                        if let Some(p) = dlg.pick_file() {
                            chosen = Some(match p.strip_prefix(&src) {
                                Ok(r) => r.display().to_string(),
                                Err(_) => p.display().to_string(), // flagged by validation
                            });
                        }
                    }
                    if !exes.is_empty() {
                        ui.menu_button("Detected...", |ui| {
                            ui.set_min_width(220.0);
                            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                                for x in &exes {
                                    if ui.button(x).clicked() {
                                        chosen = Some(x.clone());
                                        ui.close_menu();
                                    }
                                }
                            });
                        });
                    }
                });
                if let Some(c) = chosen {
                    f.main_exe = c;
                }

                let err = show(&e.license_file, &f.license_file);
                let has_lic = !f.license_file.is_empty();
                field(ui, "License file", "Optional .txt shown before installing", &mut f.license_file, err.as_ref(), |ui| {
                    if has_lic && ui.button("Clear").on_hover_text("Remove the license file").clicked() {
                        clear_lic = true;
                    }
                    if ui.button("Browse...").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("Text", &["txt", "md"]).pick_file() {
                            set_lic = Some(p.display().to_string());
                        }
                    }
                });
            });
            ui.add_space(2.0);
            ui.label(ui::muted("Tip: drag a folder onto this window to use it as the application folder.").size(12.0));
        });
        if clear_lic {
            f.license_file.clear();
        }
        if let Some(s) = set_lic {
            f.license_file = s;
        }
        ui.add_space(12.0);

        ui::card(ui, "Installer", "How the setup wizard behaves on the user's PC.", |ui| {
            grid("opts").show(ui, |ui| {
                let hint = if f.app_name.trim().is_empty() { "Defaults to the application name".to_string() } else { f.app_name.trim().to_string() };
                let err = show(&e.install_folder, &f.install_folder);
                field(ui, "Install folder name", &hint, &mut f.install_folder, err.as_ref(), |_| {});
                ui.label("");
                ui.label(ui::muted("Installed per user to %LOCALAPPDATA%\\Programs\\<folder>, no admin rights needed.").size(12.0));
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.checkbox(&mut f.desktop, "Desktop shortcut — ticked by default");
            ui.checkbox(&mut f.start_menu, "Start Menu shortcut — ticked by default");
            ui.checkbox(&mut f.launch, "Offer to launch the app when setup finishes");
        });
        ui.add_space(12.0);

        ui::card(ui, "Output", "Where the finished setup executable is written.", |ui| {
            grid("out").show(ui, |ui| {
                let err = show(&e.out_dir, &f.out_dir);
                field(ui, "Output folder", "Folder for the setup .exe", &mut f.out_dir, err.as_ref(), |ui| {
                    if ui.button("Browse...").clicked() {
                        if let Some(p) = rfd::FileDialog::new().pick_folder() {
                            set_out = Some(p.display().to_string());
                        }
                    }
                });
                let hint = f.default_out_name();
                let err = show(&e.out_name, &f.out_name);
                field(ui, "Setup file name", &hint, &mut f.out_name, err.as_ref(), |_| {});
            });
        });
        if let Some(s) = set_out {
            f.out_dir = s;
        }

        if let Some(src) = picked_src {
            self.set_source(src);
        }
    }

    // ── Source folder handling ──

    fn set_source(&mut self, src: String) {
        let name = Path::new(&src).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if self.f.app_name.trim().is_empty() {
            self.f.app_name = name;
        }
        if self.f.source_dir != src {
            self.f.main_exe.clear(); // re-detected when the scan finishes
        }
        self.f.source_dir = src;
    }

    /// Start a background scan when the folder changes; apply exe auto-detection when done.
    fn poll_scan(&mut self, ctx: &egui::Context) {
        let dir = self.f.source_dir.trim().to_string();
        let mut s = self.scan.lock().unwrap();
        if s.dir != dir {
            *s = Scan { dir: dir.clone(), ..Default::default() };
            if Path::new(&dir).is_dir() {
                s.busy = true;
                let scan = self.scan.clone();
                let ctx = ctx.clone();
                thread::spawn(move || {
                    let (files, bytes, exes) = scan_dir(Path::new(&dir));
                    let mut s = scan.lock().unwrap();
                    if s.dir == dir {
                        s.files = files;
                        s.bytes = bytes;
                        s.exes = exes;
                        s.busy = false;
                    }
                    ctx.request_repaint();
                });
            }
        } else if !s.busy && !s.done && !dir.is_empty() {
            s.done = true;
            if self.f.main_exe.trim().is_empty() {
                if let Some(x) = pick_main_exe(&dir, &s.exes) {
                    self.f.main_exe = x;
                }
            }
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        for p in dropped {
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if p.is_dir() {
                self.set_source(p.display().to_string());
                self.notice = Some((format!("Application folder set to {}", p.display()), true));
            } else if ext == PROJECT_EXT {
                self.open_project(&p);
            } else if ext == "txt" || ext == "md" {
                self.f.license_file = p.display().to_string();
                self.notice = Some(("License file set.".into(), true));
            } else if ext == "exe" {
                let src = PathBuf::from(self.f.source_dir.trim());
                match p.strip_prefix(&src) {
                    Ok(rel) if src.is_dir() => self.f.main_exe = rel.display().to_string(),
                    _ => self.notice = Some(("Drop the app folder first; the .exe must be inside it.".into(), false)),
                }
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context, running: bool) {
        let (save, save_as, open, new, build) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND, Key::S),
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::S),
                i.consume_key(Modifiers::COMMAND, Key::O),
                i.consume_key(Modifiers::COMMAND, Key::N),
                i.consume_key(Modifiers::NONE, Key::F9) || i.consume_key(Modifiers::COMMAND, Key::B),
            )
        });
        if save_as {
            self.save_project(true);
        } else if save {
            self.save_project(false);
        }
        if open {
            self.open_project_dialog();
        }
        if new {
            self.new_project();
        }
        if build && !running {
            self.tried = true;
            if validate(&self.f).all().is_empty() {
                self.start_build();
            }
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let mut t = String::from("OUTNO");
        if let Some(p) = &self.project {
            t = format!("{} — OUTNO", p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
            if self.f != self.saved {
                t.insert(0, '*');
            }
        }
        if t != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(t.clone()));
            self.title = t;
        }
    }

    // ── Projects ──

    fn reset(&mut self) {
        self.f = Form::default();
        self.saved = Form::default();
        self.project = None;
        self.tried = false;
        self.notice = None;
        self.state.lock().unwrap().result = None;
    }

    fn new_project(&mut self) {
        if self.f != self.saved {
            self.confirm_new = true;
        } else {
            self.reset();
        }
    }

    fn open_project_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("OUTNO project", &[PROJECT_EXT]).pick_file() {
            self.open_project(&p);
        }
    }

    fn open_project(&mut self, p: &Path) {
        match Form::load(p) {
            Ok(f) => {
                self.f = f.clone();
                self.saved = f;
                self.project = Some(p.to_path_buf());
                self.tried = false;
                self.notice = Some((format!("Opened {}", p.display()), true));
                self.state.lock().unwrap().result = None;
            }
            Err(e) => self.notice = Some((e, false)),
        }
    }

    fn save_project(&mut self, save_as: bool) {
        let path = match (&self.project, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => {
                let name = if self.f.app_name.trim().is_empty() { "setup".to_string() } else { self.f.app_name.trim().to_string() };
                rfd::FileDialog::new()
                    .add_filter("OUTNO project", &[PROJECT_EXT])
                    .set_file_name(format!("{name}.{PROJECT_EXT}"))
                    .save_file()
            }
        };
        let Some(mut path) = path else { return };
        if path.extension().is_none() {
            path.set_extension(PROJECT_EXT);
        }
        match self.f.save(&path) {
            Ok(()) => {
                self.saved = self.f.clone();
                self.project = Some(path.clone());
                self.notice = Some((format!("Saved {}", path.display()), true));
                self.state.lock().unwrap().result = None;
            }
            Err(e) => self.notice = Some((e, false)),
        }
    }

    // ── Build ──

    fn start_build(&mut self) {
        let form = self.f.clone();
        let state = self.state.clone();
        self.notice = None;
        {
            let mut s = state.lock().unwrap();
            *s = BuildState { running: true, ..Default::default() };
        }
        thread::spawn(move || {
            let st = state.clone();
            let mut report = move |m: Msg| {
                let mut s = st.lock().unwrap();
                match m {
                    Msg::Log(l) => s.log.push(l),
                    Msg::Total(t) => s.total = t,
                    Msg::Progress(d) => s.done = d,
                }
            };
            let res = build_installer(&form, &mut report).map_err(|e| e.to_string());
            let mut s = state.lock().unwrap();
            if let Err(e) = &res {
                s.log.push(format!("Error: {e}"));
            }
            s.running = false;
            s.result = Some(res);
        });
    }
}

/// Headless build for scripts / CI:  `outno --build project.outno`
pub fn build_cli(project: &Path) -> Result<PathBuf, String> {
    let f = Form::load(project)?;
    let errs = validate(&f).all();
    if !errs.is_empty() {
        return Err(errs.join("\n"));
    }
    let mut report = |m: Msg| {
        if let Msg::Log(l) = m {
            println!("{l}");
        }
    };
    build_installer(&f, &mut report).map(|(p, _)| p).map_err(|e| e.to_string())
}

fn build_installer(f: &Form, report: &mut dyn FnMut(Msg)) -> Result<(PathBuf, u64), Box<dyn std::error::Error>> {
    let errs = validate(f).all();
    if !errs.is_empty() {
        return Err(errs.join("; ").into());
    }
    let src = PathBuf::from(f.source_dir.trim());

    let license_text = if f.license_file.trim().is_empty() {
        String::new()
    } else {
        String::from_utf8_lossy(&fs::read(f.license_file.trim())?).to_string()
    };

    let manifest = Manifest {
        app_name: f.app_name.trim().into(),
        version: f.version.trim().into(),
        publisher: f.publisher.trim().into(),
        url: f.url.trim().into(),
        main_exe: f.main_exe.trim().replace('/', "\\"),
        install_folder: if f.install_folder.trim().is_empty() { f.app_name.trim().into() } else { f.install_folder.trim().into() },
        license_text,
        desktop_icon: f.desktop,
        start_menu: f.start_menu,
        launch_after: f.launch,
    };

    // 1. The stub = this very exe (minus any payload it might carry).
    let me = std::env::current_exe()?;
    let stub_len = match payload::read_payload(&me) {
        Some(p) => p.zip_start,
        None => fs::metadata(&me)?.len(),
    };

    // 2. Compress the app folder into a temp zip (skipping the output folder if it lives inside).
    let out_dir = PathBuf::from(f.out_dir.trim());
    fs::create_dir_all(&out_dir)?;
    let out_dir_c = fs::canonicalize(&out_dir).unwrap_or(out_dir.clone());
    let out_path = out_dir.join(f.out_file_name());
    let skip = |p: &Path| fs::canonicalize(p).map(|c| c.starts_with(&out_dir_c)).unwrap_or(false);

    report(Msg::Log(format!("Scanning {}", src.display())));
    let entries: Vec<walkdir::DirEntry> = WalkDir::new(&src)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| !skip(e.path()))
        .collect::<Result<_, _>>()?;
    let total = entries.iter().filter(|e| e.file_type().is_file()).count();
    report(Msg::Total(total));

    let tmp = std::env::temp_dir().join(format!("outno-{}.tmp", std::process::id()));
    let result = (|| -> Result<usize, Box<dyn std::error::Error>> {
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).large_file(true);
        let mut zw = ZipWriter::new(File::create(&tmp)?);
        let mut count = 0usize;
        for entry in &entries {
            let rel = entry.path().strip_prefix(&src)?;
            if rel.as_os_str().is_empty() {
                continue;
            }
            let name = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
            if entry.file_type().is_dir() {
                zw.add_directory(name, opts)?;
            } else if entry.file_type().is_file() {
                report(Msg::Log(format!("Adding {name}")));
                zw.start_file(name, opts)?;
                io::copy(&mut File::open(entry.path())?, &mut zw)?;
                count += 1;
                report(Msg::Progress(count));
            }
        }
        zw.finish()?;
        Ok(count)
    })();
    let count = match result {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    let zip_len = fs::metadata(&tmp)?.len();

    // 3. stub + zip + manifest + trailer
    report(Msg::Log("Writing setup executable...".into()));
    let write = || -> Result<(), Box<dyn std::error::Error>> {
        let man = serde_json::to_vec(&manifest)?;
        let mut out = io::BufWriter::new(File::create(&out_path)?);
        io::copy(&mut File::open(&me)?.take(stub_len), &mut out)?;
        io::copy(&mut File::open(&tmp)?, &mut out)?;
        out.write_all(&man)?;
        out.write_all(&zip_len.to_le_bytes())?;
        out.write_all(&(man.len() as u64).to_le_bytes())?;
        out.write_all(MAGIC)?;
        out.flush()?;
        Ok(())
    };
    let res = write();
    let _ = fs::remove_file(&tmp);
    if let Err(e) = res {
        let _ = fs::remove_file(&out_path);
        return Err(format!("Can't write {}: {e}", out_path.display()).into());
    }

    let size = fs::metadata(&out_path)?.len();
    report(Msg::Log(format!("Done. {count} files, {} -> {}", ui::fmt_size(size), out_path.display())));
    Ok((out_path, size))
}
