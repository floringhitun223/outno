//! OS specific helpers. Real implementations on Windows, harmless stubs elsewhere
//! (so you can still `cargo check` on Linux/macOS).

use crate::payload::Manifest;
use std::path::{Path, PathBuf};

/// Per-user install root (no admin rights needed): %LOCALAPPDATA%\Programs
pub fn default_install_root() -> PathBuf {
    dirs::data_local_dir()
        .map(|d| d.join("Programs"))
        .unwrap_or_else(|| PathBuf::from("C:\\Program Files"))
}

pub fn desktop_dir() -> Option<PathBuf> {
    dirs::desktop_dir()
}

pub fn start_menu_dir(app: &str) -> Option<PathBuf> {
    dirs::data_dir().map(|d| {
        d.join("Microsoft").join("Windows").join("Start Menu").join("Programs").join(app)
    })
}

pub fn registry_key_name(m: &Manifest) -> String {
    m.app_name.replace(['\\', '/'], "_")
}

#[cfg(windows)]
pub fn create_shortcut(target: &Path, lnk: &Path, work_dir: &Path) -> Result<(), String> {
    let mut sl = mslnk::ShellLink::new(target).map_err(|e| e.to_string())?;
    sl.set_working_dir(Some(work_dir.display().to_string()));
    if let Some(p) = lnk.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    sl.create_lnk(lnk).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
pub fn create_shortcut(_t: &Path, _l: &Path, _w: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub fn register_uninstall(m: &Manifest, dir: &Path, uninstaller: &Path) -> Result<(), String> {
    use winreg::{enums::*, RegKey};
    let path = format!(
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{}",
        registry_key_name(m)
    );
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey(path)
        .map_err(|e| e.to_string())?;
    let e = |r: std::io::Result<()>| r.map_err(|e| e.to_string());
    e(key.set_value("DisplayName", &m.app_name))?;
    e(key.set_value("DisplayVersion", &m.version))?;
    e(key.set_value("Publisher", &m.publisher))?;
    e(key.set_value("URLInfoAbout", &m.url))?;
    e(key.set_value("InstallLocation", &dir.display().to_string()))?;
    e(key.set_value(
        "DisplayIcon",
        &dir.join(&m.main_exe).display().to_string(),
    ))?;
    e(key.set_value(
        "UninstallString",
        &format!("\"{}\" --uninstall \"{}\"", uninstaller.display(), dir.display()),
    ))?;
    e(key.set_value("NoModify", &1u32))?;
    e(key.set_value("NoRepair", &1u32))?;
    Ok(())
}

#[cfg(not(windows))]
pub fn register_uninstall(_m: &Manifest, _d: &Path, _u: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub fn unregister_uninstall(key_name: &str) {
    use winreg::{enums::*, RegKey};
    let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(format!(
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{}",
        key_name
    ));
}

#[cfg(not(windows))]
pub fn unregister_uninstall(_k: &str) {}

/// A running exe can't delete itself on Windows: hand it to a detached cmd.exe.
#[cfg(windows)]
pub fn schedule_self_delete(exe: &Path, dir: &Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("cmd")
        .raw_arg(format!(
            "/C ping 127.0.0.1 -n 3 > nul & del /f /q \"{}\" & rmdir \"{}\"",
            exe.display(),
            dir.display()
        ))
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
}

#[cfg(not(windows))]
pub fn schedule_self_delete(_e: &Path, _d: &Path) {}

pub fn open_folder(path: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}
