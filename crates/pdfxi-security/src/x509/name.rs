//! X.501 `Name` / RDN parsing with deterministic RFC 4514 display.
//!
//! A [`Name`] is an `RDNSequence` (`SEQUENCE OF RelativeDistinguishedName`).
//! Each RDN is a `SET OF AttributeTypeAndValue`. The parser accepts the common
//! X.520 directory-string encodings and renders values in a stable, escaped
//! RFC 4514 form. Attributes inside an RDN are sorted by OID and value before
//! display so the textual form is deterministic even if a producer emitted the
//! `SET OF` elements in a different order.

use std::fmt;

use crate::der::{Tag, Tlv};
use crate::error::{Result, SecurityError};
use crate::x509::oid_string;

/// OID `2.5.4.3` — `commonName`.
const OID_COMMON_NAME: [u32; 4] = [2, 5, 4, 3];
/// OID `2.5.4.5` — `serialNumber`.
const OID_SERIAL_NUMBER: [u32; 4] = [2, 5, 4, 5];
/// OID `2.5.4.6` — `countryName`.
const OID_COUNTRY: [u32; 4] = [2, 5, 4, 6];
/// OID `2.5.4.7` — `localityName`.
const OID_LOCALITY: [u32; 4] = [2, 5, 4, 7];
/// OID `2.5.4.8` — `stateOrProvinceName`.
const OID_STATE: [u32; 4] = [2, 5, 4, 8];
/// OID `2.5.4.9` — `streetAddress`.
const OID_STREET: [u32; 4] = [2, 5, 4, 9];
/// OID `2.5.4.10` — `organizationName`.
const OID_ORGANIZATION: [u32; 4] = [2, 5, 4, 10];
/// OID `2.5.4.11` — `organizationalUnitName`.
const OID_ORGANIZATIONAL_UNIT: [u32; 4] = [2, 5, 4, 11];
/// OID `2.5.4.97` — `organizationIdentifier`.
const OID_ORGANIZATION_IDENTIFIER: [u32; 4] = [2, 5, 4, 97];
/// OID `1.2.840.113549.1.9.1` — `emailAddress`.
const OID_EMAIL_ADDRESS: [u32; 7] = [1, 2, 840, 113_549, 1, 9, 1];
/// OID `0.9.2342.19200300.100.1.25` — `domainComponent`.
const OID_DOMAIN_COMPONENT: [u32; 7] = [0, 9, 2342, 19200300, 100, 1, 25];

fn short_name(oid: &[u32]) -> Option<&'static str> {
    match oid {
        o if o == OID_COMMON_NAME => Some("CN"),
        o if o == OID_SERIAL_NUMBER => Some("serialNumber"),
        o if o == OID_COUNTRY => Some("C"),
        o if o == OID_LOCALITY => Some("L"),
        o if o == OID_STATE => Some("ST"),
        o if o == OID_STREET => Some("STREET"),
        o if o == OID_ORGANIZATION => Some("O"),
        o if o == OID_ORGANIZATIONAL_UNIT => Some("OU"),
        o if o == OID_ORGANIZATION_IDENTIFIER => Some("organizationIdentifier"),
        o if o == OID_EMAIL_ADDRESS => Some("emailAddress"),
        o if o == OID_DOMAIN_COMPONENT => Some("DC"),
        _ => None,
    }
}

fn decode_bmp(content: &[u8]) -> Result<String> {
    if !content.len().is_multiple_of(2) {
        return Err(SecurityError::DerInvalid);
    }
    let units: Vec<u16> = content
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes(*pair))
        .collect();
    Ok(char::decode_utf16(units)
        .map(|decoded| decoded.unwrap_or('\u{fffd}'))
        .collect())
}

fn decode_value(tag: Tag, content: &[u8]) -> Result<String> {
    if tag.is_constructed() {
        return Err(SecurityError::DerInvalid);
    }
    if tag == Tag::UTF8_STRING {
        String::from_utf8(content.to_vec()).map_err(|_| SecurityError::DerInvalid)
    } else if tag == Tag::BMP_STRING {
        decode_bmp(content)
    } else {
        // PrintableString, IA5String, NumericString, TeletexString and any other
        // primitive DirectoryString encoding are mapped byte-for-byte to Unicode
        // scalar values. This is lossless for the ASCII/Latin-1 range used by
        // X.520 names and keeps display deterministic for unrecognised tags.
        Ok(content.iter().copied().map(char::from).collect())
    }
}

fn escape_value(value: &str) -> String {
    let mut chars = value.chars().peekable();
    let mut out = String::new();
    if let Some(first) = chars.peek().copied()
        && (first == '#' || first == ' ')
    {
        out.push('\\');
        out.push(first);
        chars.next();
    }
    let rest: Vec<char> = chars.collect();
    for (index, ch) in rest.iter().copied().enumerate() {
        let is_last = index + 1 == rest.len();
        match ch {
            '"' | '+' | ',' | ';' | '<' | '>' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            ' ' if is_last => {
                out.push('\\');
                out.push(' ');
            }
            ch if ch.is_control() => {
                // RFC 4514 section 2.4: control characters are escaped as the
                // hexadecimal form of each UTF-8 octet, not the scalar value.
                let mut buffer = [0u8; 4];
                for byte in ch.encode_utf8(&mut buffer).as_bytes() {
                    out.push('\\');
                    out.push_str(&format!("{byte:02X}"));
                }
            }
            ch => out.push(ch),
        }
    }
    out
}

