//! libFuzzer target: core parser, classic/xref streams, and object streams.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = pdfxi_testkit::fuzz::fuzz_parser(data);
});
