use crate::lang::{self, Lang};
use crate::payload::{self, InstallMode, Manifest, Payload};
use crate::platform::{self, quote_arg, Previous};
use crate::taskbar::{self, Taskbar};
use crate::ui::{self, Nav};
use crate::win;
use eframe::egui::{self, Color32, RichText};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const UNINSTALLER: &str = "unins000.exe";
const UNINSTALL_LOG: &str = "unins000.dat";

// ───────────────────────────── Shared install logic ─────────────────────────────

/// What to install where. Also handed to the elevated helper process as JSON.
#[derive(Serialize, Deserialize, Clone)]
pub struct Job {
    pub dir: PathBuf,
    /// Shortcut folders, resolved by the (non-elevated) wizard for the right user.
    pub desktop_dir: Option<PathBuf>,
    pub start_menu_dir: Option<PathBuf>,
    pub all_users: bool,
    pub lang: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Status {
    done: usize,
    total: usize,
    current: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct UninstallLog {
    pub app_name: String,
    pub version: String,
    pub dir: String,
    pub files: Vec<String>,
    pub dirs: Vec<String>,
    pub shortcuts: Vec<String>,
    pub reg_key: String,
    pub all_users: bool,
    pub lang_code: String,
    pub lang: BTreeMap<String, String>,
}

fn pick_lang(m: &Manifest, code: &str) -> Lang {
    m.languages
        .iter()
        .find(|p| p.code == code)
        .or(m.languages.first())
        .map(Lang::from_pack)
        .unwrap_or_else(Lang::english)
}

/// Turn an io error on `path` into a message the user can act on.
fn io_err(lang: &Lang, app: &str, path: &Path, e: &io::Error) -> String {
    let p = path.display().to_string();
    if platform::is_in_use_error(e) {
        lang.fmt("err_in_use", &[("path", &p), ("app", app)])
    } else if e.kind() == io::ErrorKind::PermissionDenied {
        lang.fmt("err_denied", &[("path", &p)])
    } else if matches!(e.raw_os_error(), Some(39) | Some(112)) {
        lang.fmt("err_disk_full", &[("path", &p)])
    } else {
        lang.fmt("err_generic", &[("path", &p), ("error", &e.to_string())])
    }
}

/// Append `old` to `into`, keeping the first occurrence of each path (case-insensitive).
fn merge(into: &mut Vec<String>, old: Vec<String>) {
    let mut seen = HashSet::new();
    let all: Vec<String> = into.drain(..).chain(old).collect();
    into.extend(all.into_iter().filter(|s| seen.insert(s.to_lowercase())));
}

/// Extract the app, write the uninstaller, shortcuts and registry entry.
/// `Ok(Some(warning))`: installed, but the Installed apps entry couldn't be written.
fn do_install(payload: &Payload, job: &Job, lang: &Lang, progress: &mut dyn FnMut(&Status)) -> Result<Option<String>, String> {
    let m = &payload.manifest;
    let app = m.app_name.as_str();
    let dir = job.dir.as_path();
    let ioe = |p: &Path, e: &io::Error| io_err(lang, app, p, e);

    let mut zip = payload::open_zip(payload).map_err(|e| e.to_string())?;
    let entries: Vec<usize> = (0..zip.len())
        .filter(|&i| zip.by_index_raw(i).map(|e| !e.name().starts_with(payload::ASSET_PREFIX)).unwrap_or(false))
        .collect();
    let mut st = Status { total: entries.len(), ..Default::default() };
    progress(&st);

    fs::create_dir_all(dir).map_err(|e| ioe(dir, &e))?;

    // An earlier install's log is merged, so the uninstaller also removes old files.
    let old_log: Option<UninstallLog> =
        fs::read(dir.join(UNINSTALL_LOG)).ok().and_then(|b| serde_json::from_slice(&b).ok());

    let mut log = UninstallLog {
        app_name: m.app_name.clone(),
        version: m.version.clone(),
        dir: dir.display().to_string(),
        reg_key: platform::registry_key_name(m),
        all_users: job.all_users,
        lang_code: lang.code.clone(),
        lang: lang.strings().clone(),
        ..Default::default()
    };
    let mut bytes = 0u64;

    for (n, &i) in entries.iter().enumerate() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(rel) = entry.enclosed_name() else { continue }; // blocks ../ path tricks
        let target = dir.join(&rel);
        st.current = rel.display().to_string();
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| ioe(&target, &e))?;
            log.dirs.push(target.display().to_string());
        } else {
            if let Some(p) = target.parent() {
                fs::create_dir_all(p).map_err(|e| ioe(p, &e))?;
            }
            platform::make_writable(&target);
            let mut out = File::create(&target).map_err(|e| ioe(&target, &e))?;
            bytes += io::copy(&mut entry, &mut out).map_err(|e| ioe(&target, &e))?;
            log.files.push(target.display().to_string());
        }
        st.done = n + 1;
        progress(&st);
    }

    // Uninstaller: the separately signed copy if the setup has one, else this exe's stub.
    let unins = dir.join(UNINSTALLER);
    platform::make_writable(&unins);
    {
        let mut out = io::BufWriter::new(File::create(&unins).map_err(|e| ioe(&unins, &e))?);
        let res = match zip.by_name(payload::ASSET_UNINSTALLER) {
            Ok(mut signed) => io::copy(&mut signed, &mut out).map(|_| ()),
            Err(_) => payload::write_stub(&payload.exe_path, &mut out).map(|_| ()),
        };
        res.and_then(|_| io::Write::flush(&mut out)).map_err(|e| ioe(&unins, &e))?;
    }
    log.files.push(unins.display().to_string());

    // Shortcuts
    let target = dir.join(&m.main_exe);
    let lnk_name = format!("{}.lnk", m.app_name);
    let shortcut = |folder: &Path, log: &mut UninstallLog| -> Result<(), String> {
        let lnk = folder.join(&lnk_name);
        platform::create_shortcut(&target, &lnk, dir)
            .map_err(|e| lang.fmt("err_generic", &[("path", &lnk.display().to_string()), ("error", &e)]))?;
        log.shortcuts.push(lnk.display().to_string());
        Ok(())
    };
    if let Some(d) = &job.desktop_dir {
        shortcut(d, &mut log)?;
    }
    if let Some(d) = &job.start_menu_dir {
        shortcut(d, &mut log)?;
        log.dirs.push(d.display().to_string());
    }

    log.dirs.push(dir.display().to_string());
    let old = old_log.unwrap_or_default();
    merge(&mut log.files, old.files);
    merge(&mut log.dirs, old.dirs);
    merge(&mut log.shortcuts, old.shortcuts);
    let log_path = dir.join(UNINSTALL_LOG);
    platform::make_writable(&log_path);
    let json = serde_json::to_vec_pretty(&log).map_err(|e| e.to_string())?;
    fs::write(&log_path, json).map_err(|e| ioe(&log_path, &e))?;

    // The app is fully installed at this point: a registry problem is only a warning.
    let size_kb = (bytes / 1024).min(u32::MAX as u64) as u32;
    let warning = platform::register_uninstall(m, dir, &unins, job.all_users, size_kb)
        .err()
        .map(|e| lang.fmt("warn_registry", &[("app", app), ("error", &e)]));
    Ok(warning)
}

