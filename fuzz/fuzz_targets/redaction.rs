//! libFuzzer target: redaction removal, full save, verification, and sanitize.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = pdfxi_fuzz_entry::fuzz_redaction(data);
});
