//! Build script for `pdfxi-app`.
//!
//! Embeds the viewer's Windows resources into every binary of this package:
//! the multi-size icon in `assets/pdfxi.ico`, a VERSIONINFO block whose product
//! name is `PDFXI` and whose version comes from `Cargo.toml`, and the
//! application manifest in `assets/pdfxi.manifest` (per-monitor-v2 DPI,
//! UTF-8 active code page, common-controls v6).
//!
//! No third-party build dependency is used: the resource script is compiled by
//! the toolchain's own resource compiler (`windres` for `*-windows-gnu`,
//! `rc.exe` for `*-msvc`) and the resulting COFF object is linked directly.
//! When no compiler is available the build fails with an actionable message,
//! because a viewer without icon, version resource, and DPI manifest is not a
//! shippable build.
//!
//! Set `PDFXI_RC` to the full path of a resource compiler to override the
//! lookup. Other platforms are a no-op.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Multi-size icon, `16` through `256` pixels, original artwork.
const ICON: &str = "assets/pdfxi.ico";
/// Manifest template; `{...}` tokens are substituted at build time.
const MANIFEST: &str = "assets/pdfxi.manifest";
fn main() {
    println!("cargo:rerun-if-changed={ICON}");
    println!("cargo:rerun-if-changed={MANIFEST}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PDFXI_RC");
    println!("cargo:rerun-if-env-changed=RC");

    if !matches!(env::var("CARGO_CFG_TARGET_OS").as_deref(), Ok("windows")) {
        return;
    }

    let out_dir = env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| fail("OUT_DIR is not set; cargo must run this script"));
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".to_owned());
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned());
    let (major, minor, patch) = version_parts(&version);

    let manifest = read_text(Path::new(MANIFEST))
        .replace("{assembly_version}", &format!("{major}.{minor}.{patch}.0"));
    let manifest = manifest.replace("{processor_architecture}", architecture_name(&arch));
    let manifest_path = out_dir.join("pdfxi-ui.manifest");
    write_text(&manifest_path, &manifest);

    let icon_path = absolute(Path::new(ICON));
    let rc_path = out_dir.join("pdfxi-ui.rc");
    write_text(
        &rc_path,
        &resource_script(&icon_path, &manifest_path, &version, (major, minor, patch)),
    );

    let res_path = out_dir.join("pdfxi-ui.res");
    let mut attempts = Vec::new();
    for compiler in compiler_candidates(&target_env, &arch) {
        match compile_resources(&compiler, &target_env, &arch, &rc_path, &res_path) {
            Ok(()) => {
                // GNU links inject the toolchain's own `default-manifest.o`
                // through GCC's `%:if-exists(default-manifest.o%s)` spec. An
                // empty object in an extra `-B` prefix shadows it, so the
                // embedded manifest is the only RT_MANIFEST resource.
                if target_env != "msvc" {
                    match empty_manifest_shim(&compiler, &target_env, &arch, &out_dir) {
                        Ok(shim_dir) => println!(
                            "cargo:rustc-link-arg=-B{}/",
                            shim_dir.display().to_string().replace('\\', "/")
                        ),
                        Err(reason) => println!("cargo:warning={reason}"),
                    }
                }
                println!("cargo:rustc-link-arg={}", res_path.display());
                return;
            }
            Err(reason) => attempts.push(format!("{compiler}: {reason}")),
        }
    }
    println!(
        "cargo:warning=no Windows resource compiler found ({}); pdfxi-ui.exe will have no \
         icon, version resource, or manifest and is rejected by scripts/package.ps1",
        attempts.join("; ")
    );
}

