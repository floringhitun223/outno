//! The "compiler": pick an app folder + options, produce a self-contained setup.exe.

use crate::payload::{self, Manifest, MAGIC};
use crate::{platform, ui};
use eframe::egui::{self, RichText};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use walkdir::WalkDir;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

#[derive(Clone)]
struct Form {
    app_name: String,
    version: String,
    publisher: String,
    url: String,
    source_dir: String,
    main_exe: String,
    license_file: String,
    install_folder: String,
    out_dir: String,
    out_name: String,
    desktop: bool,
    start_menu: bool,
    launch: bool,
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

#[derive(Default)]
struct BuildState {
    running: bool,
    log: Vec<String>,
    result: Option<Result<PathBuf, String>>,
}

pub fn run() -> eframe::Result<()> {
    let opts = ui::window_options("Rusty Setup Compiler", [760.0, 640.0]);
    eframe::run_native(
        "Rusty Setup Compiler",
        opts,
        Box::new(|cc| {
            ui::apply_classic(&cc.egui_ctx);
            Ok(Box::new(Builder { f: Form::default(), state: Arc::default() }))
        }),
    )
}

struct Builder {
    f: Form,
    state: Arc<Mutex<BuildState>>,
}

fn path_row(ui: &mut egui::Ui, label: &str, value: &mut String, browse: impl FnOnce() -> Option<String>) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(value).desired_width(430.0));
    if ui.button("Browse...").clicked() {
        if let Some(v) = browse() {
            *value = v;
        }
    }
    ui.end_row();
}

fn text_row(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(value).desired_width(430.0));
    ui.label("");
    ui.end_row();
}

impl eframe::App for Builder {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        let (running, log, result) = {
            let s = self.state.lock().unwrap();
            (s.running, s.log.clone(), s.result.clone())
        };
        if running {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }

        ui::header(ctx, "Rusty Setup Compiler", "Select an application folder and create an installable setup.exe");

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.label(RichText::new("Application").strong());
                egui::Grid::new("app").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                    text_row(ui, "Application name:", &mut self.f.app_name);
                    text_row(ui, "Version:", &mut self.f.version);
                    text_row(ui, "Publisher:", &mut self.f.publisher);
                    text_row(ui, "Website:", &mut self.f.url);
                });
                ui.add_space(10.0);
                ui.label(RichText::new("Files").strong());
                egui::Grid::new("files").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                    let mut picked_src: Option<String> = None;
                    path_row(ui, "Application folder:", &mut self.f.source_dir, || {
                        rfd::FileDialog::new().pick_folder().map(|p| {
                            picked_src = Some(p.display().to_string());
                            p.display().to_string()
                        })
                    });
                    if let Some(src) = picked_src {
                        self.autodetect(&src);
                    }
                    let src = PathBuf::from(&self.f.source_dir);
                    path_row(ui, "Main executable:", &mut self.f.main_exe, || {
                        rfd::FileDialog::new()
                            .set_directory(&src)
                            .add_filter("Executable", &["exe"])
                            .pick_file()
                            .and_then(|p| p.strip_prefix(&src).ok().map(|r| r.display().to_string()))
                    });
                    path_row(ui, "License file (.txt):", &mut self.f.license_file, || {
                        rfd::FileDialog::new()
                            .add_filter("Text", &["txt", "md", "rtf"])
                            .pick_file()
                            .map(|p| p.display().to_string())
                    });
                });
                ui.add_space(10.0);
                ui.label(RichText::new("Installer options").strong());
                egui::Grid::new("opts").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                    text_row(ui, "Default install folder:", &mut self.f.install_folder);
                });
                ui.checkbox(&mut self.f.desktop, "Offer \"Create a desktop shortcut\" (checked by default)");
                ui.checkbox(&mut self.f.start_menu, "Offer \"Create a Start Menu shortcut\" (checked by default)");
                ui.checkbox(&mut self.f.launch, "Offer \"Launch application\" on the finish page");
                ui.add_space(10.0);
                ui.label(RichText::new("Output").strong());
                egui::Grid::new("out").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                    path_row(ui, "Output folder:", &mut self.f.out_dir, || {
                        rfd::FileDialog::new().pick_folder().map(|p| p.display().to_string())
                    });
                    text_row(ui, "Setup file name:", &mut self.f.out_name);
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(!running, egui::Button::new("Build installer").min_size(egui::vec2(130.0, 28.0))).clicked() {
                        self.start_build();
                    }
                    if let Some(Ok(p)) = &result {
                        if ui.button("Open output folder").clicked() {
                            if let Some(parent) = p.parent() {
                                platform::open_folder(parent);
                            }
                        }
                    }
                });
                ui.add_space(8.0);
                match &result {
                    Some(Ok(p)) => { ui.colored_label(egui::Color32::from_rgb(0, 120, 0), format!("Success: {}", p.display())); }
                    Some(Err(e)) => { ui.colored_label(egui::Color32::from_rgb(180, 0, 0), format!("Error: {e}")); }
                    None => {}
                }
                egui::Frame::none().fill(egui::Color32::WHITE).inner_margin(6.0).show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    egui::ScrollArea::vertical().max_height(110.0).stick_to_bottom(true).show(ui, |ui| {
                        for l in &log {
                            ui.label(RichText::new(l).monospace().size(11.0));
                        }
                        if log.is_empty() {
                            ui.label(RichText::new("Compiler output appears here.").weak());
                        }
                    });
                });
            });
        });
    }
}