/// One `AttributeTypeAndValue` from an X.501 relative distinguished name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeTypeAndValue {
    oid: Vec<u32>,
    value_text: String,
    value_tag: u8,
    encoded: Vec<u8>,
}

impl AttributeTypeAndValue {
    fn parse(tlv: &Tlv<'_>) -> Result<Self> {
        let mut sequence = tlv.as_sequence()?;
        let oid = sequence.read_tlv()?.as_oid()?;
        let value_tlv = sequence.read_tlv()?;
        sequence.expect_end()?;
        let value_text = decode_value(value_tlv.tag(), value_tlv.content())?;
        Ok(Self {
            oid,
            value_text,
            value_tag: value_tlv.tag().byte(),
            encoded: tlv.encoded().to_vec(),
        })
    }

    /// Return the attribute type OID arcs.
    pub fn oid(&self) -> &[u32] {
        &self.oid
    }

    /// Return the decoded directory-string value.
    pub fn value(&self) -> &str {
        &self.value_text
    }

    /// Return the identifier octet of the encoded value.
    pub const fn value_tag(&self) -> u8 {
        self.value_tag
    }

    /// Return the complete encoded `AttributeTypeAndValue`.
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    /// Return the short attribute name, if this crate knows one.
    pub fn short_name(&self) -> Option<&'static str> {
        short_name(&self.oid)
    }

    /// Render this attribute as `key=value` with RFC 4514 escaping.
    pub fn display_rfc4514(&self) -> String {
        let key = match short_name(&self.oid) {
            Some(name) => name.to_owned(),
            None => format!("OID.{}", oid_string(&self.oid)),
        };
        format!("{}={}", key, escape_value(&self.value_text))
    }
}

/// A `RelativeDistinguishedName` — a non-empty `SET OF` attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelativeDistinguishedName {
    attributes: Vec<AttributeTypeAndValue>,
}

impl RelativeDistinguishedName {
    fn parse(tlv: &Tlv<'_>) -> Result<Self> {
        let mut set = tlv.as_set()?;
        let mut attributes = Vec::new();
        while !set.is_empty() {
            attributes.push(AttributeTypeAndValue::parse(&set.read_tlv()?)?);
        }
        if attributes.is_empty() {
            return Err(SecurityError::DerInvalid);
        }
        // DER requires SET OF elements to be sorted by encoding, but sorting
        // here too makes display deterministic even for slightly non-canonical
        // producers, without changing the parsed values.
        attributes.sort_by(|left, right| {
            oid_string(&left.oid)
                .cmp(&oid_string(&right.oid))
                .then_with(|| left.value_text.cmp(&right.value_text))
        });
        Ok(Self { attributes })
    }

    /// Return the attributes in deterministic display order.
    pub fn attributes(&self) -> &[AttributeTypeAndValue] {
        &self.attributes
    }

    /// Render the RDN by joining its attributes with `+`.
    pub fn display_rfc4514(&self) -> String {
        self.attributes
            .iter()
            .map(AttributeTypeAndValue::display_rfc4514)
            .collect::<Vec<_>>()
            .join("+")
    }
}

/// An X.501 `Name`, represented as its `RDNSequence`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    rdns: Vec<RelativeDistinguishedName>,
}

impl Name {
    /// Parse a `Name` from a DER `SEQUENCE`.
    ///
    /// Returns [`SecurityError::DerUnexpectedTag`] if `tlv` is not a constructed
    /// `SEQUENCE`, and [`SecurityError::DerInvalid`] for an empty RDN.
    pub fn parse(tlv: &Tlv<'_>) -> Result<Self> {
        let mut sequence = tlv.as_sequence()?;
        let mut rdns = Vec::new();
        while !sequence.is_empty() {
            rdns.push(RelativeDistinguishedName::parse(&sequence.read_tlv()?)?);
        }
        Ok(Self { rdns })
    }

    /// Build a name from already-parsed RDNs.
    pub fn new(rdns: Vec<RelativeDistinguishedName>) -> Self {
        Self { rdns }
    }

    /// Return the RDN sequence in encoded (most-general first) order.
    pub fn rdns(&self) -> &[RelativeDistinguishedName] {
        &self.rdns
    }

    /// Return `true` when the name contains no RDNs.
    pub fn is_empty(&self) -> bool {
        self.rdns.is_empty()
    }

    /// Render the name as an RFC 4514 distinguished string.
    ///
    /// RDNs are emitted in reverse (most-specific first) order, matching RFC
    /// 4514; attributes within an RDN use the deterministic order established
    /// during parsing. This is the stable key used by `DigitalIdStore` lookups.
    pub fn display_rfc4514(&self) -> String {
        self.rdns
            .iter()
            .rev()
            .map(RelativeDistinguishedName::display_rfc4514)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Return the value of the first attribute with the given OID, if any.
    pub fn first_attribute_value(&self, oid: &[u32]) -> Option<&str> {
        self.rdns
            .iter()
            .flat_map(|rdn| rdn.attributes())
            .find_map(|attribute| {
                if attribute.oid() == oid {
                    Some(attribute.value())
                } else {
                    None
                }
            })
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display_rfc4514())
    }
}
