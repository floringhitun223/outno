//! Embeds the OUTNO icon, version info and application manifest into the Windows exe.
//! Generated setups and uninstallers are copies of this exe, so they carry them too.

fn main() {
    println!("cargo:rerun-if-changed=assets/outno.ico");
    println!("cargo:rerun-if-changed=assets/outno.manifest");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/outno.ico")
        .set_manifest_file("assets/outno.manifest")
        .set("ProductName", "OUTNO")
        .set("FileDescription", "OUTNO Setup")
        .set("InternalName", "outno")
        .set("OriginalFilename", "outno.exe");
    if let Err(e) = res.compile() {
        panic!("failed to embed Windows resources (icon / manifest): {e}");
    }
}