/// Elevated helper:  setup.exe --elevated-install <job.json>
/// No window; progress goes to `<job>.status`, a failure message to `<job>.error`.
pub fn run_elevated_job(payload: Payload, job_path: &Path) -> i32 {
    let job: Job = match fs::read(job_path).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
        Some(j) => j,
        None => return 2,
    };
    let lang = pick_lang(&payload.manifest, &job.lang);
    let status_path = job_path.with_extension("status");
    let tmp_path = job_path.with_extension("status.tmp");
    let mut last = Instant::now() - Duration::from_secs(1);
    let res = do_install(&payload, &job, &lang, &mut |s| {
        if last.elapsed() >= Duration::from_millis(80) || s.done == s.total {
            last = Instant::now();
            if let Ok(b) = serde_json::to_vec(s) {
                if fs::write(&tmp_path, b).is_ok() {
                    let _ = fs::rename(&tmp_path, &status_path);
                }
            }
        }
    });
    match res {
        Ok(warning) => {
            if let Some(w) = warning {
                let _ = fs::write(job_path.with_extension("warning"), w);
            }
            0
        }
        Err(e) => {
            let _ = fs::write(job_path.with_extension("error"), e);
            1
        }
    }
}

// ───────────────────────────── Installer wizard ─────────────────────────────

#[derive(PartialEq, Clone, Copy)]
enum Page {
    Language,
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
    status: Status,
    /// Elevated install started, but the helper hasn't reported yet (UAC prompt open).
    waiting_admin: bool,
    finished: bool,
    error: Option<String>,
    /// Installed, but with a non-fatal problem (shown on the Finish page).
    warning: Option<String>,
}

pub fn run(payload: Payload) -> eframe::Result<()> {
    let title = format!("Setup - {}", payload.manifest.app_name);
    let opts = ui::window_options(&title, [620.0, 460.0], false);
    if let Some(rgb) = payload.manifest.accent {
        ui::set_accent(rgb);
    }
    eframe::run_native(
        &title,
        opts,
        Box::new(move |cc| {
            let mut brand = ui::setup(&cc.egui_ctx);
            brand.wizard = payload::read_entry(&payload, payload::ASSET_WIZARD_IMAGE)
                .and_then(|b| ui::load_png(&cc.egui_ctx, "wizard", &b));
            brand.header = payload::read_entry(&payload, payload::ASSET_HEADER_IMAGE)
                .and_then(|b| ui::load_png(&cc.egui_ctx, "header", &b));
            Ok(Box::new(Wizard::new(payload, brand)))
        }),
    )
}

