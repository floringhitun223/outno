//! OS specific helpers. Real implementations on Windows, harmless stubs elsewhere
//! (so you can still `cargo check` and try the UI on Linux/macOS).

use crate::payload::Manifest;
use std::io;
use std::path::{Path, PathBuf};

/// Install root: %LOCALAPPDATA%\Programs (per user) or Program Files (all users).
pub fn install_root(all_users: bool) -> PathBuf {
    if all_users {
        std::env::var_os("ProgramW6432")
            .or_else(|| std::env::var_os("ProgramFiles"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:\\Program Files"))
    } else {
        dirs::data_local_dir()
            .map(|d| d.join("Programs"))
            .unwrap_or_else(|| PathBuf::from("C:\\Program Files"))
    }
}

pub fn desktop_dir(all_users: bool) -> Option<PathBuf> {
    if all_users {
        std::env::var_os("PUBLIC").map(|p| PathBuf::from(p).join("Desktop"))
    } else {
        dirs::desktop_dir()
    }
}

pub fn start_menu_dir(app: &str, all_users: bool) -> Option<PathBuf> {
    let base = if all_users {
        std::env::var_os("ProgramData").map(PathBuf::from)
    } else {
        dirs::data_dir()
    };
    base.map(|d| d.join("Microsoft").join("Windows").join("Start Menu").join("Programs").join(app))
}

pub fn registry_key_name(m: &Manifest) -> String {
    m.app_name.replace(['\\', '/'], "_")
}

/// A previous installation found in the registry.
pub struct Previous {
    pub dir: PathBuf,
    pub version: String,
    pub all_users: bool,
}

/// Can we create files in `dir` (or, if it doesn't exist yet, in its nearest existing parent)?
pub fn can_write(dir: &Path) -> bool {
    let mut probe = dir.to_path_buf();
    while !probe.exists() {
        match probe.parent() {
            Some(p) => probe = p.to_path_buf(),
            None => return false,
        }
    }
    if !probe.is_dir() {
        return false;
    }
    let test = probe.join(format!(".outno-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&test) {
        Ok(_) => {
            let _ = std::fs::remove_file(&test);
            true
        }
        Err(_) => false,
    }
}

/// Windows error codes for "file is locked by another process".
pub fn is_in_use_error(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(32) | Some(33))
}

/// True if `path` exists and another process holds it open (e.g. the app is running).
pub fn file_in_use(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(_) => false,
        Err(e) => is_in_use_error(&e) || (e.kind() == io::ErrorKind::PermissionDenied && !is_readonly(path)),
    }
}

fn is_readonly(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.permissions().readonly()).unwrap_or(false)
}

/// Clear the read-only attribute so an existing file can be overwritten or deleted.
pub fn make_writable(path: &Path) {
    if let Ok(m) = std::fs::metadata(path) {
        let mut p = m.permissions();
        if p.readonly() {
            #[allow(clippy::permissions_set_readonly_false)]
            p.set_readonly(false);
            let _ = std::fs::set_permissions(path, p);
        }
    }
}

#[cfg(windows)]
pub fn create_shortcut(target: &Path, lnk: &Path, work_dir: &Path) -> Result<(), String> {
    let mut sl = mslnk::ShellLink::new(target).map_err(|e| e.to_string())?;
    sl.set_working_dir(Some(work_dir.display().to_string()));
    if let Some(p) = lnk.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    if lnk.exists() {
        make_writable(lnk);
        let _ = std::fs::remove_file(lnk);
    }
    sl.create_lnk(lnk).map_err(|e| e.to_string())
}

