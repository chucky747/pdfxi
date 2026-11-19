//! Standard-14 fallback metrics and simple character-encoding helpers.
//!
//! The 14 standard fonts of PDF (ISO 32000-1, Annex D) are the only fonts whose
//! metrics a reader may synthesize when a font is neither embedded nor supplied
//! with a `/Widths` (or `/W`) array. This module carries a compact subset of the
//! published AFM data: vertical metrics, bounding boxes, and WinAnsi ASCII
//! advances for the core families. It is deliberately conservative — where an
//! exact table is not carried, a documented nominal advance is used rather than
//! inventing a value.

use super::FontEncoding;

/// One of the 14 standard fonts defined by PDF (ISO 32000-1, Annex D).
///
/// These are the only families for which this crate synthesizes metrics when a
/// font is not embedded and carries no explicit widths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standard14Font {
    /// Times Roman.
    TimesRoman,
    /// Times Bold.
    TimesBold,
    /// Times Italic.
    TimesItalic,
    /// Times Bold Italic.
    TimesBoldItalic,
    /// Helvetica.
    Helvetica,
    /// Helvetica Bold.
    HelveticaBold,
    /// Helvetica Oblique.
    HelveticaOblique,
    /// Helvetica Bold Oblique.
    HelveticaBoldOblique,
    /// Courier (fixed pitch).
    Courier,
    /// Courier Bold (fixed pitch).
    CourierBold,
    /// Courier Oblique (fixed pitch).
    CourierOblique,
    /// Courier Bold Oblique (fixed pitch).
    CourierBoldOblique,
    /// Symbol (symbolic encoding).
    Symbol,
    /// ZapfDingbats (symbolic encoding).
    ZapfDingbats,
}

impl Standard14Font {
    /// Resolve a `/BaseFont` name, ignoring a six-letter subset prefix such as
    /// `ABCDEF+`.
    pub fn from_base_font(name: &str) -> Option<Self> {
        let base = strip_subset_prefix(name);
        match base {
            "Times-Roman" => Some(Self::TimesRoman),
            "Times-Bold" => Some(Self::TimesBold),
            "Times-Italic" => Some(Self::TimesItalic),
            "Times-BoldItalic" => Some(Self::TimesBoldItalic),
            "Helvetica" => Some(Self::Helvetica),
            "Helvetica-Bold" => Some(Self::HelveticaBold),
            "Helvetica-Oblique" => Some(Self::HelveticaOblique),
            "Helvetica-BoldOblique" => Some(Self::HelveticaBoldOblique),
            "Courier" => Some(Self::Courier),
            "Courier-Bold" => Some(Self::CourierBold),
            "Courier-Oblique" => Some(Self::CourierOblique),
            "Courier-BoldOblique" => Some(Self::CourierBoldOblique),
            "Symbol" => Some(Self::Symbol),
            "ZapfDingbats" => Some(Self::ZapfDingbats),
            _ => None,
        }
    }

