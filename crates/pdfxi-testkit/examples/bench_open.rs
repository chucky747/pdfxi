//! Deterministic wall-time harness that "opens" every committed fixture.
//!
//! Run from anywhere with:
//!
//! ```text
//! cargo run -p pdfxi-testkit --example bench_open --release
//! ```
//!
//! `pdfxi-testkit` deliberately does not depend on `pdfxi-core` (the core crate
//! already uses the testkit as a dev-dependency, so adding the reverse edge
//! would create a cycle). This harness therefore defines *open* at the
//! testkit level: read the fixture bytes from disk and run the same lightweight
//! structural checks used by the fixture integration tests (header, trailer,
//! and `startxref` targeting the `xref` keyword). It is a stable proxy for file
//! open cost, not a full PDF parse; the renderer/parser benchmarks live with
//! those crates.
//!
//! The harness has no sleeps, no randomness, and no criterion dependency. It
//! fails only if the total measured time exceeds a generous, fixed budget.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use pdfxi_testkit::fixtures;
use pdfxi_testkit::perf::{format_ms, summarize};

/// Number of times each fixture is opened.
const ITERATIONS: usize = 20;
/// Generous total budget for all fixture opens (2 seconds).
const BUDGET_MS: u128 = 2_000;

/// Perform a lightweight, panic-free structural "open" of PDF bytes.
fn open(bytes: &[u8]) -> Result<(), String> {
    if !bytes.starts_with(b"%PDF-1.7") {
        return Err("missing PDF header".to_string());
    }
    if !bytes.ends_with(b"%%EOF\n") {
        return Err("missing PDF trailer marker".to_string());
    }
    let text = String::from_utf8_lossy(bytes);
    let marker = "startxref\n";
    let at = text
        .find(marker)
        .ok_or_else(|| "missing startxref keyword".to_string())?;
    let tail = &text[at + marker.len()..];
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    let offset: usize = digits
        .parse()
        .map_err(|_| "non-numeric startxref".to_string())?;
    if bytes.get(offset..offset + 4) != Some(b"xref") {
        return Err("startxref does not point at xref".to_string());
    }
    Ok(())
}

fn read_and_open(path: &Path) -> Result<(), String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    open(&bytes)
}

fn workspace_root() -> Result<PathBuf, String> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .map(Path::to_path_buf)
        .ok_or_else(|| "could not resolve workspace root".to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixtures_dir = workspace_root()?.join("fixtures");
    let all = fixtures::all();
    let paths: Vec<PathBuf> = all
        .iter()
        .map(|fixture| fixtures_dir.join(&fixture.file_name))
        .collect();

    let mut samples: Vec<Duration> = Vec::with_capacity(all.len() * ITERATIONS);
    let started = Instant::now();
    for _ in 0..ITERATIONS {
        for path in &paths {
            let at = Instant::now();
            read_and_open(path)?;
            samples.push(at.elapsed());
        }
    }
    let total = started.elapsed();
    let timing = summarize(&samples).ok_or("no timing samples collected")?;

    println!(
        "bench_open: fixtures={} iterations={} opens={}",
        all.len(),
        ITERATIONS,
        samples.len()
    );
    println!(
        "min={}ms median={}ms max={}ms",
        format_ms(timing.min),
        format_ms(timing.median),
        format_ms(timing.max)
    );
    println!("total={}ms budget={}.000000ms", format_ms(total), BUDGET_MS);

    if total.as_millis() > BUDGET_MS {
        return Err(format!(
            "bench_open budget exceeded: {}ms > {}ms",
            total.as_millis(),
            BUDGET_MS
        )
        .into());
    }
    println!("PASS");
    Ok(())
}
