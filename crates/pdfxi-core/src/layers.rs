//! Optional-content layers (`/OCProperties`).
//!
//! Reads the catalog's optional-content configuration: every OCG with its name,
//! its position and nesting in `/Order`, and whether the default configuration
//! shows it. Visibility follows the PDF rules: `/OFF` hides, `/ON` shows, and
//! anything else follows `/BaseState` (which itself defaults to `/ON`).

use std::collections::BTreeSet;

use crate::object::{Dictionary, Object};
use crate::parser::{Structure, load_object};
use crate::{Document, Result};

/// Maximum reference-following depth while reading the layer tree.
const MAX_DEPTH: u8 = 32;

/// One optional-content layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    /// Layer name from the OCG's `/Name` entry, when present.
    pub name: Option<String>,
    /// Whether the default configuration makes the layer visible.
    pub visible: bool,
    /// Nesting depth in the default `/Order` array; 0 for top level.
    pub depth: usize,
}

/// Reader over one document's bytes and parsed structure.
struct Reader<'a> {
    data: &'a [u8],
    structure: &'a Structure,
}

impl Reader<'_> {
    fn resolve(&self, object: &Object, depth: u8) -> Result<Object> {
        if depth > MAX_DEPTH {
            return Ok(Object::Null);
        }
        match object {
            Object::Reference(id) => {
                let loaded = load_object(self.data, &self.structure.xref, *id)?;
                self.resolve(&loaded, depth + 1)
            }
            other => Ok(other.clone()),
        }
    }

    fn dictionary(&self, object: &Object, depth: u8) -> Result<Option<Dictionary>> {
        Ok(match self.resolve(object, depth)? {
            Object::Dictionary(dictionary) => Some(dictionary),
            _ => None,
        })
    }

    /// Object number of a reference, for visibility set membership.
    fn number(object: &Object) -> Option<u32> {
        match object {
            Object::Reference(id) => Some(id.number),
            _ => None,
        }
    }
}

/// Collect the object numbers listed in an `/ON` or `/OFF` array.
fn visibility_set(value: Option<&Object>) -> BTreeSet<u32> {
    let mut set = BTreeSet::new();
    if let Some(Object::Array(items)) = value {
        for item in items {
            if let Some(number) = Reader::number(item) {
                set.insert(number);
            }
        }
    }
    set
}

/// Add the OCGs listed in `/Order`, with their nesting depth.
fn collect_order(
    reader: &Reader<'_>,
    value: &Object,
    depth: usize,
    seen: &mut BTreeSet<u32>,
    ordered: &mut Vec<(u32, usize)>,
) {
    if depth > usize::from(MAX_DEPTH) {
        return;
    }
    match value {
        Object::Array(items) => {
            for item in items {
                let child_depth = if matches!(item, Object::Array(_)) {
                    depth + 1
                } else {
                    depth
                };
                collect_order(reader, item, child_depth, seen, ordered);
            }
        }
        Object::Reference(id) => {
            if seen.insert(id.number) {
                ordered.push((id.number, depth));
            }
        }
        Object::Dictionary(_) => {
            let _ = reader;
        }
        _ => {}
    }
}

