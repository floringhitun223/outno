//! Layout of a generated setup.exe:
//!
//! [ stub exe ][ zip archive ][ manifest json ][ zip_len u64 ][ manifest_len u64 ][ MAGIC ]
//!
//! A code-signed setup additionally has `[ padding ][ certificate table ]` after MAGIC;
//! the PE header says where that table starts, so the trailer is found in front of it.

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use zip::ZipArchive;

pub const MAGIC: &[u8; 8] = b"RSINST01";
pub const TRAILER_LEN: u64 = 24;

/// Archive entries under this prefix are setup resources (wizard images, signed
/// uninstaller), never extracted into the install folder.
pub const ASSET_PREFIX: &str = "$outno/";
pub const ASSET_WIZARD_IMAGE: &str = "$outno/wizard.png";
pub const ASSET_HEADER_IMAGE: &str = "$outno/header.png";
pub const ASSET_UNINSTALLER: &str = "$outno/unins000.exe";

#[derive(Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum InstallMode {
    /// %LOCALAPPDATA%\Programs, HKCU — no admin rights.
    #[default]
    PerUser,
    /// Program Files, HKLM, shortcuts for everyone — asks for admin rights.
    AllUsers,
    /// The user picks on the destination page.
    Ask,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Manifest {
    pub app_name: String,
    pub version: String,
    pub publisher: String,
    pub url: String,
    /// Main executable, relative to the install dir (used for shortcuts / launch).
    pub main_exe: String,
    /// Default folder name under the install root.
    pub install_folder: String,
    pub license_text: String,
    pub desktop_icon: bool,
    pub start_menu: bool,
    pub launch_after: bool,
    pub install_mode: InstallMode,
    /// Languages offered by the wizard (first = default). Empty = English.
    pub languages: Vec<crate::lang::Pack>,
    /// Accent colour for buttons, links and the banner gradient.
    pub accent: Option<[u8; 3]>,
}

#[derive(Clone)]
pub struct Payload {
    pub manifest: Manifest,
    pub exe_path: PathBuf,
    /// Offset where the zip starts == length of the stub exe.
    pub zip_start: u64,
    pub zip_len: u64,
}

/// Read+Seek view over a byte range of a file (so we never load the whole zip in RAM).
pub struct SliceReader {
    file: File,
    start: u64,
    len: u64,
    pos: u64,
}

impl Read for SliceReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.len.saturating_sub(self.pos);
        if remaining == 0 {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(remaining) as usize;
        self.file.seek(SeekFrom::Start(self.start + self.pos))?;
        let r = self.file.read(&mut buf[..n])?;
        self.pos += r as u64;
        Ok(r)
    }
}

impl Seek for SliceReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let new = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(n) => self.len as i64 + n,
            SeekFrom::Current(n) => self.pos as i64 + n,
        };
        if new < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

/// Location of the Authenticode certificate table, read from the PE header.
struct PeSignature {
    /// File offset of the IMAGE_DIRECTORY_ENTRY_SECURITY entry (8 bytes) in the header.
    dir_entry: u64,
    /// File offset of the certificate table (0 = not signed).
    offset: u64,
    size: u64,
}