    /// Return the canonical PDF base-font name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::TimesRoman => "Times-Roman",
            Self::TimesBold => "Times-Bold",
            Self::TimesItalic => "Times-Italic",
            Self::TimesBoldItalic => "Times-BoldItalic",
            Self::Helvetica => "Helvetica",
            Self::HelveticaBold => "Helvetica-Bold",
            Self::HelveticaOblique => "Helvetica-Oblique",
            Self::HelveticaBoldOblique => "Helvetica-BoldOblique",
            Self::Courier => "Courier",
            Self::CourierBold => "Courier-Bold",
            Self::CourierOblique => "Courier-Oblique",
            Self::CourierBoldOblique => "Courier-BoldOblique",
            Self::Symbol => "Symbol",
            Self::ZapfDingbats => "ZapfDingbats",
        }
    }

    /// Return the documented ascender in glyph-space (1/1000 text) units.
    pub const fn ascent(self) -> f32 {
        match self {
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                629.0
            }
            Self::Helvetica
            | Self::HelveticaBold
            | Self::HelveticaOblique
            | Self::HelveticaBoldOblique => 718.0,
            Self::ZapfDingbats => 820.0,
            Self::TimesRoman
            | Self::TimesBold
            | Self::TimesItalic
            | Self::TimesBoldItalic
            | Self::Symbol => 683.0,
        }
    }

    /// Return the documented descender in glyph-space (1/1000 text) units.
    pub const fn descent(self) -> f32 {
        match self {
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                -157.0
            }
            Self::Helvetica
            | Self::HelveticaBold
            | Self::HelveticaOblique
            | Self::HelveticaBoldOblique
            | Self::ZapfDingbats => -207.0,
            Self::TimesRoman
            | Self::TimesBold
            | Self::TimesItalic
            | Self::TimesBoldItalic
            | Self::Symbol => -217.0,
        }
    }

    /// Return the documented `(x_min, y_min, x_max, y_max)` font bounding box.
    pub const fn bbox(self) -> (f32, f32, f32, f32) {
        match self {
            Self::TimesRoman | Self::TimesBold | Self::TimesItalic | Self::TimesBoldItalic => {
                (-168.0, -218.0, 1000.0, 898.0)
            }
            Self::Helvetica | Self::HelveticaOblique => (-166.0, -225.0, 1000.0, 931.0),
            Self::HelveticaBold | Self::HelveticaBoldOblique => (-166.0, -219.0, 1000.0, 931.0),
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                (-23.0, -250.0, 715.0, 805.0)
            }
            Self::Symbol => (-180.0, -293.0, 1090.0, 1010.0),
            Self::ZapfDingbats => (-10.0, -173.0, 1040.0, 820.0),
        }
    }

    /// Return the AFM advance for an ASCII character, or `None` when the font
    /// has no carried table for that character.
    ///
    /// Italic variants reuse their upright family's table (a documented
    /// approximation). Symbol and ZapfDingbats have symbolic encodings and use
    /// [`Self::nominal_width`] instead of an ASCII table.
    pub fn width_for_char(self, c: char) -> Option<u16> {
        match self {
            Self::Symbol | Self::ZapfDingbats => None,
            _ => {
                let code = u32::from(c);
                if (0x20..=0x7e).contains(&code) {
                    Some(self.width_table()[(code - 0x20) as usize])
                } else {
                    None
                }
            }
        }
    }

    /// Return a conservative nominal advance used when no exact AFM value is
    /// carried (Symbol, ZapfDingbats, or non-ASCII codes).
    pub const fn nominal_width(self) -> u16 {
        match self {
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                600
            }
            Self::Symbol | Self::ZapfDingbats => 500,
            _ => 500,
        }
    }

    /// Width table for codes `0x20..=0x7e` (95 entries), in glyph-space units.
    fn width_table(self) -> &'static [u16; 95] {
        match self {
            Self::Helvetica | Self::HelveticaOblique | Self::HelveticaBoldOblique => {
                &HELVETICA_WIDTHS
            }
            Self::HelveticaBold => &HELVETICA_BOLD_WIDTHS,
            Self::TimesRoman | Self::TimesItalic => &TIMES_ROMAN_WIDTHS,
            Self::TimesBold | Self::TimesBoldItalic => &TIMES_BOLD_WIDTHS,
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                &COURIER_WIDTHS
            }
            Self::Symbol | Self::ZapfDingbats => &COURIER_WIDTHS,
        }
    }
}

/// Strip a PDF subset tag (six uppercase letters plus `+`) from a base name.
fn strip_subset_prefix(name: &str) -> &str {
    let bytes = name.as_bytes();
    if bytes.len() > 7 && bytes[6] == b'+' && bytes[..6].iter().all(u8::is_ascii_uppercase) {
        &name[7..]
    } else {
        name
    }
}

