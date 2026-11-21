//! libFuzzer target: annotation and content-edit entry points.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = pdfxi_fuzz_entry::fuzz_edit(data);
});
