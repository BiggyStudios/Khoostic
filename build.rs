//! Embeds the Windows app manifest in `khoostic.exe`.
//!
//! Khoostic stays a console program (so `khoostic --tui` and the subcommands use the terminal
//! they're started from, and the shell waits for them), but asks Windows not to open a console
//! window when it's started without one (from Explorer, a shortcut, the Start menu). That's the
//! "detached" console allocation policy of Windows 11 24H2+; older Windows ignore it, and
//! `main` closes the console there instead.

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <consoleAllocationPolicy xmlns="http://schemas.microsoft.com/SMI/2024/WindowsSettings">detached</consoleAllocationPolicy>
    </windowsSettings>
  </application>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc");
    if !(windows && msvc) {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let path = out.join("khoostic.exe.manifest");
    std::fs::write(&path, MANIFEST).expect("writing the app manifest");
    println!("cargo:rustc-link-arg-bin=khoostic=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bin=khoostic=/MANIFESTINPUT:{}",
        path.display()
    );
}