struct Wizard {
    payload: Payload,
    m: Arc<Manifest>,
    brand: ui::Brand,
    lang: Lang,
    lang_idx: usize,
    page: Page,
    accepted: bool,
    dir: String,
    all_users: bool,
    previous: Option<Previous>,
    desktop: bool,
    start_menu: bool,
    launch: bool,
    total_size: u64,
    app_running: bool,
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
            .map(|mut z| {
                (0..z.len())
                    .filter_map(|i| z.by_index_raw(i).ok().filter(|e| !e.name().starts_with(payload::ASSET_PREFIX)).map(|e| e.size()))
                    .sum()
            })
            .unwrap_or(0);

        let previous = platform::previous_install(&m);
        let all_users = previous.as_ref().map_or(m.install_mode == InstallMode::AllUsers, |p| p.all_users);
        let dir = match &previous {
            Some(p) => p.dir.clone(),
            None => platform::install_root(all_users).join(&m.install_folder),
        };

        let lang_idx = lang::best_match(&m.languages, &win::ui_language());
        let lang = m.languages.get(lang_idx).map(Lang::from_pack).unwrap_or_else(Lang::english);
        let page = if m.languages.len() > 1 { Page::Language } else { Page::Welcome };

        Self {
            desktop: m.desktop_icon,
            start_menu: m.start_menu,
            launch: m.launch_after,
            dir: dir.display().to_string(),
            all_users,
            previous,
            payload,
            m,
            brand,
            lang,
            lang_idx,
            page,
            accepted: false,
            total_size,
            app_running: false,
            confirm_cancel: false,
            progress: Arc::new(Mutex::new(Progress::default())),
            taskbar: Taskbar::default(),
            attention_sent: false,
        }
    }

    fn t(&self, key: &str) -> String {
        self.lang.fmt(key, &[("app", &self.m.app_name), ("version", &self.m.version)])
    }

    fn has_license(&self) -> bool {
        !self.m.license_text.trim().is_empty()
    }

    /// The user may switch between per-user and all-users installs.
    fn can_choose_mode(&self) -> bool {
        self.m.install_mode == InstallMode::Ask && self.previous.is_none()
    }

    fn dir_error(&self) -> Option<String> {
        let d = self.dir.trim();
        if d.is_empty() {
            Some(self.t("dir_empty"))
        } else if !Path::new(d).is_absolute() {
            Some(self.t("dir_relative"))
        } else if Path::new(d).is_file() {
            Some(self.t("dir_is_file"))
        } else {
            None
        }
    }

    /// Installing here needs a UAC prompt (all-users install, or a protected folder).
    fn needs_elevation(&self) -> bool {
        !win::is_admin() && (self.all_users || !platform::can_write(Path::new(self.dir.trim())))
    }

    fn set_all_users(&mut self, all: bool) {
        if all == self.all_users {
            return;
        }
        // Move the suggested folder along, unless the user typed their own.
        let old_default = platform::install_root(self.all_users).join(&self.m.install_folder);
        if Path::new(self.dir.trim()) == old_default {
            self.dir = platform::install_root(all).join(&self.m.install_folder).display().to_string();
        }
        self.all_users = all;
    }

    fn go_next(&mut self, ctx: &egui::Context) {
        self.page = match self.page {
            Page::Language => Page::Welcome,
            Page::Welcome if self.has_license() => Page::License,
            Page::Welcome | Page::License => Page::Dir,
            Page::Dir => Page::Tasks,
            Page::Tasks => {
                self.app_running = platform::file_in_use(&Path::new(self.dir.trim()).join(&self.m.main_exe));
                Page::Ready
            }
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
            Page::Welcome if self.m.languages.len() > 1 => Page::Language,
            Page::Dir if self.has_license() => Page::License,
            Page::Dir => Page::Welcome,
            Page::License => Page::Welcome,
            Page::Tasks => Page::Dir,
            Page::Ready => Page::Tasks,
            p => p,
        };
    }

    fn job(&self) -> Job {
        let app = &self.m.app_name;
        Job {
            dir: PathBuf::from(self.dir.trim()),
            desktop_dir: if self.desktop { platform::desktop_dir(self.all_users) } else { None },
            start_menu_dir: if self.start_menu { platform::start_menu_dir(app, self.all_users) } else { None },
            all_users: self.all_users,
            lang: self.lang.code.clone(),
        }
    }

    fn start_install(&mut self, ctx: &egui::Context) {
        *self.progress.lock().unwrap() = Progress::default();
        self.attention_sent = false;
        let job = self.job();
        let prog = self.progress.clone();
        let ctx = ctx.clone();

        if self.needs_elevation() {
            prog.lock().unwrap().waiting_admin = true;
            let lang = self.lang.clone();
            thread::spawn(move || {
                let res = install_elevated(&job, &lang, &prog, &ctx);
                let mut p = prog.lock().unwrap();
                match res {
                    Ok(w) => p.warning = w,
                    Err(e) => p.error = Some(e),
                }
                p.finished = true;
                ctx.request_repaint();
            });
        } else {
            let payload = self.payload.clone();
            let lang = self.lang.clone();
            thread::spawn(move || {
                let res = do_install(&payload, &job, &lang, &mut |s| {
                    prog.lock().unwrap().status = s.clone();
                    ctx.request_repaint();
                });
                let mut p = prog.lock().unwrap();
                match res {
                    Ok(w) => p.warning = w,
                    Err(e) => p.error = Some(e),
                }
                p.finished = true;
                ctx.request_repaint();
            });
        }
    }

    /// Mirror the install progress on the taskbar button and flash it when done.
    fn update_taskbar(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let (status, waiting, finished, failed) = {
            let p = self.progress.lock().unwrap();
            (p.status.clone(), p.waiting_admin, p.finished, p.error.is_some())
        };
        let state = if self.page == Page::Installing {
            let frac = if status.total == 0 { 0.0 } else { status.done as f32 / status.total as f32 };
            if failed {
                taskbar::State::Error(frac.max(0.05))
            } else if waiting || status.total == 0 {
                taskbar::State::Indeterminate
            } else {
                taskbar::State::Normal(frac)
            }
        } else {
            taskbar::State::None
        };
        self.taskbar.set(frame, state);

        if finished && !self.attention_sent {
            self.attention_sent = true;
            if !ctx.input(|i| i.focused) {
                let kind = if failed { egui::UserAttentionType::Critical } else { egui::UserAttentionType::Informational };
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(kind));
            }
        }
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

    fn panel(fill: Color32, x: f32, y: f32) -> egui::Frame {
        egui::Frame::none().fill(fill).inner_margin(egui::Margin::symmetric(x, y))
    }
}

