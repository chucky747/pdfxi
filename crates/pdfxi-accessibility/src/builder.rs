//! Deterministic reading-order builder from ordered page text blocks.
//!
//! [`ReadingOrderBuilder`] consumes [`TextBlock`] values (page index, text,
//! page-space box, and an optional font size) and produces a validated
//! [`A11yTree`]. The output is a document root, one page node per page that has
//! content, and paragraph or heading nodes below each page.
//!
//! # Grouping rules
//!
//! The builder is deterministic: input blocks are first sorted into a canonical
//! reading order (page ascending, then top edge descending, then left edge
//! ascending, with full tie-breaks), so scrambling the input never changes the
//! output tree or its stable ids. Within a page, the following rules apply:
//!
//! 1. **Same-line merge.** Consecutive blocks whose boxes overlap vertically
//!    are one visual line; their text is joined with a single space and their
//!    boxes are united.
//! 2. **Paragraph split by vertical gap.** Lines are accumulated into one
//!    paragraph until the vertical gap to the next line exceeds
//!    [`PARAGRAPH_GAP_MULTIPLIER`] times the page's median line height, at which
//!    point a new paragraph begins. Lines within a paragraph are joined with a
//!    single space.
//! 3. **Heading detection.** When font sizes are supplied, the body size is the
//!    most frequent supplied size (smallest value wins ties). A paragraph whose
//!    largest font size reaches [`HEADING3_RATIO`], [`HEADING2_RATIO`], or
//!    [`HEADING1_RATIO`] times the body size becomes a `Heading3`, `Heading2`, or
//!    `Heading1` node respectively. Without font sizes every block is a
//!    paragraph.

use std::collections::BTreeMap;

use crate::{A11yNode, A11yTree, BBox, NodeId, TagRole, TreeError};

/// Gap, as a multiple of the median line height, that starts a new paragraph.
pub const PARAGRAPH_GAP_MULTIPLIER: f32 = 0.8;

/// Font-size ratio at or above which a paragraph is a third-level heading.
pub const HEADING3_RATIO: f32 = 1.15;

/// Font-size ratio at or above which a paragraph is a second-level heading.
pub const HEADING2_RATIO: f32 = 1.35;

/// Font-size ratio at or above which a paragraph is a first-level heading.
pub const HEADING1_RATIO: f32 = 1.6;

/// A single ordered block of page text.
#[derive(Clone, Debug, PartialEq)]
pub struct TextBlock {
    /// Zero-based page index containing the block.
    pub page: usize,
    /// Text content of the block.
    pub text: String,
    /// Box of the block in page space.
    pub bbox: BBox,
    /// Font size of the block, when known.
    pub font_size: Option<f32>,
}

impl TextBlock {
    /// Creates a text block, validating its box and optional font size.
    pub fn new(
        page: usize,
        text: impl Into<String>,
        bbox: BBox,
        font_size: Option<f32>,
    ) -> Result<Self, TreeError> {
        bbox.check()?;
        if let Some(size) = font_size {
            check_font_size(size)?;
        }
        Ok(Self {
            page,
            text: text.into(),
            bbox,
            font_size,
        })
    }
}

fn check_font_size(value: f32) -> Result<(), TreeError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(TreeError::NonFiniteFontSize { value })
    }
}

fn max_opt(a: Option<f32>, b: Option<f32>) -> Option<f32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

#[derive(Clone, Debug)]
struct Line {
    text: String,
    bbox: BBox,
    font_size: Option<f32>,
}

fn sort_blocks(blocks: &mut [TextBlock]) {
    blocks.sort_by(|a, b| {
        a.page
            .cmp(&b.page)
            .then_with(|| b.bbox.y1().total_cmp(&a.bbox.y1()))
            .then_with(|| a.bbox.x0().total_cmp(&b.bbox.x0()))
            .then_with(|| a.bbox.x1().total_cmp(&b.bbox.x1()))
            .then_with(|| b.bbox.y0().total_cmp(&a.bbox.y0()))
            .then_with(|| a.text.cmp(&b.text))
            .then_with(|| {
                a.font_size
                    .map(f32::to_bits)
                    .cmp(&b.font_size.map(f32::to_bits))
            })
    });
}

