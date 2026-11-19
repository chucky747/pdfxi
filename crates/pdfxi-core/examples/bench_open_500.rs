//! M1 open benchmark over a generated 500-page PDF.
//!
//! Run from anywhere (the 500-page PDF is generated into `target/perf/` and is
//! never committed):
//!
//! ```text
//! cargo run -p pdfxi-core --example bench_open_500 --release
//! ```
//!
//! The example writes the fixture with `pdfxi-testkit`, then uses the real
//! `pdfxi_core::Document::from_bytes` parser. It prints a stable,
//! machine-readable set of lines:
//!
//! ```text
//! iterations=7
//! open_median_ms=<seconds.microseconds>
//! first_page_ms=<seconds.microseconds>
//! all_pages_ms=<seconds.microseconds>
//! pages=500
//! bytes=<fixture size>
//! PASS | FAIL
//! ```
//!
//! M1 budgets are strict: median open below 1500 ms and first-page access plus
//! text extraction below 300 ms. The iteration count is fixed, there are no
//! sleeps, and a violation prints `FAIL` and exits non-zero. A single untimed
//! warm-up is performed first so the OS page cache does not bias the samples.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use pdfxi_core::Document;
use pdfxi_testkit::bench::{
    BENCH_PAGE_COUNT, default_500_page_pdf_path, page_text_marker, write_500_page_pdf,
};
use pdfxi_testkit::perf::{format_ms, summarize};

/// Fixed, odd sample count so the median is a measured duration.
const ITERATIONS: usize = 7;
/// M1 budget for a full structural open/parse of 500 pages.
const OPEN_BUDGET: Duration = Duration::from_millis(1_500);
/// M1 budget for reaching page zero and extracting its text.
const FIRST_PAGE_BUDGET: Duration = Duration::from_millis(300);

/// Measured medians and fixture facts used to build the report.
struct Report {
    open_median: Duration,
    first_page: Duration,
    all_pages: Duration,
    pages: usize,
    bytes: usize,
}

/// Verify that extracted text for a page matches its deterministic marker.
fn check_marker(index: usize, text: &str) -> Result<(), String> {
    let expected = page_text_marker(index);
    if text.trim() != expected {
        return Err(format!(
            "page {index} text mismatch: expected {expected:?}, got {:?}",
            text.trim()
        ));
    }
    Ok(())
}

/// Run the benchmark, printing metric lines before returning.
fn run() -> Result<Report, String> {
    let path: PathBuf = default_500_page_pdf_path();
    let bytes =
        write_500_page_pdf(&path).map_err(|error| format!("write {}: {error}", path.display()))?;
    println!("fixture={}", path.display());

    // Untimed warm-up: settle the allocator and OS page cache.
    let warmup = Document::from_bytes(bytes.clone()).map_err(|error| format!("open: {error}"))?;
    let warmup_pages = warmup
        .page_count()
        .map_err(|error| format!("page_count: {error}"))?;
    if warmup_pages != BENCH_PAGE_COUNT {
        return Err(format!(
            "expected {BENCH_PAGE_COUNT} pages, got {warmup_pages}"
        ));
    }

    let mut open_samples: Vec<Duration> = Vec::with_capacity(ITERATIONS);
    let mut first_samples: Vec<Duration> = Vec::with_capacity(ITERATIONS);
    let mut all_samples: Vec<Duration> = Vec::with_capacity(ITERATIONS);

    for _ in 0..ITERATIONS {
        // Clone outside the timed region: from_bytes takes ownership of the bytes.
        let owned = bytes.clone();

        let started = Instant::now();
        let document = Document::from_bytes(owned).map_err(|error| format!("open: {error}"))?;
        let pages = document
            .page_count()
            .map_err(|error| format!("page_count: {error}"))?;
        open_samples.push(started.elapsed());
        if pages != BENCH_PAGE_COUNT {
            return Err(format!("expected {BENCH_PAGE_COUNT} pages, got {pages}"));
        }

        let started = Instant::now();
        let first_page = document
            .page(0)
            .map_err(|error| format!("page(0): {error}"))?;
        let first_text = document
            .page_text(0)
            .map_err(|error| format!("page_text(0): {error}"))?;
        first_samples.push(started.elapsed());
        if first_page.index != 0 {
            return Err(format!("unexpected first page index {}", first_page.index));
        }
        check_marker(0, &first_text)?;

        let started = Instant::now();
        let mut texts: Vec<String> = Vec::with_capacity(pages);
        for index in 0..pages {
            texts.push(
                document
                    .page_text(index)
                    .map_err(|error| format!("page_text({index}): {error}"))?,
            );
        }
        all_samples.push(started.elapsed());
        for (index, text) in texts.iter().enumerate() {
            check_marker(index, text)?;
        }
    }

    let median_of = |samples: &[Duration]| -> Result<Duration, String> {
        summarize(samples)
            .map(|timing| timing.median)
            .ok_or_else(|| "no timing samples collected".to_string())
    };
    let report = Report {
        open_median: median_of(&open_samples)?,
        first_page: median_of(&first_samples)?,
        all_pages: median_of(&all_samples)?,
        pages: BENCH_PAGE_COUNT,
        bytes: bytes.len(),
    };

    println!("iterations={ITERATIONS}");
    println!("open_median_ms={}", format_ms(report.open_median));
    println!("first_page_ms={}", format_ms(report.first_page));
    println!("all_pages_ms={}", format_ms(report.all_pages));
    println!("pages={}", report.pages);
    println!("bytes={}", report.bytes);

    let open_ok = report.open_median < OPEN_BUDGET;
    let first_ok = report.first_page < FIRST_PAGE_BUDGET;
    if open_ok && first_ok {
        println!("PASS");
        Ok(report)
    } else {
        Err(format!(
            "M1 budget violation: open {} vs < {} ms; first page {} vs < {} ms",
            format_ms(report.open_median),
            format_ms(OPEN_BUDGET),
            format_ms(report.first_page),
            format_ms(FIRST_PAGE_BUDGET)
        ))
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            println!("FAIL");
            eprintln!("bench_open_500: {error}");
            ExitCode::FAILURE
        }
    }
}