/// Run the install in an elevated copy of this setup and relay its progress.
fn install_elevated(job: &Job, lang: &Lang, prog: &Arc<Mutex<Progress>>, ctx: &egui::Context) -> Result<Option<String>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let job_path = std::env::temp_dir().join(format!("outno-job-{}-{stamp}.json", std::process::id()));
    let status_path = job_path.with_extension("status");
    let error_path = job_path.with_extension("error");
    let warning_path = job_path.with_extension("warning");
    fs::write(&job_path, serde_json::to_vec(job).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;

    // Poll the helper's status file while we wait for it.
    let done = Arc::new(AtomicBool::new(false));
    let poller = {
        let (done, prog, ctx, status_path) = (done.clone(), prog.clone(), ctx.clone(), status_path.clone());
        thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if let Some(s) = fs::read(&status_path).ok().and_then(|b| serde_json::from_slice::<Status>(&b).ok()) {
                    let mut p = prog.lock().unwrap();
                    p.waiting_admin = false;
                    p.status = s;
                    ctx.request_repaint();
                }
                thread::sleep(Duration::from_millis(100));
            }
        })
    };

    let params = format!("--elevated-install {}", quote_arg(&job_path.display().to_string()));
    let res = win::run_elevated_and_wait(&exe, &params);
    done.store(true, Ordering::Relaxed);
    let _ = poller.join();

    let out = match res {
        win::Elevated::Exited(0) => Ok(fs::read_to_string(&warning_path).ok()),
        win::Elevated::Exited(code) => {
            Err(fs::read_to_string(&error_path).unwrap_or_else(|_| format!("Setup helper failed (exit code {code}).")))
        }
        win::Elevated::Cancelled => Err(lang.get("err_no_admin")),
        win::Elevated::Failed(e) => Err(format!("{}\n{e}", lang.get("err_no_admin"))),
    };
    for p in [&job_path, &status_path, &error_path, &warning_path, &job_path.with_extension("status.tmp")] {
        let _ = fs::remove_file(p);
    }
    out
}

impl eframe::App for Wizard {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let m = self.m.clone();
        // Dialog opened this frame must not react to the same key press.
        let dialog_open = self.confirm_cancel;