fn body_font_size(blocks: &[TextBlock]) -> Option<f32> {
    let mut sizes: Vec<f32> = blocks.iter().filter_map(|block| block.font_size).collect();
    if sizes.is_empty() {
        return None;
    }
    sizes.sort_by(|a, b| a.total_cmp(b));
    let mut best: Option<(f32, usize)> = None;
    let mut i = 0usize;
    while i < sizes.len() {
        let value = sizes[i];
        let mut j = i + 1;
        while j < sizes.len() && sizes[j] == value {
            j += 1;
        }
        let count = j - i;
        best = match best {
            None => Some((value, count)),
            Some((best_value, best_count)) => {
                if count > best_count || (count == best_count && value < best_value) {
                    Some((value, count))
                } else {
                    Some((best_value, best_count))
                }
            }
        };
        i = j;
    }
    best.map(|(value, _)| value)
}

fn heading_role(font_size: Option<f32>, body: Option<f32>) -> TagRole {
    match (font_size, body) {
        (Some(size), Some(body_size)) => {
            let ratio = size / body_size;
            if ratio >= HEADING1_RATIO {
                TagRole::Heading1
            } else if ratio >= HEADING2_RATIO {
                TagRole::Heading2
            } else if ratio >= HEADING3_RATIO {
                TagRole::Heading3
            } else {
                TagRole::Paragraph
            }
        }
        _ => TagRole::Paragraph,
    }
}

fn group_lines(blocks: &[TextBlock]) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    for block in blocks {
        if let Some(last) = lines.last_mut()
            && last.bbox.vertically_overlaps(&block.bbox)
        {
            last.text.push(' ');
            last.text.push_str(&block.text);
            last.bbox = last.bbox.union(block.bbox);
            last.font_size = max_opt(last.font_size, block.font_size);
            continue;
        }
        lines.push(Line {
            text: block.text.clone(),
            bbox: block.bbox,
            font_size: block.font_size,
        });
    }
    lines
}

fn median_line_height(lines: &[Line]) -> f32 {
    let mut heights: Vec<f32> = lines.iter().map(|line| line.bbox.height()).collect();
    heights.sort_by(|a, b| a.total_cmp(b));
    let mid = heights.len() / 2;
    if heights.len() % 2 == 1 {
        heights[mid]
    } else {
        (heights[mid - 1] + heights[mid]) * 0.5
    }
}

fn group_paragraphs(lines: Vec<Line>, reference_height: f32) -> Vec<Vec<Line>> {
    let mut paragraphs: Vec<Vec<Line>> = Vec::new();
    let threshold = PARAGRAPH_GAP_MULTIPLIER * reference_height;
    for line in lines {
        let should_start = match paragraphs.last() {
            None => true,
            Some(current) => match current.last() {
                None => true,
                Some(previous) => (previous.bbox.y0() - line.bbox.y1()) > threshold,
            },
        };
        if should_start {
            paragraphs.push(vec![line]);
        } else if let Some(current) = paragraphs.last_mut() {
            current.push(line);
        }
    }
    paragraphs
}

fn union_of(boxes: impl Iterator<Item = BBox>) -> Option<BBox> {
    let mut result: Option<BBox> = None;
    for bbox in boxes {
        result = Some(match result {
            Some(current) => current.union(bbox),
            None => bbox,
        });
    }
    result
}

/// Builds deterministic accessibility trees from ordered page text blocks.
#[derive(Clone, Copy, Debug, Default)]
pub struct ReadingOrderBuilder;

impl ReadingOrderBuilder {
    /// Creates a builder with the default grouping rules.
    pub fn new() -> Self {
        Self
    }