impl Builder {
    fn autodetect(&mut self, src: &str) {
        let name = Path::new(src).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        if self.f.app_name.is_empty() {
            self.f.app_name = name.clone();
        }
        if self.f.install_folder.is_empty() {
            self.f.install_folder = name;
        }
        if let Ok(rd) = fs::read_dir(src) {
            let mut exes: Vec<String> = rd
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("exe")))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            exes.sort();
            if let Some(first) = exes.first() {
                self.f.main_exe = first.clone();
            }
        }
    }

    fn start_build(&mut self) {
        let form = self.f.clone();
        let state = self.state.clone();
        {
            let mut s = state.lock().unwrap();
            s.running = true;
            s.log.clear();
            s.result = None;
        }
        thread::spawn(move || {
            let st = state.clone();
            let mut log = move |m: String| st.lock().unwrap().log.push(m);
            let res = build_installer(&form, &mut log).map_err(|e| e.to_string());
            let mut s = state.lock().unwrap();
            s.running = false;
            s.result = Some(res);
        });
    }
}

fn build_installer(f: &Form, log: &mut dyn FnMut(String)) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let src = PathBuf::from(f.source_dir.trim());
    if f.app_name.trim().is_empty() {
        return Err("Application name is required.".into());
    }
    if !src.is_dir() {
        return Err("Application folder does not exist.".into());
    }
    if f.main_exe.trim().is_empty() || !src.join(f.main_exe.trim()).is_file() {
        return Err("Main executable not found inside the application folder.".into());
    }

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
        main_exe: f.main_exe.trim().into(),
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
    let mut stub = vec![0u8; stub_len as usize];
    File::open(&me)?.read_exact(&mut stub)?;

    // 2. Compress the app folder into a temp zip.
    let out_dir = PathBuf::from(f.out_dir.trim());
    fs::create_dir_all(&out_dir)?;
    let out_name = if f.out_name.trim().is_empty() {
        format!("{}-setup-{}.exe", manifest.app_name.replace(' ', "_"), manifest.version)
    } else if f.out_name.trim().to_lowercase().ends_with(".exe") {
        f.out_name.trim().to_string()
    } else {
        format!("{}.exe", f.out_name.trim())
    };
    let out_path = out_dir.join(out_name);

    let tmp = std::env::temp_dir().join(format!("rsinst-{}.tmp", std::process::id()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).large_file(true);
    let mut zw = ZipWriter::new(File::create(&tmp)?);
    let mut count = 0usize;
    for entry in WalkDir::new(&src).sort_by_file_name() {
        let entry = entry?;
        let path = entry.path();
        if path == out_path {
            continue; // never pack the output into itself
        }
        let rel = path.strip_prefix(&src)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let name = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
        if entry.file_type().is_dir() {
            zw.add_directory(name, opts)?;
        } else if entry.file_type().is_file() {
            log(format!("Adding {name}"));
            zw.start_file(name, opts)?;
            io::copy(&mut File::open(path)?, &mut zw)?;
            count += 1;
        }
    }
    zw.finish()?;
    let zip_len = fs::metadata(&tmp)?.len();

    // 3. stub + zip + manifest + trailer
    log("Writing setup executable...".into());
    let man = serde_json::to_vec(&manifest)?;
    let mut out = File::create(&out_path)?;
    out.write_all(&stub)?;
    io::copy(&mut File::open(&tmp)?, &mut out)?;
    out.write_all(&man)?;
    out.write_all(&zip_len.to_le_bytes())?;
    out.write_all(&(man.len() as u64).to_le_bytes())?;
    out.write_all(MAGIC)?;
    out.flush()?;
    let _ = fs::remove_file(&tmp);

    log(format!("Done. {count} files, {}", ui::fmt_size(fs::metadata(&out_path)?.len())));
    Ok(out_path)
}