#[cfg(not(windows))]
pub fn create_shortcut(_t: &Path, _l: &Path, _w: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

#[cfg(windows)]
fn hive(all_users: bool) -> winreg::RegKey {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    winreg::RegKey::predef(if all_users { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER })
}

#[cfg(windows)]
pub fn register_uninstall(m: &Manifest, dir: &Path, uninstaller: &Path, all_users: bool, size_kb: u32) -> Result<(), String> {
    use winreg::enums::{KEY_READ, KEY_WRITE};
    let path = format!(r"{UNINSTALL_KEY}\{}", registry_key_name(m));
    let where_ = format!(r"{}\{path}", if all_users { "HKLM" } else { "HKCU" });
    // Read/write only: create_subkey() asks for KEY_ALL_ACCESS (incl. WRITE_DAC/WRITE_OWNER),
    // which is denied on an existing key created by an elevated process.
    let (key, _) = hive(all_users)
        .create_subkey_with_flags(&path, KEY_READ | KEY_WRITE)
        .map_err(|e| format!("{where_}: {e}"))?;
    let e = |r: std::io::Result<()>| r.map_err(|e| format!("{where_}: {e}"));
    e(key.set_value("DisplayName", &m.app_name))?;
    e(key.set_value("DisplayVersion", &m.version))?;
    e(key.set_value("Publisher", &m.publisher))?;
    e(key.set_value("URLInfoAbout", &m.url))?;
    e(key.set_value("InstallLocation", &dir.display().to_string()))?;
    e(key.set_value("DisplayIcon", &dir.join(&m.main_exe).display().to_string()))?;
    e(key.set_value(
        "UninstallString",
        &format!("\"{}\" --uninstall \"{}\"", uninstaller.display(), dir.display()),
    ))?;
    e(key.set_value("EstimatedSize", &size_kb))?;
    e(key.set_value("NoModify", &1u32))?;
    e(key.set_value("NoRepair", &1u32))?;
    Ok(())
}

#[cfg(not(windows))]
pub fn register_uninstall(_m: &Manifest, _d: &Path, _u: &Path, _all: bool, _kb: u32) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
pub fn unregister_uninstall(key_name: &str, all_users: bool) {
    let _ = hive(all_users).delete_subkey_all(format!(r"{UNINSTALL_KEY}\{key_name}"));
}

#[cfg(not(windows))]
pub fn unregister_uninstall(_k: &str, _all: bool) {}

/// Look for an earlier install of this app (current user first, then all users).
#[cfg(windows)]
pub fn previous_install(m: &Manifest) -> Option<Previous> {
    for all_users in [false, true] {
        let Ok(key) = hive(all_users).open_subkey(format!(r"{UNINSTALL_KEY}\{}", registry_key_name(m))) else {
            continue;
        };
        let dir: String = key.get_value("InstallLocation").unwrap_or_default();
        if dir.trim().is_empty() || !Path::new(&dir).is_dir() {
            continue;
        }
        let version: String = key.get_value("DisplayVersion").unwrap_or_default();
        return Some(Previous { dir: PathBuf::from(dir), version, all_users });
    }
    None
}

/// Elsewhere there is no registry: detect an earlier install by its uninstall data.
#[cfg(not(windows))]
pub fn previous_install(m: &Manifest) -> Option<Previous> {
    let dir = install_root(false).join(&m.install_folder);
    dir.join("unins000.dat").is_file().then(|| Previous { dir, version: String::new(), all_users: false })
}

/// Delete `exe` (and optionally the folder it lived in) once this process has exited.
/// A running exe can't delete itself on Windows, so a detached cmd.exe retries for ~2 min.
#[cfg(windows)]
pub fn schedule_self_delete(exe: &Path, dir: Option<&Path>) {
    use std::os::windows::process::CommandExt;
    let e = exe.display();
    let rmdir = dir.map(|d| format!("rmdir \"{}\" >nul 2>&1 & ", d.display())).unwrap_or_default();
    let cmd = format!(
        "/C for /L %i in (1,1,120) do @(ping -n 2 127.0.0.1 >nul & del /f /q \"{e}\" >nul 2>&1 & if not exist \"{e}\" ({rmdir}exit))"
    );
    let _ = std::process::Command::new("cmd")
        .raw_arg(cmd)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn();
}

#[cfg(not(windows))]
pub fn schedule_self_delete(_e: &Path, _d: Option<&Path>) {}

pub fn open_folder(path: &Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Open Explorer with the file selected (falls back to opening its folder).
pub fn reveal_file(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    if let Some(parent) = path.parent() {
        open_folder(parent);
    }
}

/// GUI-subsystem exes have no console; attach to the parent's so CLI output is visible.
#[cfg(windows)]
pub fn attach_parent_console() {
    extern "system" {
        fn AttachConsole(pid: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
pub fn attach_parent_console() {}

/// Quote an argument for a Windows command line (CommandLineToArgvW rules).
pub fn quote_arg(s: &str) -> String {
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in s.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}