    /// Builds and validates an accessibility tree from the supplied blocks.
    ///
    /// Returns an empty tree when `blocks` is empty. Blocks that already carry
    /// invalid boxes or font sizes are rejected with a structured [`TreeError`].
    pub fn build(&self, mut blocks: Vec<TextBlock>) -> Result<A11yTree, TreeError> {
        for block in &blocks {
            block.bbox.check()?;
            if let Some(size) = block.font_size {
                check_font_size(size)?;
            }
        }
        if blocks.is_empty() {
            return Ok(A11yTree::empty());
        }

        sort_blocks(&mut blocks);
        let body = body_font_size(&blocks);
        let document_box = match union_of(blocks.iter().map(|block| block.bbox)) {
            Some(value) => value,
            None => return Ok(A11yTree::empty()),
        };

        let mut by_page: BTreeMap<usize, Vec<TextBlock>> = BTreeMap::new();
        for block in blocks {
            by_page.entry(block.page).or_default().push(block);
        }

        let mut nodes: Vec<A11yNode> = Vec::new();
        let mut next_id = 1u64;
        let document_id = NodeId::new(next_id);
        next_id += 1;
        let mut document_children: Vec<NodeId> = Vec::new();

        for (page, page_blocks) in by_page {
            let page_id = NodeId::new(next_id);
            next_id += 1;
            let page_box = match union_of(page_blocks.iter().map(|block| block.bbox)) {
                Some(value) => value,
                None => continue,
            };
            let lines = group_lines(&page_blocks);
            let reference_height = median_line_height(&lines);
            let paragraphs = group_paragraphs(lines, reference_height);

            let mut page_children: Vec<NodeId> = Vec::new();
            for paragraph in paragraphs {
                let first = match paragraph.first() {
                    Some(value) => value,
                    None => continue,
                };
                let mut paragraph_box = first.bbox;
                let mut font_size = first.font_size;
                let mut text = first.text.clone();
                for line in paragraph.iter().skip(1) {
                    paragraph_box = paragraph_box.union(line.bbox);
                    font_size = max_opt(font_size, line.font_size);
                    text.push(' ');
                    text.push_str(&line.text);
                }
                let role = heading_role(font_size, body);
                let id = NodeId::new(next_id);
                next_id += 1;
                nodes.push(A11yNode::new(
                    id,
                    role,
                    page,
                    paragraph_box,
                    text,
                    Vec::new(),
                ));
                page_children.push(id);
            }

            nodes.push(A11yNode::new(
                page_id,
                TagRole::Page,
                page,
                page_box,
                String::new(),
                page_children,
            ));
            document_children.push(page_id);
        }

        nodes.push(A11yNode::new(
            document_id,
            TagRole::Document,
            0,
            document_box,
            String::new(),
            document_children,
        ));

        A11yTree::new(nodes, vec![document_id])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bbox(x0: f32, y0: f32, x1: f32, y1: f32) -> BBox {
        match BBox::new(x0, y0, x1, y1) {
            Ok(value) => value,
            Err(_) => panic!("test box must be valid"),
        }
    }

    fn block(page: usize, text: &str, y0: f32, y1: f32, size: Option<f32>) -> TextBlock {
        match TextBlock::new(page, text, bbox(10.0, y0, 100.0, y1), size) {
            Ok(value) => value,
            Err(_) => panic!("test block must be valid"),
        }
    }

    #[test]
    fn builder_produces_ordered_document_page_paragraph_tree() {
        let blocks = vec![
            block(0, "Second", 100.0, 110.0, None),
            block(0, "First", 120.0, 130.0, None),
        ];
        let tree = ReadingOrderBuilder::new()
            .build(blocks)
            .expect("valid tree");
        assert!(tree.validate().is_ok());
        assert_eq!(tree.len(), 4);

        let document = tree.node(NodeId::new(1)).expect("document");
        assert_eq!(document.role, TagRole::Document);
        assert_eq!(document.children, vec![NodeId::new(2)]);

        let page = tree.node(NodeId::new(2)).expect("page");
        assert_eq!(page.role, TagRole::Page);
        assert_eq!(page.page, 0);
        assert_eq!(page.children.len(), 2);

        // Descending y means "First" (higher) precedes "Second".
        let first = tree.node(page.children[0]).expect("first paragraph");
        let second = tree.node(page.children[1]).expect("second paragraph");
        assert_eq!(first.role, TagRole::Paragraph);
        assert_eq!(first.text, "First");
        assert_eq!(second.text, "Second");
        assert_eq!(
            tree.traversal(),
            vec![
                NodeId::new(1),
                NodeId::new(2),
                NodeId::new(3),
                NodeId::new(4)
            ]
        );
    }

    #[test]
    fn same_line_blocks_merge_into_one_node() {
        let a = match TextBlock::new(0, "Hello", bbox(10.0, 100.0, 50.0, 110.0), None) {
            Ok(value) => value,
            Err(_) => panic!("test block must be valid"),
        };
        let b = match TextBlock::new(0, "world", bbox(55.0, 100.0, 95.0, 110.0), None) {
            Ok(value) => value,
            Err(_) => panic!("test block must be valid"),
        };
        let tree = ReadingOrderBuilder::new()
            .build(vec![a, b])
            .expect("valid tree");
        let content: Vec<_> = tree
            .nodes()
            .iter()
            .filter(|node| node.role == TagRole::Paragraph)
            .collect();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0].text, "Hello world");
        assert_eq!((content[0].bbox.x0(), content[0].bbox.x1()), (10.0, 95.0));
    }

    #[test]
    fn vertical_gap_splits_paragraphs_but_small_gap_merges() {
        // Two lines separated by 2 points (< 0.8 * 10 = 8): one paragraph.
        let close = vec![
            block(0, "Upper", 100.0, 110.0, None),
            block(0, "Lower", 88.0, 98.0, None),
        ];
        let close_tree = ReadingOrderBuilder::new().build(close).expect("valid tree");
        let close_paragraphs: Vec<_> = close_tree
            .nodes()
            .iter()
            .filter(|node| node.role == TagRole::Paragraph)
            .collect();
        assert_eq!(close_paragraphs.len(), 1);
        assert_eq!(close_paragraphs[0].text, "Upper Lower");

        // A 10-point gap (> 8): two paragraphs.
        let apart = vec![
            block(0, "First paragraph", 100.0, 110.0, None),
            block(0, "Second paragraph", 80.0, 90.0, None),
        ];
        let apart_tree = ReadingOrderBuilder::new().build(apart).expect("valid tree");
        let apart_texts: Vec<_> = apart_tree
            .nodes()
            .iter()
            .filter(|node| node.role == TagRole::Paragraph)
            .map(|node| node.text.as_str())
            .collect();
        assert_eq!(apart_texts, vec!["First paragraph", "Second paragraph"]);
    }

    #[test]
    fn font_size_assigns_heading_roles() {
        // Body size 12 (three occurrences); headings 20, 17, 14.
        let blocks = vec![
            block(0, "Body one", 100.0, 110.0, Some(12.0)),
            block(0, "Body two", 88.0, 98.0, Some(12.0)),
            block(0, "Body three", 76.0, 86.0, Some(12.0)),
            block(0, "Big", 300.0, 315.0, Some(20.0)),
            block(0, "Medium", 250.0, 264.0, Some(17.0)),
            block(0, "Small heading", 200.0, 213.0, Some(14.0)),
        ];
        let tree = ReadingOrderBuilder::new()
            .build(blocks)
            .expect("valid tree");
        let roles_by_text: Vec<_> = tree
            .nodes()
            .iter()
            .filter(|node| !node.text.is_empty())
            .map(|node| (node.text.as_str(), node.role))
            .collect();
        assert!(roles_by_text.contains(&("Big", TagRole::Heading1)));
        assert!(roles_by_text.contains(&("Medium", TagRole::Heading2)));
        assert!(roles_by_text.contains(&("Small heading", TagRole::Heading3)));
    }

    #[test]
    fn builder_is_deterministic_for_scrambled_input() {
        let make = |scramble: bool| {
            let mut blocks = vec![
                block(1, "Page two line", 120.0, 132.0, None),
                block(0, "Page one heading", 200.0, 216.0, Some(18.0)),
                block(0, "Page one body", 100.0, 112.0, Some(11.0)),
                block(1, "Another", 100.0, 112.0, None),
            ];
            if scramble {
                blocks.reverse();
            }
            ReadingOrderBuilder::new()
                .build(blocks)
                .expect("valid tree")
        };
        assert_eq!(make(false), make(true));
    }

    #[test]
    fn empty_input_produces_empty_tree() {
        let tree = ReadingOrderBuilder::new()
            .build(Vec::new())
            .expect("empty build");
        assert!(tree.is_empty());
        assert!(tree.traversal().is_empty());
    }

    #[test]
    fn invalid_font_size_is_rejected() {
        // Struct literal bypasses TextBlock::new so the builder's own
        // validation is exercised.
        let bad = TextBlock {
            page: 0,
            text: "Bad".to_string(),
            bbox: bbox(0.0, 0.0, 10.0, 10.0),
            font_size: Some(f32::NAN),
        };
        let tree = ReadingOrderBuilder::new().build(vec![bad]);
        assert!(matches!(
            tree.err(),
            Some(TreeError::NonFiniteFontSize { .. })
        ));
    }
}