impl Document {
    /// Return the optional-content layers in default display order.
    ///
    /// A document without `/OCProperties` has no layers and yields an empty
    /// vector. OCGs that do not appear in `/Order` are appended in the order of
    /// the `/OCGs` array at depth 0.
    pub fn layers(&self) -> Result<Vec<Layer>> {
        let structure = self.supported_structure()?;
        let reader = Reader {
            data: self.bytes(),
            structure,
        };
        let catalog = reader.resolve(&Object::Reference(structure.root_id), 0)?;
        let Object::Dictionary(catalog) = catalog else {
            return Ok(Vec::new());
        };
        let Some(properties) = catalog.get("OCProperties") else {
            return Ok(Vec::new());
        };
        let Some(properties) = reader.dictionary(properties, 0)? else {
            return Ok(Vec::new());
        };
        let mut names: Vec<(u32, Option<String>)> = Vec::new();
        if let Some(Object::Array(groups)) = properties.get("OCGs") {
            for group in groups {
                let Some(number) = Reader::number(group) else {
                    continue;
                };
                let name = reader
                    .dictionary(group, 0)?
                    .and_then(|dictionary| dictionary.get("Name").cloned())
                    .and_then(|name| match name {
                        Object::String(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
                        Object::Name(name) => Some(name),
                        _ => None,
                    });
                names.push((number, name));
            }
        }

        let mut off = BTreeSet::new();
        let mut on = BTreeSet::new();
        let mut base_visible = true;
        let mut ordered: Vec<(u32, usize)> = Vec::new();
        if let Some(default) = properties.get("D")
            && let Some(default) = reader.dictionary(default, 0)?
        {
            off = visibility_set(default.get("OFF"));
            on = visibility_set(default.get("ON"));
            base_visible = !matches!(
                default.get("BaseState").and_then(Object::as_name),
                Some("OFF")
            );
            if let Some(order) = default.get("Order") {
                let mut seen = BTreeSet::new();
                collect_order(&reader, order, 0, &mut seen, &mut ordered);
            }
        }
        for (number, _) in &names {
            if !ordered
                .iter()
                .any(|(ordered_number, _)| ordered_number == number)
            {
                ordered.push((*number, 0));
            }
        }

        let mut layers = Vec::with_capacity(ordered.len());
        for (number, depth) in ordered {
            let Some((_, name)) = names.iter().find(|(candidate, _)| *candidate == number) else {
                continue;
            };
            let visible = if off.contains(&number) {
                false
            } else if on.contains(&number) {
                true
            } else {
                base_visible
            };
            layers.push(Layer {
                name: name.clone(),
                visible,
                depth,
            });
        }
        Ok(layers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble a complete classic-table PDF from a list of object bodies.
    fn assemble(objects: &[String]) -> Vec<u8> {
        let mut out = String::from("%PDF-1.7\n%PDFXI\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
        }
        let xref = out.len();
        out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
        out.push_str("0000000000 65535 f \n");
        for offset in &offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    /// One page with three OCGs: A visible, B hidden, C shown by /ON, nested.
    fn layered_pdf() -> Vec<u8> {
        assemble(&[
            "<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [4 0 R 5 0 R 6 0 R] /D << /BaseState /ON /OFF [5 0 R] /ON [6 0 R] /Order [4 0 R [5 0 R 6 0 R]] >> >> >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_owned(),
            "<< /Type /OCG /Name (Background) >>".to_owned(),
            "<< /Type /OCG /Name (Watermark) >>".to_owned(),
            "<< /Type /OCG /Name (Notes) >>".to_owned(),
        ])
    }

    #[test]
    fn layers_report_names_visibility_and_nesting() {
        let document = Document::from_bytes(layered_pdf()).expect("fixture parses");
        let layers = document.layers().expect("layers read");
        assert_eq!(layers.len(), 3, "{layers:?}");
        assert_eq!(layers[0].name.as_deref(), Some("Background"));
        assert!(layers[0].visible);
        assert_eq!(layers[0].depth, 0);
        assert_eq!(layers[1].name.as_deref(), Some("Watermark"));
        assert!(!layers[1].visible, "/OFF must hide the layer");
        assert_eq!(layers[1].depth, 1);
        assert_eq!(layers[2].name.as_deref(), Some("Notes"));
        assert!(layers[2].visible, "/ON must show the layer");
        assert_eq!(layers[2].depth, 1);
    }

    #[test]
    fn documents_without_ocproperties_have_no_layers() {
        let document = Document::from_bytes(assemble(&[
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_owned(),
        ]))
        .expect("fixture parses");
        assert!(document.layers().expect("layers read").is_empty());
    }
}