        let (install_error, finished, warning) = {
            let p = self.progress.lock().unwrap();
            (p.error.clone(), p.finished, p.warning.clone())
        };
        if self.page == Page::Installing {
            if finished && install_error.is_none() {
                self.page = Page::Finished;
            }
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        self.update_taskbar(ctx, frame);

        // Window close button: ask first, and never interrupt a running install.
        if ctx.input(|i| i.viewport().close_requested()) {
            let busy = self.page == Page::Installing && !finished;
            let done = self.page == Page::Finished || install_error.is_some();
            if busy {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else if !done && !self.confirm_cancel {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm_cancel = true;
            }
        }

        let (next, back, cancel) = (self.t("btn_next"), self.t("btn_back"), self.t("btn_cancel"));
        let version_line = self.t("banner_version");

        let nav = match self.page {
            Page::Language => {
                let nav = ui::footer(ctx, None, &self.t("btn_ok"), true, Some((true, &cancel)));
                ui::header(ctx, &self.brand, &self.t("lang_title"), &self.t("lang_sub"));
                egui::CentralPanel::default().frame(Self::panel(ui::BG, 22.0, 24.0)).show(ctx, |ui| {
                    let mut idx = self.lang_idx;
                    egui::ComboBox::from_id_salt("lang")
                        .width(280.0)
                        .selected_text(m.languages.get(idx).map(|p| p.name.clone()).unwrap_or_default())
                        .show_ui(ui, |ui| {
                            for (i, p) in m.languages.iter().enumerate() {
                                ui.selectable_value(&mut idx, i, &p.name);
                            }
                        });
                    if idx != self.lang_idx {
                        self.lang_idx = idx;
                        self.lang = Lang::from_pack(&m.languages[idx]);
                    }
                });
                nav
            }

            Page::Welcome => {
                let back_btn = (m.languages.len() > 1).then_some((true, back.as_str()));
                let nav = ui::footer(ctx, back_btn, &next, true, Some((true, &cancel)));
                ui::side_banner(ctx, &self.brand, &m.app_name, &version_line);
                egui::CentralPanel::default().frame(Self::panel(Color32::WHITE, 26.0, 20.0)).show(ctx, |ui| {
                    ui.add_space(18.0);
                    ui.label(ui::bold(self.t("welcome_title"), 20.0));
                    ui.add_space(16.0);
                    Self::body(ui, self.t("welcome_body"));
                    if let Some(prev) = &self.previous {
                        ui.add_space(6.0);
                        let old = if prev.version.is_empty() { String::new() } else { prev.version.clone() };
                        let msg = self.lang.fmt("welcome_upgrade", &[("app", &m.app_name), ("old", &old), ("version", &m.version)]);
                        ui.label(RichText::new(msg.replace("  ", " ")).size(14.0).color(ui::accent()));
                    }
                    ui.add_space(6.0);
                    Self::body(ui, self.t("welcome_close"));
                    ui.add_space(6.0);
                    Self::body(ui, self.t("welcome_next"));
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        if !m.url.is_empty() {
                            ui.hyperlink_to(RichText::new(&m.url).color(ui::accent()), &m.url);
                        }
                        if !m.publisher.is_empty() {
                            ui.label(ui::muted(self.lang.fmt("published_by", &[("publisher", &m.publisher)])));
                        }
                    });
                });
                nav
            }

            Page::License => {
                let nav = ui::footer(ctx, Some((true, &back)), &next, self.accepted, Some((true, &cancel)));
                ui::header(ctx, &self.brand, &self.t("lic_title"), &self.t("lic_sub"));
                egui::CentralPanel::default().frame(Self::panel(ui::BG, 22.0, 14.0)).show(ctx, |ui| {
                    Self::body(ui, self.t("lic_body"));
                    ui.add_space(4.0);
                    ui::inset_frame().show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(176.0).auto_shrink([false, false]).show(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(&m.license_text).size(13.0)).wrap());
                        });
                    });
                    ui.add_space(6.0);
                    if ui.radio(self.accepted, self.t("lic_accept")).clicked() {
                        self.accepted = true;
                    }
                    if ui.radio(!self.accepted, self.t("lic_decline")).clicked() {
                        self.accepted = false;
                    }
                });
                nav
            }

            Page::Dir => {
                let err = self.dir_error();
                let nav = ui::footer(ctx, Some((true, &back)), &next, err.is_none(), Some((true, &cancel)));
                ui::header(ctx, &self.brand, &self.t("dir_title"), &self.t("dir_sub"));
                let elevate = err.is_none() && self.needs_elevation();
                egui::CentralPanel::default().frame(Self::panel(ui::BG, 22.0, 16.0)).show(ctx, |ui| {
                    if self.can_choose_mode() {
                        ui.label(ui::bold(self.t("mode_label"), 13.5));
                        let mut all = self.all_users;
                        ui.radio_value(&mut all, false, self.t("mode_user"));
                        ui.radio_value(&mut all, true, self.t("mode_all"));
                        self.set_all_users(all);
                        ui.add_space(10.0);
                    }
                    Self::body(ui, self.t("dir_body"));
                    ui.add_space(2.0);
                    ui.label(ui::muted(self.t("dir_hint")));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(ui::button(self.t("btn_browse"))).clicked() {
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
                    if let Some(e) = &err {
                        ui.label(RichText::new(e).color(ui::ERROR).size(12.5));
                    } else if elevate {
                        ui.label(RichText::new(self.t("dir_admin")).color(ui::accent()).size(12.5));
                    }
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        ui.label(ui::muted(self.lang.fmt("dir_space", &[("size", &ui::fmt_size(self.total_size))])));
                    });
                });
                nav
            }

            Page::Tasks => {
                let nav = ui::footer(ctx, Some((true, &back)), &next, true, Some((true, &cancel)));
                ui::header(ctx, &self.brand, &self.t("tasks_title"), &self.t("tasks_sub"));
                egui::CentralPanel::default().frame(Self::panel(ui::BG, 22.0, 18.0)).show(ctx, |ui| {
                    Self::body(ui, self.t("tasks_body"));
                    ui.add_space(12.0);
                    ui::inset_frame().inner_margin(egui::Margin::symmetric(14.0, 12.0)).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(ui::bold(self.t("tasks_shortcuts"), 13.5));
                        ui.add_space(2.0);
                        let label = self.t("task_desktop");
                        ui.checkbox(&mut self.desktop, label);
                        let label = self.t("task_startmenu");
                        ui.checkbox(&mut self.start_menu, label);
                    });
                });
                nav
            }

            Page::Ready => {
                let nav = ui::footer(ctx, Some((true, &back)), &self.t("btn_install"), true, Some((true, &cancel)));
                ui::header(ctx, &self.brand, &self.t("ready_title"), &self.t("ready_sub"));
                egui::CentralPanel::default().frame(Self::panel(ui::BG, 22.0, 14.0)).show(ctx, |ui| {
                    Self::body(ui, self.t("ready_body"));
                    if self.app_running {
                        ui.label(RichText::new(self.t("ready_running")).color(ui::ERROR).size(13.0));
                    }
                    ui.add_space(6.0);
                    ui::inset_frame().inner_margin(egui::Margin::symmetric(14.0, 10.0)).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = 4.0;
                        Self::summary_row(ui, &self.t("ready_dest"), &[self.dir.trim().to_string()]);
                        if m.install_mode != InstallMode::PerUser || self.all_users {
                            let scope = if self.all_users { self.t("scope_all") } else { self.t("scope_user") };
                            Self::summary_row(ui, &self.t("ready_scope"), &[scope]);
                        }
                        Self::summary_row(ui, &self.t("ready_space"), &[ui::fmt_size(self.total_size)]);
                        let mut tasks = Vec::new();
                        if self.desktop {
                            tasks.push(self.t("task_desktop"));
                        }
                        if self.start_menu {
                            tasks.push(self.t("task_startmenu"));
                        }
                        if !tasks.is_empty() {
                            Self::summary_row(ui, &self.t("ready_tasks"), &tasks);
                        }
                    });
                });
                nav
            }

            Page::Installing => {
                let failed = install_error.is_some();
                let retry = self.t("btn_retry");
                let nav = if failed {
                    ui::footer(ctx, Some((true, &retry)), &self.t("btn_close"), true, None)
                } else {
                    ui::footer(ctx, None, &next, false, Some((false, &cancel)))
                };
                ui::header(ctx, &self.brand, &self.t("inst_title"), &self.t("inst_sub"));
                let (status, waiting) = {
                    let p = self.progress.lock().unwrap();
                    (p.status.clone(), p.waiting_admin)
                };
                egui::CentralPanel::default().frame(Self::panel(ui::BG, 22.0, 22.0)).show(ctx, |ui| {
                    if let Some(e) = &install_error {
                        ui.label(ui::bold(self.t("inst_failed"), 15.0).color(ui::ERROR));
                        ui.add_space(6.0);
                        ui.add(egui::Label::new(RichText::new(e).size(14.0)).wrap());
                        ui.add_space(6.0);
                        ui.label(ui::muted(self.t("inst_failed_hint")));
                    } else if waiting {
                        ui.label(ui::bold(self.t("inst_admin_wait"), 14.0));
                        ui.add_space(6.0);
                        ui.add(egui::ProgressBar::new(0.0).fill(ui::accent()).desired_width(ui.available_width()).animate(true));
                    } else {
                        ui.label(ui::bold(self.t("inst_extracting"), 14.0));
                        ui.add_space(6.0);
                        let frac = if status.total == 0 { 0.0 } else { status.done as f32 / status.total as f32 };
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .fill(ui::accent())
                                .desired_width(ui.available_width())
                                .desired_height(20.0)
                                .show_percentage(),
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(ui::muted(&status.current).size(12.5)).truncate());
                        });
                        let items = self.lang.fmt("inst_items", &[("done", &status.done.to_string()), ("total", &status.total.to_string())]);
                        ui.label(ui::muted(items).size(12.5));
                    }
                });
                match nav {
                    Nav::Next if failed => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    Nav::Back if failed => self.start_install(ctx),
                    _ => {}
                }
                Nav::None
            }

            Page::Finished => {
                let nav = ui::footer(ctx, None, &self.t("btn_finish"), true, None);
                ui::side_banner(ctx, &self.brand, &m.app_name, &version_line);
                egui::CentralPanel::default().frame(Self::panel(Color32::WHITE, 26.0, 20.0)).show(ctx, |ui| {
                    ui.add_space(18.0);
                    ui.label(ui::bold(self.t("fin_title"), 20.0));
                    ui.add_space(16.0);
                    Self::body(ui, self.t("fin_body"));
                    ui.add_space(6.0);
                    Self::body(ui, self.t("fin_hint"));
                    if let Some(w) = &warning {
                        ui.add_space(10.0);
                        ui.add(egui::Label::new(RichText::new(w).size(12.5).color(ui::WARNING)).wrap());
                    }
                    if m.launch_after {
                        ui.add_space(18.0);
                        let label = self.t("fin_launch");
                        ui.checkbox(&mut self.launch, label);
                    }
                });
                nav
            }
        };

        if dialog_open {
            let lines = [self.t("exit_body"), self.t("exit_q")];
            let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
            if let Some(yes) = ui::confirm_dialog(ctx, &self.t("exit_title"), &lines, &self.t("btn_exit"), &self.t("btn_continue")) {
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

// ───────────────────────────── Uninstaller ─────────────────────────────

fn read_log(dir: &Path) -> Option<UninstallLog> {
    fs::read(dir.join(UNINSTALL_LOG)).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

/// Remove everything the log lists. Best effort: keeps going and reports the first failure.
fn remove_all(dir: &Path, log: &UninstallLog, lang: &Lang) -> Result<(), String> {
    let mut first_error: Option<String> = None;
    let current = std::env::current_exe().ok().and_then(|p| fs::canonicalize(p).ok());
    for f in log.files.iter().chain(&log.shortcuts) {
        let p = Path::new(f);
        if !p.exists() {
            continue;
        }
        // The uninstaller can't delete itself while running (only when not run from temp).
        if current.is_some() && fs::canonicalize(p).ok() == current {
            continue;
        }
        platform::make_writable(p);
        if let Err(e) = fs::remove_file(p) {
            first_error.get_or_insert_with(|| io_err(lang, &log.app_name, p, &e));
        }
    }
    let _ = fs::remove_file(dir.join(UNINSTALL_LOG));
    let mut dirs = log.dirs.clone();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.len())); // children first
    for d in dirs {
        let _ = fs::remove_dir(d);
    }
    platform::unregister_uninstall(&log.reg_key, log.all_users);
    first_error.map_or(Ok(()), Err)
}

/// Started as `unins000.exe --uninstall <dir>` from inside the install folder:
/// copy ourselves to %TEMP% and run from there, so the folder can be fully removed.
/// Returns true if the copy was started (this process should exit).
pub fn relaunch_from_temp(dir: &Path, args: &[String]) -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let (Ok(exe_c), Ok(dir_c)) = (fs::canonicalize(&exe), fs::canonicalize(dir)) else { return false };
    if !exe_c.starts_with(&dir_c) {
        return false;
    }
    let tmp = std::env::temp_dir().join(format!("outno-unins-{}.exe", std::process::id()));
    if fs::copy(&exe, &tmp).is_err() {
        return false;
    }
    let mut a: Vec<String> = args.iter().skip(1).cloned().collect();
    a.push("--from-temp".into());
    std::process::Command::new(&tmp).args(a).spawn().is_ok()
}

