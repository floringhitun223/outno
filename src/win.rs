//! Small Win32 helpers declared by hand (no extra crates, keeps setups small):
//! elevation, system UI language and replacing the icon of a stub exe.
//! Every function has a harmless fallback on other platforms.

use std::path::Path;

/// Result of trying to run something as administrator.
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Elevated {
    /// The process ran; this is its exit code.
    Exited(u32),
    /// The user clicked "No" on the UAC prompt.
    Cancelled,
    Failed(String),
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Is the current process elevated (or the user an admin with UAC off)?
pub fn is_admin() -> bool {
    #[cfg(windows)]
    unsafe {
        ffi::IsUserAnAdmin() != 0
    }
    #[cfg(not(windows))]
    false
}

/// Run `exe` with `params` through the UAC prompt and wait for it to finish.
pub fn run_elevated_and_wait(exe: &Path, params: &str) -> Elevated {
    #[cfg(windows)]
    unsafe {
        use ffi::*;
        let verb = wide("runas");
        let file = wide(&exe.display().to_string());
        let par = wide(params);
        let mut info: ShellExecuteInfoW = std::mem::zeroed();
        info.cb_size = std::mem::size_of::<ShellExecuteInfoW>() as u32;
        info.f_mask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        info.lp_verb = verb.as_ptr();
        info.lp_file = file.as_ptr();
        info.lp_parameters = par.as_ptr();
        info.n_show = SW_SHOWNORMAL;
        if ShellExecuteExW(&mut info) == 0 {
            let err = GetLastError();
            return if err == ERROR_CANCELLED {
                Elevated::Cancelled
            } else {
                Elevated::Failed(std::io::Error::from_raw_os_error(err as i32).to_string())
            };
        }
        if info.h_process == 0 {
            return Elevated::Failed("no process handle".into());
        }
        WaitForSingleObject(info.h_process, INFINITE);
        let mut code = 1u32;
        GetExitCodeProcess(info.h_process, &mut code);
        CloseHandle(info.h_process);
        Elevated::Exited(code)
    }
    #[cfg(not(windows))]
    {
        let _ = (exe, params, wide(""));
        Elevated::Failed("elevation is only available on Windows".into())
    }
}

/// System UI language as a short code ("en", "ro", "de" ...).
pub fn ui_language() -> String {
    #[cfg(windows)]
    {
        let id = unsafe { ffi::GetUserDefaultUILanguage() } & 0x3FF;
        match id {
            0x09 => "en",
            0x18 => "ro",
            0x0C => "fr",
            0x07 => "de",
            0x0A => "es",
            0x10 => "it",
            0x16 => "pt",
            0x13 => "nl",
            0x15 => "pl",
            0x19 => "ru",
            0x1F => "tr",
            0x0E => "hu",
            0x05 => "cs",
            0x22 => "uk",
            0x11 => "ja",
            0x04 => "zh",
            0x12 => "ko",
            _ => "en",
        }
        .into()
    }
    #[cfg(not(windows))]
    {
        std::env::var("LANG").ok().and_then(|l| l.get(..2).map(str::to_string)).unwrap_or_else(|| "en".into())
    }
}

// ───────────────────────────── Icons ─────────────────────────────

/// An icon as stored in resources: one RT_GROUP_ICON directory + its RT_ICON images.
pub struct IconSet {
    /// GRPICONDIRENTRY without the trailing id: width, height, colours, reserved,
    /// planes (u16), bit count (u16), bytes in resource (u32).
    entries: Vec<[u8; 12]>,
    images: Vec<Vec<u8>>,
}

impl IconSet {
    /// Parse a `.ico` file.
    pub fn from_ico(bytes: &[u8]) -> Option<Self> {
        let rd16 = |o: usize| bytes.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let rd32 = |o: usize| bytes.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if rd16(0)? != 0 || rd16(2)? != 1 {
            return None;
        }
        let count = rd16(4)? as usize;
        if count == 0 {
            return None;
        }
        let mut set = IconSet { entries: Vec::new(), images: Vec::new() };
        for i in 0..count {
            let e = 6 + i * 16;
            let entry: [u8; 12] = bytes.get(e..e + 12)?.try_into().ok()?;
            let size = rd32(e + 8)? as usize;
            let offset = rd32(e + 12)? as usize;
            set.images.push(bytes.get(offset..offset.checked_add(size)?)?.to_vec());
            set.entries.push(entry);
        }
        Some(set)
    }

    /// RT_GROUP_ICON data with icon ids `first_id..`.
    fn group_data(&self, first_id: u16) -> Vec<u8> {
        let mut d = Vec::with_capacity(6 + self.entries.len() * 14);
        d.extend_from_slice(&0u16.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        for (i, e) in self.entries.iter().enumerate() {
            d.extend_from_slice(e);
            d.extend_from_slice(&(first_id + i as u16).to_le_bytes());
        }
        d
    }
}

/// Read the first (main) icon of an exe or dll.
pub fn extract_icon(exe: &Path) -> Option<IconSet> {
    #[cfg(windows)]
    unsafe {
        use ffi::*;
        let module = LoadLibraryExW(wide(&exe.display().to_string()).as_ptr(), 0, LOAD_AS_RESOURCES);
        if module == 0 {
            return None;
        }
        let result = (|| {
            let groups = enum_names(module, RT_GROUP_ICON);
            let group = load(module, groups.first()?.ptr(), RT_GROUP_ICON)?;
            let rd16 = |o: usize| group.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
            let count = rd16(4)? as usize;
            let mut set = IconSet { entries: Vec::new(), images: Vec::new() };
            for i in 0..count {
                let e = 6 + i * 14;
                let entry: [u8; 12] = group.get(e..e + 12)?.try_into().ok()?;
                let id = rd16(e + 12)?;
                let image = load(module, id as usize as *const u16, RT_ICON)?;
                set.entries.push(entry);
                set.images.push(image);
            }
            (!set.images.is_empty()).then_some(set)
        })();
        FreeLibrary(module);
        result
    }
    #[cfg(not(windows))]
    {
        let _ = exe;
        None
    }
}

/// Replace every icon in `exe` (a stub without payload) with `icon`.
pub fn replace_icon(exe: &Path, icon: &IconSet) -> Result<(), String> {
    #[cfg(windows)]
    unsafe {
        use ffi::*;
        let path = wide(&exe.display().to_string());

        // 1. List the existing icon resources (name + language) so we can delete them.
        let module = LoadLibraryExW(path.as_ptr(), 0, LOAD_AS_RESOURCES);
        if module == 0 {
            return Err(format!("can't open {} ({})", exe.display(), std::io::Error::last_os_error()));
        }
        let mut existing: Vec<(usize, ResName, u16)> = Vec::new();
        for ty in [RT_GROUP_ICON, RT_ICON] {
            for name in enum_names(module, ty) {
                for lang in enum_langs(module, ty, name.ptr()) {
                    existing.push((ty, name.clone(), lang));
                }
            }
        }
        FreeLibrary(module);
        let lang = existing.iter().find(|(t, _, _)| *t == RT_GROUP_ICON).map_or(0, |(_, _, l)| *l);
        let group_name = existing
            .iter()
            .find(|(t, _, _)| *t == RT_GROUP_ICON)
            .map(|(_, n, _)| n.clone())
            .unwrap_or(ResName::Id(1));

        // 2. Delete them and add the new icon under the original group name.
        let h = BeginUpdateResourceW(path.as_ptr(), 0);
        if h == 0 {
            return Err(format!("can't update resources ({})", std::io::Error::last_os_error()));
        }
        let mut ok = true;
        for (ty, name, l) in &existing {
            ok &= UpdateResourceW(h, *ty as *const u16, name.ptr(), *l, std::ptr::null(), 0) != 0;
        }
        const FIRST_ID: u16 = 1;
        for (i, img) in icon.images.iter().enumerate() {
            let id = (FIRST_ID + i as u16) as usize as *const u16;
            ok &= UpdateResourceW(h, RT_ICON as *const u16, id, lang, img.as_ptr().cast(), img.len() as u32) != 0;
        }
        let group = icon.group_data(FIRST_ID);
        ok &= UpdateResourceW(h, RT_GROUP_ICON as *const u16, group_name.ptr(), lang, group.as_ptr().cast(), group.len() as u32)
            != 0;
        if !ok {
            let e = std::io::Error::last_os_error();
            EndUpdateResourceW(h, 1);
            return Err(format!("can't write icon resources ({e})"));
        }
        if EndUpdateResourceW(h, 0) == 0 {
            return Err(format!("can't save resources ({})", std::io::Error::last_os_error()));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (exe, icon.group_data(1));
        Err("changing the setup icon is only available on Windows".into())
    }
}

#[cfg(windows)]
#[derive(Clone)]
enum ResName {
    Id(u16),
    /// NUL-terminated UTF-16 name.
    Str(Vec<u16>),
}

#[cfg(windows)]
impl ResName {
    fn ptr(&self) -> *const u16 {
        match self {
            ResName::Id(id) => *id as usize as *const u16,
            ResName::Str(s) => s.as_ptr(),
        }
    }
}

#[cfg(windows)]
unsafe fn enum_names(module: isize, ty: usize) -> Vec<ResName> {
    unsafe extern "system" fn cb(_m: isize, _t: *const u16, name: *const u16, param: isize) -> i32 {
        let out = &mut *(param as *mut Vec<ResName>);
        if (name as usize) >> 16 == 0 {
            out.push(ResName::Id(name as usize as u16));
        } else {
            let mut len = 0;
            while *name.add(len) != 0 {
                len += 1;
            }
            out.push(ResName::Str(std::slice::from_raw_parts(name, len + 1).to_vec()));
        }
        1
    }
    let mut out: Vec<ResName> = Vec::new();
    ffi::EnumResourceNamesW(module, ty as *const u16, cb, &mut out as *mut _ as isize);
    out
}

#[cfg(windows)]
unsafe fn enum_langs(module: isize, ty: usize, name: *const u16) -> Vec<u16> {
    unsafe extern "system" fn cb(_m: isize, _t: *const u16, _n: *const u16, lang: u16, param: isize) -> i32 {
        (*(param as *mut Vec<u16>)).push(lang);
        1
    }
    let mut out: Vec<u16> = Vec::new();
    ffi::EnumResourceLanguagesW(module, ty as *const u16, name, cb, &mut out as *mut _ as isize);
    out
}

#[cfg(windows)]
unsafe fn load(module: isize, name: *const u16, ty: usize) -> Option<Vec<u8>> {
    use ffi::*;
    let res = FindResourceW(module, name, ty as *const u16);
    if res == 0 {
        return None;
    }
    let size = SizeofResource(module, res) as usize;
    let data = LoadResource(module, res);
    if data == 0 || size == 0 {
        return None;
    }
    let p = LockResource(data);
    if p.is_null() {
        return None;
    }
    Some(std::slice::from_raw_parts(p, size).to_vec())
}

#[cfg(windows)]
#[allow(non_snake_case, clippy::upper_case_acronyms)]
mod ffi {
    use std::ffi::c_void;

    pub const RT_ICON: usize = 3;
    pub const RT_GROUP_ICON: usize = 14;
    /// LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE
    pub const LOAD_AS_RESOURCES: u32 = 0x2 | 0x20;
    pub const SEE_MASK_NOCLOSEPROCESS: u32 = 0x40;
    pub const SEE_MASK_NOASYNC: u32 = 0x100;
    pub const SW_SHOWNORMAL: i32 = 1;
    pub const INFINITE: u32 = 0xFFFF_FFFF;
    pub const ERROR_CANCELLED: u32 = 1223;

    #[repr(C)]
    pub struct ShellExecuteInfoW {
        pub cb_size: u32,
        pub f_mask: u32,
        pub hwnd: isize,
        pub lp_verb: *const u16,
        pub lp_file: *const u16,
        pub lp_parameters: *const u16,
        pub lp_directory: *const u16,
        pub n_show: i32,
        pub h_inst_app: isize,
        pub lp_id_list: *mut c_void,
        pub lp_class: *const u16,
        pub hkey_class: isize,
        pub dw_hot_key: u32,
        pub h_icon_or_monitor: isize,
        pub h_process: isize,
    }

    pub type EnumNameProc = unsafe extern "system" fn(isize, *const u16, *const u16, isize) -> i32;
    pub type EnumLangProc = unsafe extern "system" fn(isize, *const u16, *const u16, u16, isize) -> i32;

    #[link(name = "shell32")]
    extern "system" {
        pub fn IsUserAnAdmin() -> i32;
        pub fn ShellExecuteExW(info: *mut ShellExecuteInfoW) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetLastError() -> u32;
        pub fn WaitForSingleObject(h: isize, ms: u32) -> u32;
        pub fn GetExitCodeProcess(h: isize, code: *mut u32) -> i32;
        pub fn CloseHandle(h: isize) -> i32;
        pub fn GetUserDefaultUILanguage() -> u16;
        pub fn LoadLibraryExW(file: *const u16, reserved: isize, flags: u32) -> isize;
        pub fn FreeLibrary(module: isize) -> i32;
        pub fn FindResourceW(module: isize, name: *const u16, ty: *const u16) -> isize;
        pub fn SizeofResource(module: isize, res: isize) -> u32;
        pub fn LoadResource(module: isize, res: isize) -> isize;
        pub fn LockResource(data: isize) -> *const u8;
        pub fn EnumResourceNamesW(module: isize, ty: *const u16, cb: EnumNameProc, param: isize) -> i32;
        pub fn EnumResourceLanguagesW(module: isize, ty: *const u16, name: *const u16, cb: EnumLangProc, param: isize) -> i32;
        pub fn BeginUpdateResourceW(file: *const u16, delete_existing: i32) -> isize;
        pub fn UpdateResourceW(h: isize, ty: *const u16, name: *const u16, lang: u16, data: *const c_void, size: u32) -> i32;
        pub fn EndUpdateResourceW(h: isize, discard: i32) -> i32;
    }

    // ShellExecuteInfoW must match the 64-bit SDK layout (112 bytes).
    #[cfg(target_pointer_width = "64")]
    const _: () = assert!(std::mem::size_of::<ShellExecuteInfoW>() == 112);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_outno_ico() {
        let ico = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/outno.ico")).unwrap();
        let set = IconSet::from_ico(&ico).unwrap();
        assert_eq!(set.images.len(), 9);
        let g = set.group_data(1);
        assert_eq!(g.len(), 6 + 9 * 14);
        assert_eq!(u16::from_le_bytes([g[4], g[5]]), 9);
        // every image size in the group matches the image data
        for (i, img) in set.images.iter().enumerate() {
            let e = 6 + i * 14;
            assert_eq!(u32::from_le_bytes(g[e + 8..e + 12].try_into().unwrap()) as usize, img.len());
        }
    }
}
