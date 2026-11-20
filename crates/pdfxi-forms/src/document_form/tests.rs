//! Unit tests for the private helpers of [`super`].

use super::{decode_pdf_text, encode_pdf_text, parse_default_appearance, replace_token};
use crate::appearance::RgbColor;

#[test]
fn default_appearance_parses_font_size_and_grey() {
    let da = parse_default_appearance("/Helv 12 Tf 0.5 g");
    assert_eq!(da.font, "Helv");
    assert_eq!(da.size, 12.0);
    assert_eq!(da.color, RgbColor::new(0.5, 0.5, 0.5));
}

#[test]
fn default_appearance_parses_rgb_and_auto_size() {
    let da = parse_default_appearance("/F2 0 Tf 0.1 0.2 0.3 rg");
    assert_eq!(da.font, "F2");
    assert_eq!(da.size, 0.0);
    assert_eq!(da.color, RgbColor::new(0.1, 0.2, 0.3));
}

#[test]
fn default_appearance_falls_back_to_helvetica_black() {
    let da = parse_default_appearance("");
    assert_eq!(da.font, "Helv");
    assert_eq!(da.size, 0.0);
    assert_eq!(da.color, RgbColor::BLACK);
}

#[test]
fn token_replacement_is_bytewise_and_repeats() {
    let replaced = replace_token(b"BT /F1 12 Tf /F1 Tj ET", b"/F1 ", b"/Helv ");
    assert_eq!(
        String::from_utf8(replaced).expect("ASCII"),
        "BT /Helv 12 Tf /Helv Tj ET"
    );
}

#[test]
fn utf16_text_round_trips_through_pdf_string_encoding() {
    let text = "caf\u{e9} \u{4e2d}\u{6587}";
    let encoded = encode_pdf_text(text);
    assert_eq!(&encoded[..2], &[0xFE, 0xFF]);
    assert_eq!(decode_pdf_text(&encoded), text);
    assert_eq!(decode_pdf_text(b"plain ascii"), "plain ascii");
}
