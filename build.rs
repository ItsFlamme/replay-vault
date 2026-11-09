fn main() {
    // DLLs the exe imports are looked up in System32 only, never next to the exe: run from
    // Downloads, a planted dwmapi.dll or version.dll there would otherwise load into the app.
    // 0x800 is LOAD_LIBRARY_SEARCH_SYSTEM32.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
        version_info();
    }

    // Built from a work branch (not master, main or a release tag): say "Dev build" in the app.
    // Release builds (tools/release.ps1, CI) set RV_RELEASE and never get the label.
    println!("cargo:rerun-if-env-changed=RV_RELEASE");
    if std::env::var_os("RV_RELEASE").is_some() {
        return;
    }
    let git = |args: &[&str]| {
        std::process::Command::new("git").args(args).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(branch) = git(&["rev-parse", "--abbrev-ref", "HEAD"])
        && !["master", "main", "public", "HEAD"].contains(&branch.as_str())
    {
        println!("cargo:rustc-env=RV_DEV_BUILD={branch}");
    }
}

/// The exe's Properties > Details (product name, version, publisher), from Cargo.toml's version.
fn version_info() {
    let ver = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut n = ver.split(['.', '-']).map(|p| p.parse::<u16>().unwrap_or(0));
    let nums = format!("{},{},{},0", n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0));
    let rc = format!(
        r#"#include <winver.h>
VS_VERSION_INFO VERSIONINFO
FILEVERSION {nums}
PRODUCTVERSION {nums}
FILEOS VOS_NT_WINDOWS32
FILETYPE VFT_APP
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "ItsFlamme"
      VALUE "FileDescription", "Replay Vault"
      VALUE "FileVersion", "{ver}"
      VALUE "InternalName", "replay-vault"
      VALUE "LegalCopyright", "Copyright ItsFlamme. Licensed under Apache-2.0."
      VALUE "OriginalFilename", "replay-vault.exe"
      VALUE "ProductName", "Replay Vault"
      VALUE "ProductVersion", "{ver}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("version.rc");
    std::fs::write(&path, rc).unwrap();
    embed_resource::compile(&path, embed_resource::NONE).manifest_optional().unwrap();
}