/// Elevated helper for all-users uninstalls (no window). Exit code 0 = success.
pub fn uninstall_headless(dir: &Path) -> i32 {
    let Some(log) = read_log(dir) else { return 2 };
    let lang = Lang::from_strings(&log.lang_code, log.lang.clone());
    match remove_all(dir, &log, &lang) {
        Ok(()) => 0,
        Err(e) => {
            let _ = fs::write(std::env::temp_dir().join(format!("outno-unins-{}.error", log.reg_key)), e);
            1
        }
    }
}

pub fn run_uninstaller(dir: PathBuf, from_temp: bool) -> eframe::Result<()> {
    let log = read_log(&dir);
    let lang = log.as_ref().map(|l| Lang::from_strings(&l.lang_code, l.lang.clone())).unwrap_or_else(Lang::english);
    let name = log.as_ref().map(|l| l.app_name.clone()).unwrap_or_else(|| "application".into());
    let title = lang.fmt("un_title", &[("app", &name)]);
    let opts = ui::window_options(&title, [540.0, 340.0], false);
    eframe::run_native(
        &title,
        opts,
        Box::new(move |cc| {
            let brand = ui::setup(&cc.egui_ctx);
            Ok(Box::new(Uninstall { dir, log, lang, brand, from_temp, state: Arc::default() }))
        }),
    )
}

