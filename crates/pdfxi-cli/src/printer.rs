//! Virtual PDF printer: install, inspect, remove and capture a print queue
//! that turns anything printable into a PDF.
//!
//! Windows already ships a user-mode PDF generator (`Microsoft Print To PDF`).
//! The missing piece was a queue whose output goes to a known file instead of a
//! save dialog. This module creates a local printer port that is a file path and
//! a printer that uses that port; the spooler writes the generated PDF there,
//! one job at a time, and [`watch`] ingests each finished job.
//!
//! Nothing here needs elevation: both [`install`] and [`remove`] use per-user
//! printer cmdlets, and the capture file lives under the user's local app data
//! unless `--port` names another absolute path.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// Default queue name shown in the Windows print dialog.
pub(crate) const DEFAULT_PRINTER: &str = "PDFXI PDF";

/// Driver name shipped with Windows that produces a PDF from any application.
#[cfg(windows)]
const PDF_DRIVER: &str = "Microsoft Print To PDF";

/// File name the spooler writes for a finished job.
const CAPTURE_FILE: &str = "capture.pdf";

/// Absolute path of the default capture port file.
///
/// Uses `%LOCALAPPDATA%\PDFXI\Inbox\capture.pdf`; the parent directory is
/// created by [`install`].
pub(crate) fn default_port_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("PDFXI").join("Inbox").join(CAPTURE_FILE)
}

/// Resolve a user-supplied port file, requiring an absolute path.
pub(crate) fn resolve_port(port: Option<&Path>) -> anyhow::Result<PathBuf> {
    let path = match port {
        Some(path) => path.to_path_buf(),
        None => default_port_path(),
    };
    if !path.is_absolute() {
        bail!(
            "printer port file must be an absolute path: {}",
            path.display()
        );
    }
    Ok(path)
}

/// Return true when `bytes` begins with a PDF header.
pub(crate) fn has_pdf_header(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
}

/// Return true when the capture file exists, is non-empty and is a PDF.
pub(crate) fn capture_is_ready(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() < 8 {
        return false;
    }
    let mut head = [0_u8; 5];
    match std::fs::File::open(path).and_then(|mut file| {
        use std::io::Read;
        file.read_exact(&mut head)
    }) {
        Ok(()) => has_pdf_header(&head),
        Err(_) => false,
    }
}