/// Map a character code to a Unicode scalar through a simple encoding.
///
/// Returns `None` for composite (`Identity-H`, named CMap) encodings because
/// those codes are CIDs, not characters. WinAnsi maps the `0x80..=0x9f` range
/// through the Adobe WinAnsi table; other simple encodings pass ASCII through.
pub fn code_to_char(code: u32, encoding: &FontEncoding) -> Option<char> {
    match encoding {
        FontEncoding::IdentityH | FontEncoding::IdentityV | FontEncoding::NamedCMap(_) => None,
        FontEncoding::WinAnsiEncoding => win_ansi_char(code),
        FontEncoding::MacRomanEncoding
        | FontEncoding::PdfDocEncoding
        | FontEncoding::MacExpertEncoding
        | FontEncoding::StandardEncoding => {
            if (0x20..=0x7e).contains(&code) || (0xa0..=0xff).contains(&code) {
                char::from_u32(code)
            } else {
                None
            }
        }
    }
}

/// WinAnsiEncoding code-to-Unicode mapping (ISO 32000-1, Annex D.2).
fn win_ansi_char(code: u32) -> Option<char> {
    if (0x20..=0x7e).contains(&code) || (0xa0..=0xff).contains(&code) {
        return char::from_u32(code);
    }
    if (0x80..=0x9f).contains(&code) {
        // Undefined WinAnsi codes map to the bullet glyph.
        let cp = WIN_ANSI_HIGH[(code - 0x80) as usize];
        return char::from_u32(cp);
    }
    None
}

/// WinAnsi `0x80..=0x9f` Unicode code points; undefined slots use U+2022.
const WIN_ANSI_HIGH: [u32; 32] = [
    0x20ac, 0x2022, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x2022, 0x017d, 0x2022, 0x2022, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014,
    0x02dc, 0x2122, 0x0161, 0x203a, 0x0153, 0x2022, 0x017e, 0x0178,
];

/// Helvetica ASCII (0x20..=0x7e) widths from the AFM.
const HELVETICA_WIDTHS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, // 20..29
    389, 584, 278, 333, 278, 278, 556, 556, 556, 556, // 2a..33
    556, 556, 556, 556, 556, 556, 278, 278, 584, 584, // 34..3d
    584, 556, 1015, 667, 667, 722, 722, 667, 611, 778, // 3e..47
    722, 278, 500, 667, 556, 833, 722, 778, 667, 778, // 48..51
    722, 667, 611, 722, 667, 944, 667, 667, 611, 278, // 52..5b
    278, 278, 469, 556, 333, 556, 556, 500, 556, 556, // 5c..65
    278, 556, 556, 222, 222, 500, 222, 833, 556, 556, // 66..6f
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, // 70..79
    500, 334, 260, 334, 584, // 7a..7e
];

/// Times Roman ASCII (0x20..=0x7e) widths from the AFM.
const TIMES_ROMAN_WIDTHS: [u16; 95] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, // 20..29
    500, 564, 250, 333, 250, 278, 500, 500, 500, 500, // 2a..33
    500, 500, 500, 500, 500, 500, 278, 278, 564, 564, // 34..3d
    564, 444, 921, 722, 667, 667, 722, 611, 556, 722, // 3e..47
    722, 333, 389, 722, 611, 889, 722, 722, 556, 722, // 48..51
    667, 556, 611, 722, 722, 944, 722, 722, 611, 333, // 52..5b
    278, 333, 469, 500, 333, 444, 500, 444, 500, 444, // 5c..65
    333, 500, 500, 278, 278, 500, 278, 778, 500, 500, // 66..6f
    500, 500, 333, 389, 278, 500, 500, 722, 500, 500, // 70..79
    444, 480, 200, 480, 541, // 7a..7e
];

