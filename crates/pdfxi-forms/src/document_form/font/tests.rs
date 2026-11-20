//! Unit tests for [`super`] font resolution and CMap inversion.

use std::collections::BTreeMap;

use super::{
    EncodingKind, c_map_tokens, filters_from_dictionary, hex_string, hex_value, reverse_cmap,
    sanitise_name, simple_codes_from_cid, win_ansi_char,
};
use crate::appearance::EncodedText;

#[test]
fn reverse_cmap_reads_bfchar_pairs() {
    let cmap = b"/CIDInit /ProcSet findresource begin\n\
        1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
        2 beginbfchar\n<0001> <0041>\n<0002> <20AC>\nendbfchar\n";
    let codes = reverse_cmap(cmap);
    assert_eq!(codes.get(&'A'), Some(&1));
    assert_eq!(codes.get(&'\u{20AC}'), Some(&2));
    assert_eq!(codes.len(), 2);
}

#[test]
fn reverse_cmap_expands_bfrange_incrementally() {
    let cmap = b"1 beginbfrange\n<0010> <0012> <0030>\nendbfrange\n\
        1 beginbfrange\n<0020> <0021> [<0041> <0042>]\nendbfrange\n";
    let codes = reverse_cmap(cmap);
    assert_eq!(codes.get(&'0'), Some(&0x10));
    assert_eq!(codes.get(&'1'), Some(&0x11));
    assert_eq!(codes.get(&'2'), Some(&0x12));
    assert_eq!(codes.get(&'A'), Some(&0x20));
    assert_eq!(codes.get(&'B'), Some(&0x21));
}

#[test]
fn reverse_cmap_ignores_ligatures_and_malformed_tokens() {
    let cmap = b"2 beginbfchar\n<0001> <00660069>\n<0002> <00E9>\nendbfchar\n";
    let codes = reverse_cmap(cmap);
    assert!(!codes.contains_key(&'f'));
    assert_eq!(codes.get(&'\u{E9}'), Some(&2));
}

#[test]
fn reverse_cmap_is_bounded_and_keeps_first_code() {
    let mut cmap = String::from("3 beginbfchar\n");
    for code in 1..=3_u16 {
        cmap.push_str(&format!("<{code:04X}> <0041>\n"));
    }
    cmap.push_str("endbfchar\n");
    let codes = reverse_cmap(cmap.as_bytes());
    assert_eq!(codes.get(&'A'), Some(&1));
    assert_eq!(codes.len(), 1);
}

#[test]
fn reverse_cmap_survives_truncated_input() {
    let codes = reverse_cmap(b"2 beginbfchar\n<0001> <0041>\n<0002>");
    assert_eq!(codes.get(&'A'), Some(&1));
    assert_eq!(reverse_cmap(b"").len(), 0);
}

#[test]
fn composite_encoding_emits_two_byte_hex_codes() {
    let mut codes = BTreeMap::new();
    codes.insert('A', 1_u16);
    codes.insert('B', 0x0102_u16);
    let encoding = EncodingKind::Composite(codes);
    let encoded = encoding.encode("AB").expect("both mapped");
    assert_eq!(encoded, EncodedText::Hex(vec![0x00, 0x01, 0x01, 0x02]));
    assert_eq!(encoded.render(), "<00010102>");
    assert!(encoding.encode("A?").is_none());
}

#[test]
fn simple_encoding_emits_literal_codes_and_rejects_unknown_text() {
    let mut codes = BTreeMap::new();
    codes.insert('(', 0x28_u8);
    let encoding = EncodingKind::Simple(codes);
    let encoded = encoding.encode("(").expect("mapped");
    assert_eq!(encoded, EncodedText::Literal(vec![0x28]));
    assert_eq!(encoded.render(), "(\\()");
    assert!(encoding.encode("\u{4E2D}").is_none());
}

#[test]
fn win_ansi_maps_specials_and_latin1() {
    assert_eq!(win_ansi_char(0x41), Some('A'));
    assert_eq!(win_ansi_char(0x80), Some('\u{20AC}'));
    assert_eq!(win_ansi_char(0x85), Some('\u{2026}'));
    assert_eq!(win_ansi_char(0xE9), Some('\u{E9}'));
    assert_eq!(win_ansi_char(0x81), None);
    assert_eq!(win_ansi_char(0x7F), None);
}

#[test]
fn cid_maps_drop_codes_a_simple_font_cannot_write() {
    let mut codes = BTreeMap::new();
    codes.insert('A', 0x41_u16);
    codes.insert('B', 0x0101_u16);
    let simple = simple_codes_from_cid(codes);
    assert_eq!(simple.get(&'A'), Some(&0x41));
    assert!(!simple.contains_key(&'B'));
}

#[test]
fn c_map_tokens_skip_comments_and_keep_hex_strings() {
    let tokens = c_map_tokens(b"% a comment\n<0001> [<0041>] beginbfchar");
    assert_eq!(
        tokens,
        vec![
            "<0001>".to_owned(),
            "[".to_owned(),
            "<0041>".to_owned(),
            "]".to_owned(),
            "beginbfchar".to_owned()
        ]
    );
}

#[test]
fn hex_helpers_reject_invalid_input() {
    assert_eq!(hex_value("<0041>"), Some(0x41));
    assert_eq!(hex_value("<000000041>"), None);
    assert_eq!(hex_value("0041"), None);
    assert_eq!(hex_string("<00410042>"), Some(vec![0x41, 0x42]));
    assert_eq!(hex_string("<004>"), None);
    assert_eq!(hex_string("<ZZZZ>"), None);
}

#[test]
fn filter_chain_accepts_flate_and_rejects_the_rest() {
    use pdfxi_core::{Dictionary, Object};

    let mut flate = Dictionary::new();
    flate.insert("Filter".to_owned(), Object::Name("FlateDecode".to_owned()));
    let filters = filters_from_dictionary(&flate).expect("flate is supported");
    assert_eq!(filters.len(), 1);

    let mut other = Dictionary::new();
    other.insert("Filter".to_owned(), Object::Name("LZWDecode".to_owned()));
    assert!(filters_from_dictionary(&other).is_none());

    let mut none = Dictionary::new();
    assert!(filters_from_dictionary(&none).is_none());
    none.insert(
        "Filter".to_owned(),
        Object::Array(vec![Object::Name("FlateDecode".to_owned())]),
    );
    assert_eq!(
        filters_from_dictionary(&none).map(|chain| chain.len()),
        Some(1)
    );
}

#[test]
fn resource_names_are_sanitised_predictably() {
    assert_eq!(sanitise_name("Helv"), "Helv");
    assert_eq!(sanitise_name("/F2"), "F2");
    assert_eq!(sanitise_name("F#20Name"), "F20Name");
    assert_eq!(sanitise_name("***"), "Helv");
}