#[cfg(windows)]
fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(windows)]
fn powershell(script: &str) -> anyhow::Result<String> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .output()
        .context("launch powershell for printer control")?;
    if !output.status.success() {
        bail!(
            "printer control failed ({}): {}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Install the capture port and the `PDFXI PDF` queue.
#[cfg(windows)]
pub(crate) fn install(name: &str, port: Option<&Path>) -> anyhow::Result<PathBuf> {
    if name.trim().is_empty() {
        bail!("printer name must not be empty");
    }
    let port_path = resolve_port(port)?;
    if let Some(parent) = port_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create capture directory {}", parent.display()))?;
    }
    let port_text = port_path.to_string_lossy().into_owned();
    let script = format!(
        "$ErrorActionPreference = 'Stop'; \
         if (-not (Get-PrinterPort -Name {port} -ErrorAction SilentlyContinue)) {{ \
         Add-PrinterPort -Name {port} }}; \
         if (-not (Get-Printer -Name {printer} -ErrorAction SilentlyContinue)) {{ \
         Add-Printer -Name {printer} -DriverName {driver} -PortName {port} }}; \
         'INSTALLED'",
        port = ps_quote(&port_text),
        printer = ps_quote(name),
        driver = ps_quote(PDF_DRIVER),
    );
    powershell(&script)?;
    Ok(port_path)
}

/// Non-Windows builds cannot host a Windows print queue.
#[cfg(not(windows))]
pub(crate) fn install(_name: &str, _port: Option<&Path>) -> anyhow::Result<PathBuf> {
    bail!("the virtual PDF printer is only available on Windows")
}

/// Remove the queue and its capture port.
#[cfg(windows)]
pub(crate) fn remove(name: &str, port: Option<&Path>) -> anyhow::Result<()> {
    let port_path = resolve_port(port)?;
    let port_text = port_path.to_string_lossy().into_owned();
    let script = format!(
        "$ErrorActionPreference = 'SilentlyContinue'; \
         Remove-Printer -Name {printer}; \
         Remove-PrinterPort -Name {port}; \
         'REMOVED'",
        printer = ps_quote(name),
        port = ps_quote(&port_text),
    );
    powershell(&script)?;
    Ok(())
}

/// Non-Windows builds have no queue to remove.
#[cfg(not(windows))]
pub(crate) fn remove(_name: &str, _port: Option<&Path>) -> anyhow::Result<()> {
    bail!("the virtual PDF printer is only available on Windows")
}

/// Print the queue state as text or JSON.
#[cfg(windows)]
pub(crate) fn status(name: &str, port: Option<&Path>, json: bool) -> anyhow::Result<()> {
    use serde::Serialize;

    #[derive(Serialize)]
    struct Status {
        printer: String,
        installed: bool,
        driver: Option<String>,
        port: String,
        port_file_exists: bool,
        capture_ready: bool,
    }

    let port_path = resolve_port(port)?;
    let port_text = port_path.to_string_lossy().into_owned();
    let script = format!(
        "$p = Get-Printer -Name {printer} -ErrorAction SilentlyContinue; \
         if ($p) {{ $p.DriverName }} else {{ '' }}",
        printer = ps_quote(name),
    );
    let driver = powershell(&script)?.trim().to_owned();
    let installed = !driver.is_empty();
    let status = Status {
        printer: name.to_owned(),
        installed,
        driver: installed.then_some(driver),
        port: port_text,
        port_file_exists: port_path.is_file(),
        capture_ready: capture_is_ready(&port_path),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!("printer: {}", status.printer);
        println!("installed: {}", if status.installed { "yes" } else { "no" });
        match &status.driver {
            Some(driver) => println!("driver: {driver}"),
            None => println!("driver: none"),
        }
        println!("port: {}", status.port);
        println!(
            "port_file_exists: {}",
            if status.port_file_exists { "yes" } else { "no" }
        );
        println!(
            "capture_ready: {}",
            if status.capture_ready { "yes" } else { "no" }
        );
    }
    Ok(())
}

/// Non-Windows builds cannot report a Windows queue.
#[cfg(not(windows))]
pub(crate) fn status(_name: &str, _port: Option<&Path>, _json: bool) -> anyhow::Result<()> {
    bail!("the virtual PDF printer is only available on Windows")
}

/// Wait for one finished print job and copy it to `out`.
///
/// The spooler writes each job to the port file in turn, so the capture file is
/// moved out of the way before the next job starts. A job counts as finished
/// when the file exists, is non-empty, starts with `%PDF-`, and its size has not
/// changed across two checks.
pub(crate) fn watch(
    name: &str,
    port: Option<&Path>,
    out: &Path,
    timeout: std::time::Duration,
) -> anyhow::Result<PathBuf> {
    let port_path = resolve_port(port)?;
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create output directory {}", parent.display()))?;
    }

    let deadline = std::time::Instant::now() + timeout;
    let mut last_size = 0_u64;
    let mut stable_since: Option<std::time::Instant> = None;
    let mut announced = false;
    while std::time::Instant::now() < deadline {
        if let Ok(metadata) = std::fs::metadata(&port_path)
            && metadata.is_file()
        {
            if !announced {
                eprintln!("waiting for job on {name} (port {})", port_path.display());
                announced = true;
            }
            if metadata.len() > last_size {
                last_size = metadata.len();
                stable_since = Some(std::time::Instant::now());
            } else if stable_since.is_none() {
                stable_since = Some(std::time::Instant::now());
            }
            let stable = stable_since
                .map(|since| since.elapsed() >= std::time::Duration::from_millis(600))
                .unwrap_or(false);
            if stable && capture_is_ready(&port_path) {
                std::fs::copy(&port_path, out).with_context(|| {
                    format!(
                        "copy captured job {} to {}",
                        port_path.display(),
                        out.display()
                    )
                })?;
                // Free the port for the next job; a failure here must not
                // lose the captured copy, so it is reported, not fatal.
                if let Err(error) = std::fs::remove_file(&port_path) {
                    eprintln!(
                        "warning: captured job left in place ({}): {error}",
                        port_path.display()
                    );
                }
                return Ok(out.to_path_buf());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    bail!(
        "no print job arrived on {name} within {} s",
        timeout.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_is_an_absolute_capture_file() {
        let path = default_port_path();
        assert!(path.is_absolute(), "{}", path.display());
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(CAPTURE_FILE)
        );
    }

    #[test]
    fn resolve_port_rejects_relative_paths() {
        assert!(resolve_port(Some(Path::new("capture.pdf"))).is_err());
        assert!(resolve_port(Some(Path::new(r"C:\PDFXI\capture.pdf"))).is_ok());
    }

    #[test]
    fn pdf_header_detection_is_exact() {
        assert!(has_pdf_header(b"%PDF-1.7\n"));
        assert!(!has_pdf_header(b"%PDX-1.7\n"));
        assert!(!has_pdf_header(b""));
    }

    #[test]
    fn capture_readiness_requires_a_real_pdf() {
        let directory =
            std::env::temp_dir().join(format!("pdfxi-printer-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create test directory");
        let path = directory.join("capture.pdf");
        assert!(!capture_is_ready(&path));
        std::fs::write(&path, b"not a pdf").expect("write non-pdf");
        assert!(!capture_is_ready(&path));
        std::fs::write(&path, b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n").expect("write pdf");
        assert!(capture_is_ready(&path));
        std::fs::remove_dir_all(&directory).expect("remove test directory");
    }
}
