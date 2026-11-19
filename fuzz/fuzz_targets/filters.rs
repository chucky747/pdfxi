//! libFuzzer target: bounded stream filter decoding.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = pdfxi_testkit::fuzz::fuzz_filters(data);
});