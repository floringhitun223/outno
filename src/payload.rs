//! Layout of a generated setup.exe:
//!
//! [ stub exe ][ zip archive ][ manifest json ][ zip_len u64 ][ manifest_len u64 ][ MAGIC ]

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use zip::ZipArchive;

pub const MAGIC: &[u8; 8] = b"RSINST01";
pub const TRAILER_LEN: u64 = 24;

#[derive(Serialize, Deserialize, Clone, Default)]
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

pub fn read_payload(exe: &Path) -> Option<Payload> {
    let mut f = File::open(exe).ok()?;
    let total = f.metadata().ok()?.len();
    if total < TRAILER_LEN {
        return None;
    }
    f.seek(SeekFrom::End(-(TRAILER_LEN as i64))).ok()?;
    let mut t = [0u8; 24];
    f.read_exact(&mut t).ok()?;
    if &t[16..24] != MAGIC {
        return None;
    }
    let zip_len = u64::from_le_bytes(t[0..8].try_into().ok()?);
    let man_len = u64::from_le_bytes(t[8..16].try_into().ok()?);
    let man_start = total.checked_sub(TRAILER_LEN)?.checked_sub(man_len)?;
    let zip_start = man_start.checked_sub(zip_len)?;

    f.seek(SeekFrom::Start(man_start)).ok()?;
    let mut buf = vec![0u8; man_len as usize];
    f.read_exact(&mut buf).ok()?;
    let manifest: Manifest = serde_json::from_slice(&buf).ok()?;

    Some(Payload { manifest, exe_path: exe.to_path_buf(), zip_start, zip_len })
}

pub fn open_zip(p: &Payload) -> Result<ZipArchive<SliceReader>, Box<dyn std::error::Error>> {
    let file = File::open(&p.exe_path)?;
    let reader = SliceReader { file, start: p.zip_start, len: p.zip_len, pos: 0 };
    Ok(ZipArchive::new(reader)?)
}
