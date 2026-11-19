//! libFuzzer target: FDF/XFDF, validation patterns, and AcroForm loading.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = pdfxi_testkit::fuzz::fuzz_forms(data);
});