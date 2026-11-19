//! libFuzzer target: content-stream tokenizer and text decoder.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = pdfxi_testkit::fuzz::fuzz_content(data);
});