/// Compile an empty `default-manifest.o` that shadows the toolchain's own.
fn empty_manifest_shim(
    compiler: &str,
    target_env: &str,
    arch: &str,
    out_dir: &Path,
) -> Result<PathBuf, String> {
    let directory = out_dir.join("default-manifest-shim");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    let rc_path = out_dir.join("shim.rc");
    // A valid COFF object is required; a *manifest-free* resource is enough.
    // windres writes no file for a comment-only script, so use a string table.
    write_text(
        &rc_path,
        "STRINGTABLE\nBEGIN\n  1 \"pdfxi-manifest-shim\"\nEND\n",
    );
    let shim = directory.join("default-manifest.o");
    compile_resources(compiler, target_env, arch, &rc_path, &shim)?;
    Ok(directory)
}
/// Parse `major.minor.patch` and clamp each part into `VERSIONINFO` range.
fn version_parts(version: &str) -> (u16, u16, u16) {
    let mut parts = version
        .split('.')
        .map(|part| part.parse::<u16>().unwrap_or(0))
        .chain(std::iter::repeat(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// Map a Rust target architecture to the manifest's processor architecture.
fn architecture_name(arch: &str) -> &'static str {
    match arch {
        "x86_64" => "amd64",
        "x86" => "x86",
        "aarch64" => "arm64",
        "arm" => "arm",
        _ => "*",
    }
}

/// Map a Rust target architecture to a GNU windres BFD target.
fn bfd_target(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("pe-x86-64"),
        "x86" => Some("pe-i386"),
        "aarch64" => Some("pe-aarch64"),
        _ => None,
    }
}

/// Ordered resource-compiler names to try.
fn compiler_candidates(target_env: &str, arch: &str) -> Vec<String> {
    if let Some(path) = env::var_os("PDFXI_RC").filter(|value| !value.is_empty()) {
        return vec![path.to_string_lossy().into_owned()];
    }
    if let Some(path) = env::var_os("RC").filter(|value| !value.is_empty()) {
        return vec![path.to_string_lossy().into_owned()];
    }
    if target_env == "msvc" {
        return vec!["rc.exe".to_owned(), "rc".to_owned()];
    }
    let prefixed = if arch == "x86_64" {
        "x86_64-w64-mingw32-windres".to_owned()
    } else {
        format!("{arch}-w64-mingw32-windres")
    };
    vec![prefixed, "windres".to_owned()]
}

/// Compile the generated resource script into a COFF resource object.
fn compile_resources(
    compiler: &str,
    target_env: &str,
    arch: &str,
    rc_path: &Path,
    res_path: &Path,
) -> Result<(), String> {
    let mut command = Command::new(compiler);
    if target_env == "msvc" {
        command.args(["/nologo", "/fo"]).arg(res_path).arg(rc_path);
    } else {
        command.args(["-O", "coff"]);
        if let Some(target) = bfd_target(arch) {
            command.arg(format!("--target={target}"));
        }
        command.args(["-o"]).arg(res_path).arg(rc_path);
    }
    let output = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        return Err(format!("exited with {}", output.status));
    }
    Err(stderr.to_owned())
}
/// Build the `windres`/`rc.exe` script: icon, manifest, and version resource.
fn resource_script(icon: &Path, manifest: &Path, version: &str, parts: (u16, u16, u16)) -> String {
    let (major, minor, patch) = parts;
    let icon = rc_path(icon);
    let manifest = rc_path(manifest);
    format!(
        "// Generated by build.rs; do not edit.\n\
1 ICON \"{icon}\"\n\
1 24 \"{manifest}\"\n\
1 VERSIONINFO\n\
FILEVERSION {major},{minor},{patch},0\n\
PRODUCTVERSION {major},{minor},{patch},0\n\
FILEFLAGSMASK 0x3fL\n\
FILEFLAGS 0x0L\n\
FILEOS 0x40004L\n\
FILETYPE 1L\n\
FILESUBTYPE 0x0L\n\
BEGIN\n\
  BLOCK \"StringFileInfo\"\n\
  BEGIN\n\
    BLOCK \"040904b0\"\n\
    BEGIN\n\
      VALUE \"CompanyName\", \"PDFXI contributors\"\n\
      VALUE \"FileDescription\", \"PDFXI viewer\"\n\
      VALUE \"FileVersion\", \"{version}\"\n\
      VALUE \"InternalName\", \"pdfxi-ui\"\n\
      VALUE \"LegalCopyright\", \"MIT OR Apache-2.0\"\n\
      VALUE \"OriginalFilename\", \"pdfxi-ui.exe\"\n\
      VALUE \"ProductName\", \"PDFXI\"\n\
      VALUE \"ProductVersion\", \"{version}\"\n\
    END\n\
  END\n\
  BLOCK \"VarFileInfo\"\n\
  BEGIN\n\
    VALUE \"Translation\", 0x409, 1200\n\
  END\n\
END\n"
    )
}

/// Rewrite a native path so a resource script sees forward slashes.
fn rc_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
/// Resolve a package-relative path against the package root.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match env::current_dir() {
        Ok(directory) => directory.join(path),
        Err(_) => path.to_path_buf(),
    }
}

/// Read a UTF-8 package file, failing the build with its path on error.
fn read_text(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => fail(&format!("cannot read {}: {error}", path.display())),
    }
}

/// Write a generated file under `OUT_DIR`.
fn write_text(path: &Path, text: &str) {
    if let Some(parent) = path.parent()
        && let Err(error) = fs::create_dir_all(parent)
    {
        fail(&format!("cannot create {}: {error}", parent.display()));
    }
    if let Err(error) = fs::write(path, text) {
        fail(&format!("cannot write {}: {error}", path.display()));
    }
}

/// Stop the build with an actionable message.
fn fail(message: &str) -> ! {
    eprintln!("error: {message}");
    std::process::exit(1)
}