#[derive(Default, Clone, PartialEq)]
enum UnState {
    #[default]
    Confirm,
    Working,
    Done,
    Failed(String),
}

struct Uninstall {
    dir: PathBuf,
    log: Option<UninstallLog>,
    lang: Lang,
    brand: ui::Brand,
    from_temp: bool,
    state: Arc<Mutex<UnState>>,
}

impl Uninstall {
    fn start(&self, ctx: &egui::Context) {
        let Some(log) = &self.log else { return };
        *self.state.lock().unwrap() = UnState::Working;
        let (state, ctx, dir, lang) = (self.state.clone(), ctx.clone(), self.dir.clone(), self.lang.clone());
        let (all_users, reg_key) = (log.all_users, log.reg_key.clone());
        let from_temp = self.from_temp;
        let log = log.clone();
        thread::spawn(move || {
            let res = if all_users && !win::is_admin() {
                let exe = std::env::current_exe().map_err(|e| e.to_string());
                exe.and_then(|exe| {
                    let params = format!("--uninstall {} --from-temp --elevated-job", quote_arg(&dir.display().to_string()));
                    match win::run_elevated_and_wait(&exe, &params) {
                        win::Elevated::Exited(0) => Ok(()),
                        win::Elevated::Exited(_) => {
                            let p = std::env::temp_dir().join(format!("outno-unins-{reg_key}.error"));
                            let msg = fs::read_to_string(&p).unwrap_or_else(|_| lang.get("un_failed"));
                            let _ = fs::remove_file(p);
                            Err(msg)
                        }
                        win::Elevated::Cancelled => Err(lang.get("err_no_admin")),
                        win::Elevated::Failed(e) => Err(format!("{}\n{e}", lang.get("err_no_admin"))),
                    }
                })
            } else {
                remove_all(&dir, &log, &lang)
            };
            if res.is_ok() {
                if let Ok(exe) = std::env::current_exe() {
                    // From temp: just remove the temp copy. In place: also the (now empty) folder.
                    platform::schedule_self_delete(&exe, if from_temp { None } else { Some(&dir) });
                }
            }
            *state.lock().unwrap() = match res {
                Ok(()) => UnState::Done,
                Err(e) => UnState::Failed(e),
            };
            ctx.request_repaint();
        });
    }
}

