//! libFuzzer target: AES/RC4/hashes, DER, X.509, and CMS SignedData.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = pdfxi_testkit::fuzz::fuzz_crypto(data);
});