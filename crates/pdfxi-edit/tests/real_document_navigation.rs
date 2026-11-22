//! Bookmark, link and attachment edits on real-world documents.
//!
//! The unit tests in `pdfxi-edit` use `fixtures/minimal.pdf`; these tests open
//! files produced by real applications, stage an edit through the public
//! library API, save, reopen with `pdfxi-core` and assert the result. The
//! PowerPoint fixture already carries an outline, so it proves an edit keeps the
//! existing tree; the Excel fixture is a hybrid cross-reference/object-stream
//! file, so it proves the writers work on that layout too.

use std::path::{Path, PathBuf};

use pdfxi_core::{Document, LinkTarget, Object};
use pdfxi_edit::{
    Bookmark, BookmarkPath, Destination, FitType, Link, LinkHighlight, LinkTarget as EditTarget,
    Outline, PageIndex, add_attachment, add_link, read_outline, remove_attachment, remove_link,
    write_outline,
};

/// Read a fixture from the workspace `fixtures/` directory.
fn fixture(name: &str) -> Vec<u8> {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn page_destination(page: usize) -> Destination {
    Destination::new(PageIndex::new(page), FitType::Fit)
}

#[test]
fn outline_edit_keeps_the_existing_powerpoint_bookmarks() {
    let original = fixture("realworld/powerpoint-slides.pdf");
    let mut document = Document::from_bytes(original.clone()).expect("fixture parses");
    let before = document.page_text(0).expect("page 1 text before editing");

    let read = read_outline(&document).expect("read existing outline");
    assert!(read.present);
    let titles: Vec<&str> = read
        .outline
        .roots
        .iter()
        .map(|bookmark| bookmark.title.as_str())
        .collect();
    assert_eq!(
        titles,
        vec![
            "Slide 1: Lantern Slide",
            "Slide 2: Harbor Slide",
            "Slide 3: Meadow Slide"
        ]
    );

    let mut outline = read.outline;
    let appendix = Bookmark::new("Appendix")
        .expect("bookmark")
        .with_destination(page_destination(1));
    outline
        .insert_bookmark(&BookmarkPath::root(), outline.roots.len(), appendix)
        .expect("append appendix");
    write_outline(&mut document, &outline).expect("write outline");

    let saved = document.save().expect("save outline edit");
    let reopened = Document::from_bytes(saved).expect("reopen saved file");
    assert_eq!(
        reopened.page_text(0).expect("page 1 text after editing"),
        before,
        "bookmark edits must not change page text"
    );
    let read = read_outline(&reopened).expect("read written outline");
    let titles: Vec<&str> = read
        .outline
        .roots
        .iter()
        .map(|bookmark| bookmark.title.as_str())
        .collect();
    assert_eq!(
        titles,
        vec![
            "Slide 1: Lantern Slide",
            "Slide 2: Harbor Slide",
            "Slide 3: Meadow Slide",
            "Appendix"
        ]
    );
    assert_eq!(
        read.outline.roots[3]
            .destination
            .as_ref()
            .map(|dest| dest.page.get()),
        Some(1)
    );
}

#[test]
fn link_attachment_and_bookmark_edits_survive_on_a_hybrid_file() {
    let mut document = Document::from_bytes(fixture("realworld/microsoft-excel-2019-sample.pdf"))
        .expect("fixture");
    let before = document.page_text(0).expect("page 1 text before editing");

    let link = Link::new(
        pdfxi_edit::Rect::new(72.0, 600.0, 240.0, 630.0).expect("rect"),
        EditTarget::Uri("https://example.com/report".to_owned()),
    )
    .with_highlight(LinkHighlight::Outline);
    add_link(&mut document, 0, &link).expect("stage link");

    let payload = b"real navigation payload\0with NUL";
    add_attachment(
        &mut document,
        "report.bin",
        Some("report.bin"),
        "hybrid-file evidence",
        "application/octet-stream",
        payload,
    )
    .expect("stage attachment");

    let mut outline = Outline::new();
    let chapter = Bookmark::new("Sheet one")
        .expect("bookmark")
        .with_destination(page_destination(0));
    outline
        .insert_bookmark(&BookmarkPath::root(), 0, chapter)
        .expect("insert bookmark");
    write_outline(&mut document, &outline).expect("write outline");

    let saved = document.save().expect("one save for all three edits");
    let reopened = Document::from_bytes(saved).expect("reopen saved file");
    assert_eq!(
        reopened.page_text(0).expect("page 1 text after editing"),
        before,
        "navigation edits must not change page text"
    );

    let links = reopened.links().expect("read links");
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].page, 0);
    assert_eq!(
        links[0].target,
        LinkTarget::Uri {
            uri: "https://example.com/report".to_owned()
        }
    );

    let attachments = reopened.attachments().expect("read attachments");
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].name, "report.bin");
    assert_eq!(
        attachments[0].description.as_deref(),
        Some("hybrid-file evidence")
    );
    assert_eq!(
        reopened
            .attachment_bytes("report.bin")
            .expect("attachment bytes"),
        payload
    );

    let read = read_outline(&reopened).expect("read outline");
    assert!(read.present);
    assert_eq!(read.outline.roots[0].title, "Sheet one");
    assert_eq!(
        read.outline.roots[0]
            .destination
            .as_ref()
            .map(|dest| dest.page.get()),
        Some(0)
    );
}

#[test]
fn removing_a_link_and_attachment_leaves_the_file_consistent() {
    let mut document =
        Document::from_bytes(fixture("realworld/powerpoint-slides.pdf")).expect("fixture");
    let link = Link::new(
        pdfxi_edit::Rect::new(20.0, 20.0, 120.0, 60.0).expect("rect"),
        EditTarget::Uri("https://example.com".to_owned()),
    );
    let staged = add_link(&mut document, 0, &link).expect("stage link");
    add_attachment(&mut document, "temp.txt", None, "", "", b"temporary")
        .expect("stage attachment");

    remove_link(&mut document, 0, 0).expect("remove link");
    remove_attachment(&mut document, "temp.txt").expect("remove attachment");

    let saved = document.save().expect("save after removal");
    let reopened = Document::from_bytes(saved).expect("reopen");
    assert!(reopened.links().expect("links").is_empty());
    assert!(reopened.attachments().expect("attachments").is_empty());
    // A full rewrite keeps unreachable objects, so the removed annotation may
    // still exist; what matters is that no page references it.
    let page = reopened.page(0).expect("page");
    let page_object = reopened.object(page.id).expect("page object");
    let annots = page_object
        .as_dictionary()
        .and_then(|dictionary| dictionary.get("Annots"))
        .and_then(Object::as_array)
        .map(|items| items.to_vec())
        .unwrap_or_default();
    assert!(
        !annots.contains(&Object::Reference(staged.annotation)),
        "removed link must not stay in /Annots"
    );
    // The existing outline and page text still read cleanly.
    let read = read_outline(&reopened).expect("outline");
    assert_eq!(read.outline.roots.len(), 3);
    assert!(read.issues.is_empty());
    assert!(
        reopened
            .page_text(0)
            .expect("page text")
            .contains("Lantern")
    );
    // Sanity: the object model still parses the whole catalog.
    let catalog = reopened.root().expect("root");
    assert!(matches!(
        reopened.object(catalog).expect("catalog"),
        Object::Dictionary(_)
    ));
}