impl eframe::App for Uninstall {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        let panel = egui::Frame::none().fill(ui::BG).inner_margin(egui::Margin::symmetric(22.0, 20.0));
        let l = self.lang.clone();
        let close = l.get("btn_close");

        let Some(log) = &self.log else {
            let nav = ui::footer(ctx, None, &close, true, None);
            ui::header(ctx, &self.brand, &l.fmt("un_title", &[("app", "")]), &l.get("un_missing_sub"));
            egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
                ui.label(ui::bold(l.get("un_missing_title"), 15.0).color(ui::ERROR));
                ui.add_space(6.0);
                ui.label(l.get("un_missing_body"));
                ui.label(ui::muted(self.dir.display().to_string()));
                ui.add_space(6.0);
                ui.label(l.get("un_missing_hint"));
            });
            if nav == Nav::Next {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        };
        let app = log.app_name.clone();
        let a = [("app", app.as_str())];
        let state = self.state.lock().unwrap().clone();

        match state {
            UnState::Confirm => {
                let nav = ui::footer(ctx, None, &l.get("btn_uninstall"), true, Some((true, &l.get("btn_cancel"))));
                ui::header(ctx, &self.brand, &l.fmt("un_title", &a), &l.get("un_sub"));
                egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
                    ui.label(l.fmt("un_confirm", &a));
                    ui.add_space(10.0);
                    ui::inset_frame().show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(ui::bold(l.get("un_location"), 13.5));
                        ui.label(ui::muted(&log.dir));
                        let counts = l.fmt(
                            "un_counts",
                            &[("files", &log.files.len().to_string()), ("shortcuts", &log.shortcuts.len().to_string())],
                        );
                        ui.label(ui::muted(counts).size(12.5));
                    });
                });
                match nav {
                    Nav::Next => self.start(ctx),
                    Nav::Cancel => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    _ => {}
                }
            }
            UnState::Working => {
                ui::footer(ctx, None, &close, false, None);
                ui::header(ctx, &self.brand, &l.fmt("un_title", &a), &l.get("un_sub"));
                egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
                    ui.label(ui::bold(l.fmt("un_working", &a), 14.0));
                    ui.add_space(6.0);
                    ui.add(egui::ProgressBar::new(0.0).fill(ui::accent()).animate(true).desired_width(ui.available_width()));
                });
                if ctx.input(|i| i.viewport().close_requested()) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                }
            }
            UnState::Done => {
                let nav = ui::footer(ctx, None, &close, true, None);
                ui::header(ctx, &self.brand, &l.get("un_done_title"), &l.fmt("un_done_sub", &a));
                egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
                    ui.label(ui::bold(l.fmt("un_done_body", &a), 15.0).color(ui::SUCCESS));
                    ui.add_space(6.0);
                    ui.label(ui::muted(l.get("un_leftovers")));
                });
                if nav == Nav::Next {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            UnState::Failed(e) => {
                let retry = l.get("btn_retry");
                let nav = ui::footer(ctx, Some((true, &retry)), &close, true, None);
                ui::header(ctx, &self.brand, &l.fmt("un_title", &a), &l.get("un_sub"));
                egui::CentralPanel::default().frame(panel).show(ctx, |ui| {
                    ui.label(ui::bold(l.get("un_failed"), 15.0).color(ui::ERROR));
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(e).wrap());
                });
                match nav {
                    Nav::Back => self.start(ctx),
                    Nav::Next => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    _ => {}
                }
            }
        }
    }
}