/// Helvetica Bold ASCII (0x20..=0x7e) widths from the AFM.
const HELVETICA_BOLD_WIDTHS: [u16; 95] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, // 20..29
    389, 584, 278, 333, 278, 278, 556, 556, 556, 556, // 2a..33
    556, 556, 556, 556, 556, 556, 333, 333, 584, 584, // 34..3d
    584, 611, 975, 722, 722, 722, 722, 667, 611, 778, // 3e..47
    722, 278, 556, 722, 611, 833, 722, 778, 667, 778, // 48..51
    722, 667, 611, 722, 667, 944, 667, 667, 611, 333, // 52..5b
    278, 333, 584, 556, 333, 556, 611, 556, 611, 556, // 5c..65
    333, 611, 611, 278, 278, 556, 278, 889, 611, 611, // 66..6f
    611, 611, 389, 556, 333, 611, 556, 778, 556, 556, // 70..79
    500, 389, 280, 389, 584, // 7a..7e
];

/// Times Bold ASCII (0x20..=0x7e) widths from the AFM.
const TIMES_BOLD_WIDTHS: [u16; 95] = [
    250, 333, 555, 500, 500, 1000, 833, 278, 333, 333, // 20..29
    500, 570, 250, 333, 250, 278, 500, 500, 500, 500, // 2a..33
    500, 500, 500, 500, 500, 500, 333, 333, 570, 570, // 34..3d
    570, 500, 930, 722, 667, 722, 722, 667, 611, 778, // 3e..47
    778, 389, 500, 778, 667, 944, 722, 778, 611, 778, // 48..51
    722, 556, 667, 722, 722, 1000, 722, 722, 667, 333, // 52..5b
    278, 333, 581, 500, 333, 500, 556, 444, 556, 444, // 5c..65
    333, 500, 556, 278, 333, 556, 278, 833, 556, 500, // 66..6f
    556, 556, 444, 389, 333, 556, 500, 722, 500, 500, // 70..79
    444, 394, 220, 394, 520, // 7a..7e
];

/// Courier is fixed-pitch: every glyph advances 600 units.
const COURIER_WIDTHS: [u16; 95] = [600; 95];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subset_prefix_is_ignored_for_standard14_lookup() {
        assert_eq!(
            Standard14Font::from_base_font("ABCDEF+Helvetica"),
            Some(Standard14Font::Helvetica)
        );
        assert_eq!(
            Standard14Font::from_base_font("Helvetica"),
            Some(Standard14Font::Helvetica)
        );
        assert_eq!(Standard14Font::from_base_font("MadeUp"), None);
        // A name that is not a six-letter subset tag is not stripped.
        assert_eq!(Standard14Font::from_base_font("ABCDE+Helvetica"), None);
    }

    #[test]
    fn standard14_vertical_metrics_and_ascii_widths() {
        let helvetica = Standard14Font::Helvetica;
        assert_eq!(helvetica.ascent(), 718.0);
        assert_eq!(helvetica.descent(), -207.0);
        assert_eq!(helvetica.bbox(), (-166.0, -225.0, 1000.0, 931.0));
        assert_eq!(helvetica.width_for_char('A'), Some(667));
        assert_eq!(helvetica.width_for_char(' '), Some(278));
        assert_eq!(helvetica.width_for_char('\u{100}'), None);
        assert_eq!(helvetica.nominal_width(), 500);
    }

    #[test]
    fn win_ansi_high_range_maps_special_and_undefined_codes() {
        assert_eq!(
            code_to_char(0x41, &FontEncoding::WinAnsiEncoding),
            Some('A')
        );
        assert_eq!(
            code_to_char(0x80, &FontEncoding::WinAnsiEncoding),
            Some('\u{20ac}')
        );
        assert_eq!(
            code_to_char(0x81, &FontEncoding::WinAnsiEncoding),
            Some('\u{2022}')
        );
        assert_eq!(code_to_char(0x7f, &FontEncoding::WinAnsiEncoding), None);
    }
}