fn read_u16(f: &mut File, at: u64) -> io::Result<u16> {
    let mut b = [0u8; 2];
    f.seek(SeekFrom::Start(at))?;
    f.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32(f: &mut File, at: u64) -> io::Result<u32> {
    let mut b = [0u8; 4];
    f.seek(SeekFrom::Start(at))?;
    f.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

/// None if the file isn't a PE image (e.g. when developing on Linux).
fn pe_signature(f: &mut File) -> Option<PeSignature> {
    if read_u16(f, 0).ok()? != 0x5A4D {
        return None; // "MZ"
    }
    let pe = read_u32(f, 0x3C).ok()? as u64;
    if read_u32(f, pe).ok()? != 0x0000_4550 {
        return None; // "PE\0\0"
    }
    let opt = pe + 24;
    let (rva_count_at, dirs_at) = match read_u16(f, opt).ok()? {
        0x10B => (opt + 92, opt + 96),   // PE32
        0x20B => (opt + 108, opt + 112), // PE32+
        _ => return None,
    };
    if read_u32(f, rva_count_at).ok()? <= 4 {
        return None;
    }
    let dir_entry = dirs_at + 4 * 8; // IMAGE_DIRECTORY_ENTRY_SECURITY
    let offset = read_u32(f, dir_entry).ok()? as u64;
    let size = read_u32(f, dir_entry + 4).ok()? as u64;
    Some(PeSignature { dir_entry, offset, size })
}

/// End of the unsigned part of the file. Code signing appends a certificate table
/// after our trailer, so the payload must be looked for in front of it.
fn unsigned_end(f: &mut File, total: u64) -> u64 {
    match pe_signature(f) {
        Some(sig) if sig.offset > 0 && sig.offset <= total && sig.offset + sig.size <= total => sig.offset,
        _ => total,
    }
}

pub fn read_payload(exe: &Path) -> Option<Payload> {
    let mut f = File::open(exe).ok()?;
    let total = f.metadata().ok()?.len();
    let end = unsigned_end(&mut f, total);

    // Signing tools zero-pad the file to 8 bytes before the certificate table, so in a
    // signed file the trailer may end up to 7 bytes before `end`. Unsigned: exact end.
    let max_pad = if end < total { 7 } else { 0 };
    let mut t = [0u8; 32];
    let trailer_end = (0..=max_pad).map(|pad| end.saturating_sub(pad)).find(|&e| {
        let pad = (end - e) as usize;
        e >= TRAILER_LEN
            && f.seek(SeekFrom::Start(e - TRAILER_LEN)).is_ok()
            && f.read_exact(&mut t[..24 + pad]).is_ok()
            && &t[16..24] == MAGIC
            && t[24..24 + pad].iter().all(|&b| b == 0)
    })?;

    let zip_len = u64::from_le_bytes(t[0..8].try_into().ok()?);
    let man_len = u64::from_le_bytes(t[8..16].try_into().ok()?);
    let man_start = trailer_end.checked_sub(TRAILER_LEN)?.checked_sub(man_len)?;
    let zip_start = man_start.checked_sub(zip_len)?;

    f.seek(SeekFrom::Start(man_start)).ok()?;
    let mut buf = vec![0u8; man_len as usize];
    f.read_exact(&mut buf).ok()?;
    let manifest: Manifest = serde_json::from_slice(&buf).ok()?;

    Some(Payload { manifest, exe_path: exe.to_path_buf(), zip_start, zip_len })
}

/// Write the runtime stub of `exe`: everything before the payload / code signature,
/// with the PE security-directory entry cleared. Used both for new setups and for the
/// uninstaller, so neither inherits a signature entry that no longer matches the file
/// (they can then be signed on their own). Returns the number of bytes written.
pub fn write_stub(exe: &Path, out: &mut impl Write) -> io::Result<u64> {
    let mut f = File::open(exe)?;
    let total = f.metadata()?.len();
    let sig = pe_signature(&mut f);
    let len = match read_payload(exe) {
        Some(p) => p.zip_start,
        None => sig
            .as_ref()
            .filter(|s| s.offset > 0 && s.offset + s.size <= total)
            .map_or(total, |s| s.offset),
    };

    let mut stub = vec![0u8; len as usize];
    f.seek(SeekFrom::Start(0))?;
    f.read_exact(&mut stub)?;
    if let Some(s) = sig {
        let at = s.dir_entry as usize;
        if at + 8 <= stub.len() {
            stub[at..at + 8].fill(0);
        }
    }
    out.write_all(&stub)?;
    Ok(len)
}

pub fn open_zip(p: &Payload) -> Result<ZipArchive<SliceReader>, Box<dyn std::error::Error>> {
    let file = File::open(&p.exe_path)?;
    let reader = SliceReader { file, start: p.zip_start, len: p.zip_len, pos: 0 };
    Ok(ZipArchive::new(reader)?)
}

/// Read a whole archive entry (used for the small `$outno/` assets).
pub fn read_entry(p: &Payload, name: &str) -> Option<Vec<u8>> {
    let mut z = open_zip(p).ok()?;
    let mut e = z.by_name(name).ok()?;
    let mut buf = Vec::with_capacity(e.size() as usize);
    e.read_to_end(&mut buf).ok()?;
    Some(buf)
}
